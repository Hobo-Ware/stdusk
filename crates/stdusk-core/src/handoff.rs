//! Live shell handover: move LIVE pty master fds from a running app to a freshly-launched one over
//! a unix socket, so updating the app restarts the UI without killing the shells. The shell keeps
//! running because its master fd is never closed - it just changes owner; the child reparents to
//! launchd, which is fine (we track it by process group, not parentage).
//!
//! This module is the app-independent half: the transport, the per-pane metadata, and the
//! "what now if it failed" decision. What stays with the embedding app is everything about ITS
//! windows: choosing the socket path, launching the successor, snapshotting its layout, rebuilding
//! its tabs from the adopted panes, and unlinking its own instance socket.
//!
//! # Wire format
//!
//! One message per pane: a `u32` big-endian length, that many bytes of TOML metadata
//! ([`PaneMeta`]), and exactly ONE fd in the `SCM_RIGHTS` control message. One fd per message keeps
//! the ancillary buffer a fixed compile-time size (`cmsg_space!(ScmRights(1))`) - a batched send
//! would need runtime cmsg sizing for no real gain, since a window has a handful of panes.
//!
//! The header is a text message with the same framing minus the fd: TOML `{version, panes,
//! session}`. `session` is the embedding app's own layout payload ([`Layout`]); the transport never
//! looks inside it except to ask how many panes it expects. The ACK is a text message too.
//!
//! The sender and receiver must run CONCURRENTLY (they're separate processes in practice): a
//! payload larger than the socket buffer (~8K on macOS) blocks in `write_all` until the peer
//! drains it. Pane metadata is far smaller than that, but the property is real - don't "simplify"
//! this into a single-threaded send-then-receive.
//!
//! Everything here goes through `rustix`, whose `sendmsg`/`recvmsg`/`ScmRights` wrappers are SAFE -
//! the crate forbids `unsafe`, so a raw `libc::sendmsg` was never an option.
//!
//! # The exchange
//!
//! ```text
//! predecessor                                  successor (told where the socket is)
//!   listen SOCK, launch the successor -------->  connect SOCK
//!   accept_within (with a timeout)
//!   header: TOML {version, panes, session} --->  check version + pane count
//!   one message per pane, LEAF ORDER --------->  adopt_pane each, rebuild the windows
//!   wait for the ACK                    <-----   "adopted" (only once every pane is LIVE)
//!   mark_handed_off + close, nothing else
//! ```
//!
//! Pane order is part of the contract: panes travel in the order the layout's leaves are walked
//! (the same walk on both sides), and the receiver pairs fd N with leaf N by position alone.
//!
//! # Failure: the old window stays up
//!
//! The ACK is the whole safety story: until it arrives the predecessor has changed nothing, so any
//! failure (timeout, refused connect, version mismatch, short read, no ACK) degrades to "the restart
//! didn't happen" - the window stays up and the shells stay ours. `mark_handed_off` (which disarms
//! `kill` and `Drop`) is only ever reached after the successor confirmed live panes, and the close
//! must be the LAST thing that happens: our reader threads keep stealing pty bytes until we exit.
//!
//! A successor that failed to receive decides what to do with [`fallback`].
//!
//! # What the embedding app must do
//!
//! Predecessor: [`listen`], launch the successor, [`accept_within`] ([`LAUNCH_TIMEOUT`]),
//! [`capture_pane`] every pane in leaf order, [`send_session`]; on `Ok`, call
//! `PtyTerm::mark_handed_off` on every pane and exit without killing anything.
//!
//! Successor: [`Incoming::receive`], [`adopt_pane`] each pane (seeding `SpawnOpts::cwd` from
//! [`PaneMeta::cwd`]), then [`Incoming::ack`], and only after the ACK is delivered call
//! `PtyTerm::arm_teardown` on every adopted pane. A pane that fails to adopt is already reaped by
//! [`adopt_pane`]; the app starts a fresh shell in its place.
//!
//! The protocol version ([`PROTOCOL`]) must match on both sides, so both sides must run builds of
//! the same embedding app: a mismatch aborts the handover and the predecessor keeps its window.
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use serde::{Deserialize, Serialize};

use crate::repaint::Repaint;
use crate::terminal::{Adopted, PtyTerm, SpawnOpts, clamp_title, reap_orphaned_session};

/// Cap on a single pane's metadata, so a corrupt or hostile length can't make us allocate wildly.
/// Generous because the metadata now carries the pane's SCREEN (base64'd, itself capped by
/// `screen::MAX_DUMP_BYTES`); a legitimate pane sits comfortably under half of this.
const MAX_META: usize = 512 * 1024;

/// Cap on a text message (the header carries the whole session; the ACK is one word).
const MAX_TEXT: usize = 1024 * 1024;

/// Cap on the pane count a header may claim, so a hostile number can't drive a huge allocation.
const MAX_PANES: usize = 512;

/// Wire-format version. Bump on ANY change to the framing, the message order, or the metadata
/// fields. A mismatch ABORTS the handoff: an older build must never adopt fds under a contract it
/// cannot interpret, and a rollback is exactly when that happens.
pub const PROTOCOL: u32 = 2;

/// What the successor answers once every pane is adopted and live. Nothing else counts as consent.
const ACK: &str = "adopted";

/// Per-message socket timeout. Generous (the peer is a process mid-launch) but finite: a wedged
/// successor must not park the predecessor's UI thread forever.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the predecessor waits for the successor to connect. A launch plus the app's window
/// startup is ~1-3s warm; this is the give-up ceiling, after which the handoff aborts and nothing
/// changes. Pass it to [`accept_within`].
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(12);

/// The embedding app's layout payload, riding in the header. The transport only needs to know how
/// many panes follow it, so a header whose claimed count disagrees with the layout is refused.
///
/// It must serialize to a TOML table.
pub trait Layout: Serialize + serde::de::DeserializeOwned {
    /// Pane messages this layout is paired with: one per leaf, in the order the app walks them.
    fn pane_count(&self) -> usize;
}

/// The handoff header: what the successor needs before a single fd arrives - the contract version,
/// how many pane messages follow, and the app's layout the fds slot into.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Header<S> {
    version: u32,
    /// Pane messages that follow, in leaf order across the layout.
    panes: usize,
    session: S,
}

/// One pane's metadata, riding with its master fd. Construct it with `..Default::default()`: fields
/// are added without a [`PROTOCOL`] bump whenever an absent value can mean "unknown".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneMeta {
    /// Process GROUP the successor becomes responsible for killing. It comes across the wire
    /// because the successor is NOT the shell's parent and cannot look the group up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// The pane's cwd at handover. The adopted shell won't re-emit OSC 7 until its next prompt, so
    /// this is the only thing the successor can seed the pane's cwd (and its basename auto-title)
    /// from. The layout still comes from the header's session tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Seconds the shell had already been alive, so the crash-loop guard isn't fooled by a handover.
    pub alive_secs: f32,
    pub cols: usize,
    pub rows: usize,
    /// Was a full-screen app holding the ALT screen? The successor's `Term` has parsed nothing, so
    /// this is the only way it can know - and it decides how the pane is asked to repaint (SIGWINCH
    /// for a TUI, `^L` for a shell prompt). Defaulted so a decode never hard-fails on it.
    #[serde(default)]
    pub alt_screen: bool,
    /// Was a command RUNNING (OSC 133) rather than the shell sitting at its prompt? `None` = we do
    /// not know, which is what an ABSENT field means: a 1.6.2 predecessor never sent it, and a shell
    /// that emits no OSC 133 has no answer to give. Deliberately three-state and `serde(default)` so
    /// this could be added WITHOUT bumping `PROTOCOL` - a version bump makes the first restart after
    /// an upgrade refuse the handoff, and the user already paid that once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_running: Option<bool>,
    /// Theme reports (mode 2031) the app enabled; see `terminal::Adopted`. Defaulted like
    /// `cmd_running`, so no `PROTOCOL` bump.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub theme_reports: bool,
    /// The pane's screen (plus a bounded tail of scrollback) as ANSI, base64'd - what lets the
    /// successor show what was ALREADY on screen instead of a blank grid. See `screen`.
    ///
    /// Base64 rather than a raw string because the dump is arbitrary control bytes, which a TOML
    /// basic string would escape into six bytes apiece. Absent from a 1.6.2 predecessor, hence
    /// optional - and hence addable without a `PROTOCOL` bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// The pane's live OSC 0/2 window title. Carried for the same reason as `cwd`: an app re-emits
    /// its title only when the title CHANGES, so a Claude/vim pane that had one would otherwise fall
    /// back to its cwd basename for the rest of the session (the reported `vladjerca` tab).
    /// Clamped by [`clamp_title`] - it is untrusted, app-supplied text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_osc: Option<String>,
    /// The embedding app's own per-pane data. The transport never reads it and `Null` (the default)
    /// is not written at all, so an app that sets nothing keeps the wire byte-identical. It rides
    /// in TOML, which has no null: nothing inside it may be `null`, or the send fails.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

/// Panes handed to us by a predecessor, received BEFORE the window exists (the transport needs no
/// GUI context, and having the fds in hand first means the app can adopt them during startup).
/// The socket is kept open: the ACK is only sent once every pane is a live `PtyTerm`.
pub struct Incoming<S> {
    sock: UnixStream,
    session: S,
    panes: Vec<(PaneMeta, OwnedFd)>,
}

impl<S: Layout> Incoming<S> {
    /// Connect to a predecessor's handoff socket and read the header + every pane fd.
    pub fn receive(path: &Path) -> std::io::Result<Self> {
        let sock = connect(path)?;
        let (session, panes) = recv_session(&sock)?;
        Ok(Self { sock, session, panes })
    }

    pub fn session(&self) -> &S {
        &self.session
    }

    /// The received panes in leaf order across the layout. Taken (not borrowed) because each
    /// `OwnedFd` moves into a `PtyTerm`.
    pub fn take_panes(&mut self) -> Vec<(PaneMeta, OwnedFd)> {
        std::mem::take(&mut self.panes)
    }

    /// Tell the predecessor it may let go. MUST be called only after every pane is live: this is
    /// what makes it stop guarding the shells, and nothing can be undone afterwards.
    pub fn ack(&self) -> std::io::Result<()> {
        send_text(&self.sock, ACK)
    }
}

/// What a successor should do when the handoff it was launched for did NOT complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// A live primary still owns the session (it never got our ACK, so it kept every shell):
    /// a second window would only fight it for the instance socket. Exit and let it explain.
    ExitQuietly,
    /// Nobody is running: never leave the user staring at nothing after clicking Restart - open a
    /// normal window with the ordinary session restore.
    FreshWindow,
}

pub fn fallback(primary_alive: bool) -> Fallback {
    if primary_alive { Fallback::ExitQuietly } else { Fallback::FreshWindow }
}

/// `alive_secs` off the wire as a `Duration`. Negative / NaN / absurd values collapse to zero
/// rather than panicking (`Duration::from_secs_f32` panics on both) - it only feeds the crash-loop
/// guard, so "assume freshly started" is the safe reading.
fn alive_duration(secs: f32) -> Duration {
    Duration::try_from_secs_f32(secs).unwrap_or(Duration::ZERO)
}

/// Grid geometry off the wire, clamped: a pty is at least 1x1, and a hostile `cols` would otherwise
/// have `Term::new` allocate a grid of that width.
fn grid_dims(cols: usize, rows: usize) -> (usize, usize) {
    const MAX_DIM: usize = 4096;
    (cols.clamp(1, MAX_DIM), rows.clamp(1, MAX_DIM))
}

/// Bind the handoff socket (creating its dir, clearing a stale file first).
pub fn listen(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
}

/// Wait for the successor to connect, giving up after `timeout`. Polled rather than blocking so the
/// give-up is real: a successor that never launches (or is an older build that ignores `--adopt`)
/// must not park us forever.
pub fn accept_within(listener: &UnixListener, timeout: Duration) -> std::io::Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((sock, _)) => {
                sock.set_nonblocking(false)?;
                arm_timeouts(&sock)?;
                return Ok(sock);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "successor never connected",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Connect to the predecessor's socket. It binds BEFORE launching us, so the path exists already;
/// the short retry only covers a slow filesystem view of it.
fn connect(path: &Path) -> std::io::Result<UnixStream> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(path) {
            Ok(sock) => {
                arm_timeouts(&sock)?;
                return Ok(sock);
            }
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn arm_timeouts(sock: &UnixStream) -> std::io::Result<()> {
    sock.set_read_timeout(Some(IO_TIMEOUT))?;
    sock.set_write_timeout(Some(IO_TIMEOUT))
}

/// Send the whole session: header, then one message per pane in `panes` order (which MUST be leaf
/// order across the layout - see [`Layout::pane_count`]), then WAIT for the successor's ACK.
///
/// `Ok(())` therefore means "the successor has every pane live and confirmed it". Every other
/// outcome means nothing was handed over: the caller still owns its shells and must keep them.
pub fn send_session<S: Layout>(
    sock: &UnixStream,
    session: &S,
    panes: &[(PaneMeta, BorrowedFd<'_>)],
) -> std::io::Result<()> {
    if panes.len() != session.pane_count() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pane count does not match the session layout",
        ));
    }
    let header = Header { version: PROTOCOL, panes: panes.len(), session };
    send_text(sock, &encode(&header)?)?;
    for (meta, fd) in panes {
        send_pane(sock, &encode(meta)?, *fd)?;
    }
    let reply = recv_text(sock)?;
    if reply.trim() != ACK {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("successor did not acknowledge (got {reply:?})"),
        ));
    }
    Ok(())
}

/// Receive what [`send_session`] sent, refusing anything that doesn't line up: a foreign protocol
/// version, a pane count that disagrees with the layout, or a metadata blob that won't decode. The
/// ACK is deliberately NOT sent here - the caller sends it once every fd is a live `PtyTerm`.
pub fn recv_session<S: Layout>(
    sock: &UnixStream,
) -> std::io::Result<(S, Vec<(PaneMeta, OwnedFd)>)> {
    let header: Header<S> = decode(&recv_text(sock)?)?;
    if header.version != PROTOCOL {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("handoff protocol {} != ours {PROTOCOL}", header.version),
        ));
    }
    if header.panes > MAX_PANES || header.panes != header.session.pane_count() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "header pane count does not match the session layout",
        ));
    }
    let mut panes = Vec::with_capacity(header.panes);
    for _ in 0..header.panes {
        let (meta, fd) = recv_pane(sock)?;
        panes.push((decode(&meta)?, fd));
    }
    Ok((header.session, panes))
}

/// TOML for a wire struct (`toml` is already a dependency for the config/session files).
fn encode<T: Serialize>(value: &T) -> std::io::Result<String> {
    toml::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> std::io::Result<T> {
    toml::from_str(text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Send a length-prefixed text message (no fd). Same framing as [`send_pane`] minus the ancillary
/// data, so both can share one stream without either side guessing where a message ends.
fn send_text(sock: &UnixStream, text: &str) -> std::io::Result<()> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_TEXT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "handoff text message too large",
        ));
    }
    let len = u32::try_from(bytes.len()).expect("checked against MAX_TEXT above");
    let mut sock = sock;
    sock.write_all(&len.to_be_bytes())?;
    sock.write_all(bytes)
}

fn recv_text(sock: &UnixStream) -> std::io::Result<String> {
    let mut sock = sock;
    let mut header = [0u8; 4];
    sock.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_TEXT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handoff text message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    sock.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Send one pane: its `meta` text plus the live pty master `fd`.
fn send_pane(sock: &UnixStream, meta: &str, fd: BorrowedFd<'_>) -> std::io::Result<()> {
    let bytes = meta.as_bytes();
    if bytes.len() > MAX_META {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pane metadata too large",
        ));
    }
    let len = u32::try_from(bytes.len()).expect("checked against MAX_META above");
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    // The fd rides along with the FIRST byte of the message, so the length prefix and the fd must
    // go in the same sendmsg - splitting them would attach the fd to the wrong read.
    let fds = [fd];
    control.push(SendAncillaryMessage::ScmRights(&fds));
    let header = len.to_be_bytes();
    let iov = [IoSlice::new(&header)];
    let sent = sendmsg(sock, &iov, &mut control, SendFlags::empty())?;
    if sent != header.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short handoff header"));
    }
    // The payload itself carries no ancillary data, so a plain write (which handles partial
    // writes) is correct here.
    (&mut { sock }).write_all(bytes)
}

/// Receive one pane sent by [`send_pane`]: its metadata and the adopted master fd.
fn recv_pane(sock: &UnixStream) -> std::io::Result<(String, OwnedFd)> {
    let mut header = [0u8; 4];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let mut iov = [IoSliceMut::new(&mut header)];
    let got = recvmsg(sock, &mut iov, &mut control, RecvFlags::empty())?;
    if got.bytes != header.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "truncated handoff header",
        ));
    }
    let fd = control
        .drain()
        .find_map(|msg| match msg {
            RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
            _ => None,
        })
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "handoff message carried no fd")
        })?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_META {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pane metadata too large",
        ));
    }
    let mut buf = vec![0u8; len];
    (&mut { sock }).read_exact(&mut buf)?;
    let meta = String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((meta, fd))
}

/// A pane's master fd and the metadata that travels with it, or `None` when the fd cannot be
/// borrowed: the caller must then abort the whole handoff, because half a session is worse than no
/// handoff at all. Leave [`PaneMeta::extra`] to the app: fill it on the returned value.
pub fn capture_pane(term: &PtyTerm) -> Option<(PaneMeta, BorrowedFd<'_>)> {
    let (fd, pgid) = term.handoff_fd()?;
    let meta = PaneMeta {
        pgid,
        cwd: pane_cwd(term),
        alive_secs: term.alive().as_secs_f32(),
        cols: term.cols(),
        rows: term.rows(),
        alt_screen: term.is_alt_screen(),
        cmd_running: cmd_running(term.cmd_state()),
        screen: encode_screen(&term.screen_dump()),
        title_osc: clamp_title(term.title_osc()),
        theme_reports: term.theme_reports(),
        extra: serde_json::Value::Null,
    };
    Some((meta, fd))
}

/// Turn a received pane into a live `PtyTerm`. The metadata is untrusted wire data: geometry is
/// clamped, a bad `alive_secs` reads as zero, a screen dump that will not decode is dropped.
///
/// When the adoption fails the shell would be orphaned - the failed call dropped the only fd we had
/// for it, and the predecessor disarms every pane as soon as we acknowledge - so this reaps its
/// process group before returning the error. The caller starts a fresh shell instead.
///
/// `opts.cwd` is the app's call: the pane's own [`PaneMeta::cwd`] beats a saved one, since it rides
/// with the fd and an adopted pty has no other way to know it.
pub fn adopt_pane(
    repaint: Repaint,
    meta: &PaneMeta,
    fd: OwnedFd,
    opts: &SpawnOpts,
) -> std::io::Result<PtyTerm> {
    let (cols, rows) = grid_dims(meta.cols, meta.rows);
    let handover = Adopted {
        fd,
        cols,
        rows,
        pgid: meta.pgid,
        alive: alive_duration(meta.alive_secs),
        alt_screen: meta.alt_screen,
        cmd_running: meta.cmd_running,
        replay: decode_screen(meta.screen.as_ref()),
        title_osc: clamp_title(meta.title_osc.clone()),
        theme_reports: meta.theme_reports,
    };
    PtyTerm::adopt(repaint, handover, opts).inspect_err(|_| {
        if let Some(pgid) = meta.pgid {
            reap_orphaned_session(pgid);
        }
    })
}

/// The cwd to hand a pane over with: the one we tracked, else the one the OS says its shell is in.
///
/// The tracked cwd comes ONLY from OSC 7, and on macOS zsh emits that from
/// `/etc/zshrc_Apple_Terminal` - sourced only when `TERM_PROGRAM == Apple_Terminal`, which ours
/// never is. A shell whose own rc files don't emit it therefore has no tracked cwd at all, and after
/// a handoff that pane has nothing left to name its tab from: the successor's `Term` starts with no
/// title and no scrollback either, so the tab sits on the bare "zsh" placeholder until the user
/// happens to `cd`. Asking the OS costs one targeted process refresh per pane, once per restart.
fn pane_cwd(term: &PtyTerm) -> Option<String> {
    term.cwd().or_else(|| term.shell_pid().and_then(crate::procwatch::process_cwd))
}

/// A pane's screen dump for the wire: base64, or `None` when there is nothing on screen.
fn encode_screen(dump: &[u8]) -> Option<String> {
    use base64::Engine as _;
    (!dump.is_empty()).then(|| base64::engine::general_purpose::STANDARD.encode(dump))
}

/// The screen dump off the wire. A blob that will not decode is DROPPED, never fatal: a pane that
/// comes back without its old screen is a cosmetic loss, a refused handoff is not.
fn decode_screen(screen: Option<&String>) -> Vec<u8> {
    use base64::Engine as _;
    screen
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .unwrap_or_default()
}

/// Is a command running, as far as OSC 133 has told us? `Idle` is NOT "at a prompt": it is also what
/// a shell that emits no OSC 133 at all reports forever, so it maps to `None` (unknown) and lets the
/// successor decide from the tty instead. Only a completed command (`Ok`/`Fail`) proves a prompt.
fn cmd_running(cmd: crate::terminal::CmdState) -> Option<bool> {
    use crate::terminal::CmdState::{Fail, Idle, Ok, Running};
    match cmd {
        Running => Some(true),
        Ok | Fail => Some(false),
        Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    use super::*;
    use crate::terminal::MAX_TITLE_CHARS;

    #[test]
    fn passed_fd_is_the_same_open_file() {
        // The mechanism, proven on a pipe (no pty needed): send the READ end across, then write to
        // the write end and read it back through the RECEIVED fd. If SCM_RIGHTS didn't really
        // transfer the description, this read would see nothing.
        let (a, b) = UnixStream::pair().unwrap();
        let (rx, mut tx) = std::io::pipe().unwrap();

        send_pane(&a, r#"{"cwd":"/tmp","pgid":42}"#, rx.as_fd()).unwrap();
        let (meta, adopted) = recv_pane(&b).unwrap();
        assert_eq!(meta, r#"{"cwd":"/tmp","pgid":42}"#);

        tx.write_all(b"through-the-fd").unwrap();
        drop(tx);
        let mut out = String::new();
        std::fs::File::from(adopted).read_to_string(&mut out).unwrap();
        assert_eq!(out, "through-the-fd");
    }

    #[test]
    fn metadata_survives_a_payload_larger_than_the_socket_buffer() {
        // The fd rides with the 4-byte header and the payload follows on the stream, so a payload
        // bigger than one read must still arrive whole. Sent from a THREAD because a payload that
        // exceeds the socket buffer (~8K on macOS) blocks in `write_all` until someone drains it -
        // which mirrors the real shape, where the receiver is a separate process reading live.
        let (a, b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(32 * 1024));

        let sent = big.clone();
        let sender = std::thread::spawn(move || send_pane(&a, &sent, rx.as_fd()).unwrap());
        let (meta, _fd) = recv_pane(&b).unwrap();
        sender.join().unwrap();
        assert_eq!(meta, big);
    }

    #[test]
    fn oversized_metadata_is_rejected_not_allocated() {
        let (a, _b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let too_big = "x".repeat(MAX_META + 1);
        assert!(send_pane(&a, &too_big, rx.as_fd()).is_err());
    }

    /// A layout that is just a list of leaf names: three leaves means three pane messages.
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct TestLayout {
        leaves: Vec<String>,
    }

    impl Layout for TestLayout {
        fn pane_count(&self) -> usize {
            self.leaves.len()
        }
    }

    fn three_leaf_session() -> TestLayout {
        TestLayout { leaves: vec!["left".into(), "right".into(), "solo".into()] }
    }

    fn meta(n: usize) -> PaneMeta {
        PaneMeta {
            pgid: Some(1000 + n as u32),
            cwd: Some(format!("/pane{n}")),
            alive_secs: 12.5,
            cols: 80 + n,
            rows: 24,
            // Alternate per pane so a field that stopped round-tripping shows up as a mismatch.
            alt_screen: n % 2 == 1,
            cmd_running: n.is_multiple_of(2).then_some(n == 0),
            title_osc: (n == 1).then(|| "claude - pane 1".to_owned()),
            theme_reports: n == 1,
            // A dump big enough to prove the metadata survives past one socket buffer.
            screen: encode_screen(format!("\x1b[0mpane-{n} screen{}", "x".repeat(9000)).as_bytes()),
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn header_round_trips_through_toml() {
        let h = Header { version: PROTOCOL, panes: 3, session: three_leaf_session() };
        let back: Header<TestLayout> = decode(&encode(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        let m = meta(2);
        assert_eq!(decode::<PaneMeta>(&encode(&m).unwrap()).unwrap(), m);
    }

    #[test]
    fn an_apps_own_pane_data_round_trips_and_an_empty_one_is_not_written() {
        let plain = encode(&meta(0)).unwrap();
        assert!(!plain.contains("extra"), "an app that sets nothing keeps the wire as it was");
        let with = PaneMeta {
            extra: serde_json::json!({"workspace": "w1", "flags": [1, 2, 3], "pinned": true}),
            ..meta(1)
        };
        let back: PaneMeta = decode(&encode(&with).unwrap()).unwrap();
        assert_eq!(back, with);
        assert_eq!(decode::<PaneMeta>(&plain).unwrap().extra, serde_json::Value::Null);
    }

    #[test]
    fn a_pane_count_that_disagrees_with_the_layout_is_refused() {
        // Sender side: pairing fds to leaves by position only works if the counts match, so a
        // mismatch must never reach the wire.
        let (a, _b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let panes = [(meta(0), rx.as_fd())]; // one fd, three leaves
        assert!(send_session(&a, &three_leaf_session(), &panes).is_err());
    }

    #[test]
    fn a_foreign_protocol_version_is_refused_before_any_fd_is_read() {
        // A rollback to an older build must not adopt fds under a contract it cannot interpret, so
        // the version is checked before the pane loop.
        let (a, b) = UnixStream::pair().unwrap();
        let h = Header { version: PROTOCOL + 1, panes: 0, session: TestLayout::default() };
        send_text(&a, &encode(&h).unwrap()).unwrap();
        let err = recv_session::<TestLayout>(&b).expect_err("a version mismatch must abort");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("protocol"), "got {err}");
    }

    #[test]
    fn a_header_claiming_more_panes_than_the_layout_is_refused() {
        let (a, b) = UnixStream::pair().unwrap();
        let h = Header { version: PROTOCOL, panes: 9, session: three_leaf_session() };
        send_text(&a, &encode(&h).unwrap()).unwrap();
        assert!(recv_session::<TestLayout>(&b).is_err());
    }

    #[test]
    fn a_whole_session_moves_and_the_fds_land_in_leaf_order() {
        // The full exchange over a socket pair: header, three panes, ACK. Each pane carries the read
        // end of its OWN pipe, so reading through the RECEIVED fd proves both that the descriptions
        // transferred and that fd N ended up paired with leaf N (a transposition here would silently
        // hand a user's editor pane to the wrong split).
        let (a, b) = UnixStream::pair().unwrap();
        let session = three_leaf_session();
        let pipes: Vec<_> = (0..3).map(|_| std::io::pipe().unwrap()).collect();
        for (n, (_rx, tx)) in pipes.iter().enumerate() {
            let mut tx = tx;
            write!(tx, "pane-{n}").unwrap();
        }

        // Sent from a thread: `send_session` blocks on the ACK, which only comes after the receive.
        let sent = session.clone();
        let sender = std::thread::spawn(move || {
            let panes: Vec<(PaneMeta, BorrowedFd<'_>)> =
                pipes.iter().enumerate().map(|(n, (rx, _))| (meta(n), rx.as_fd())).collect();
            send_session(&a, &sent, &panes)
        });

        let (got_session, got_panes) = recv_session::<TestLayout>(&b).unwrap();
        assert_eq!(got_session, session, "the layout must arrive intact");
        assert_eq!(got_panes.len(), 3);
        for (n, (m, fd)) in got_panes.into_iter().enumerate() {
            assert_eq!(m, meta(n), "pane {n} metadata");
            let mut buf = [0u8; 6];
            std::fs::File::from(fd).read_exact(&mut buf).unwrap();
            assert_eq!(&buf, format!("pane-{n}").as_bytes(), "pane {n} got the wrong fd");
        }
        // Only the ACK completes the sender's call - that is what releases the shells.
        send_text(&b, ACK).unwrap();
        sender.join().unwrap().expect("a confirmed handoff must report success");
    }

    #[test]
    fn a_receiver_that_never_acknowledges_fails_the_send() {
        // The invariant that keeps a failed handoff harmless: no ACK, no success, so the caller
        // keeps its shells.
        let (a, b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let session = TestLayout { leaves: vec!["only".into()] };
        let sender = std::thread::spawn(move || {
            let panes = [(meta(0), rx.as_fd())];
            send_session(&a, &session, &panes)
        });
        let received = recv_session::<TestLayout>(&b);
        drop(b); // adopt failed / we crashed: hang up instead of acknowledging
        assert!(received.is_ok());
        assert!(
            sender.join().unwrap().is_err(),
            "an unacknowledged handoff must not report success"
        );
    }

    #[test]
    fn accept_gives_up_instead_of_waiting_forever() {
        // An older successor ignores `--adopt` and never connects; the predecessor has to come back
        // and keep running rather than park its UI thread for good.
        let dir = std::env::temp_dir().join(format!("stdusk-handoff-test-{}", std::process::id()));
        let path = dir.join("nobody.sock");
        let listener = listen(&path).unwrap();
        let err = accept_within(&listener, Duration::from_millis(80))
            .expect_err("nobody connected, so this must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        // ...and a successor that DOES connect is handed the stream.
        let client = std::thread::spawn({
            let path = path.clone();
            move || UnixStream::connect(path).unwrap()
        });
        let sock = accept_within(&listener, Duration::from_secs(2)).expect("a live connect");
        send_text(&sock, ACK).unwrap();
        let peer = client.join().unwrap();
        assert_eq!(recv_text(&peer).unwrap(), ACK);
        drop(listener);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn hostile_metadata_never_panics_or_allocates_wildly() {
        // `alive_secs` and the geometry come off a socket, so they are untrusted input:
        // `Duration::from_secs_f32` PANICS on negative/NaN, and a huge `cols` would have
        // `Term::new` allocate a grid that wide.
        assert_eq!(alive_duration(2.5), Duration::from_millis(2500));
        for bad in [-1.0, f32::NAN, f32::NEG_INFINITY, f32::INFINITY, 1e30] {
            assert_eq!(alive_duration(bad), Duration::ZERO, "alive_secs {bad}");
        }
        assert_eq!(grid_dims(120, 40), (120, 40));
        assert_eq!(grid_dims(0, 0), (1, 1));
        assert_eq!(grid_dims(usize::MAX, usize::MAX), (4096, 4096));
    }

    #[test]
    fn a_failed_handoff_only_opens_a_window_when_nothing_else_is_running() {
        // The predecessor keeps the whole session when it gets no ACK, so a second window would just
        // fight it; with nobody running, a window is the only acceptable outcome.
        assert_eq!(fallback(true), Fallback::ExitQuietly);
        assert_eq!(fallback(false), Fallback::FreshWindow);
    }

    #[test]
    fn a_screen_dump_survives_the_wire_and_a_corrupt_one_is_dropped() {
        // The dump is arbitrary control bytes, so it rides base64'd rather than as a TOML string.
        let dump = b"\x1b[0mhello\r\n\x1b[0m\x1b[1;1H";
        let meta = PaneMeta { screen: encode_screen(dump), ..Default::default() };
        let back: PaneMeta = decode(&encode(&meta).unwrap()).unwrap();
        assert_eq!(decode_screen(back.screen.as_ref()), dump);
        // Absent (a predecessor too old to send one) and garbage both mean "no replay", never an
        // aborted handoff - losing the old screen is cosmetic, refusing the restart is not.
        assert_eq!(decode_screen(None), [] as [u8; 0]);
        assert_eq!(decode_screen(Some(&"not base64 !!".to_owned())), [] as [u8; 0]);
        assert_eq!(encode_screen(&[]), None, "a blank screen is sent as nothing at all");
    }

    #[test]
    fn an_osc_title_is_clamped_before_it_travels() {
        // App-supplied text, so it is untrusted on both sides: a blank title is no title, and a
        // hostile length is truncated by CHARS (bytes would split a code point).
        assert_eq!(clamp_title(Some("claude - locales".into())), Some("claude - locales".into()));
        assert_eq!(clamp_title(None), None);
        assert_eq!(clamp_title(Some(String::new())), None);
        assert_eq!(clamp_title(Some("   ".into())), None);
        let huge = clamp_title(Some("\u{1f600}".repeat(5000))).expect("truncated, not dropped");
        assert_eq!(huge.chars().count(), MAX_TITLE_CHARS);
        assert!(huge.chars().all(|c| c == '\u{1f600}'), "truncation must not split a code point");
    }

    fn quiet_opts(cwd: Option<String>, profile_args: &str) -> SpawnOpts {
        SpawnOpts {
            detect_progress: false,
            shell_integration: false,
            autosuggestions: false,
            scrollback_lines: 200,
            word_separators: " ".into(),
            bold_bright: false,
            agent_tracking: None,
            cwd,
            profile: Some(crate::shell::ShellProfile {
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), profile_args.into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
            }),
        }
    }

    #[test]
    fn a_pane_that_never_emitted_osc_7_still_hands_over_its_directory() {
        // The "zsh" tab bug: a shell whose rc files never emit OSC 7 has no tracked cwd, so the
        // handover carried `cwd: None` and the successor had nothing to name the tab from. A REAL
        // pty is needed: this only reproduces with a live shell process to ask the OS about.
        let dir = std::env::temp_dir().join(format!("stdusk-pane-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let opts = quiet_opts(Some(dir.to_string_lossy().into_owned()), "sleep 30");
        let mut term = PtyTerm::spawn(80, 24, Repaint::none(), &opts);
        // Wait for the shell to exist (the pgid is captured at spawn, but be honest about timing).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while term.shell_pid().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(term.cwd(), None, "nothing emitted OSC 7, so there is no tracked cwd");
        let handed = pane_cwd(&term).expect("the OS knows where the shell is");
        assert_eq!(
            std::fs::canonicalize(&handed).ok(),
            std::fs::canonicalize(&dir).ok(),
            "handed-over cwd {handed}"
        );
        term.kill();
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_captured_pane_carries_its_fd_and_the_app_fills_the_extra_slot() {
        let mut term = PtyTerm::spawn(80, 24, Repaint::none(), &quiet_opts(None, "sleep 30"));
        let mut meta = capture_pane(&term).expect("a spawned pty has a borrowable master").0;
        assert_eq!((meta.cols, meta.rows), (80, 24));
        assert_eq!(meta.extra, serde_json::Value::Null);
        meta.extra = serde_json::json!({"mine": 1});
        assert_eq!(decode::<PaneMeta>(&encode(&meta).unwrap()).unwrap(), meta);
        term.kill();
    }

    #[test]
    fn an_adopted_pane_takes_its_clamped_geometry_from_the_wire() {
        let (rx, _tx) = std::io::pipe().unwrap();
        let hostile = PaneMeta { cols: usize::MAX, rows: 0, theme_reports: true, ..meta(0) };
        let term = adopt_pane(Repaint::none(), &hostile, OwnedFd::from(rx), &quiet_opts(None, ""))
            .expect("a pipe is as good as a pty master for adoption");
        assert_eq!((term.cols(), term.rows()), (4096, 1));
        assert!(term.theme_reports());
    }

    #[test]
    fn oversized_text_is_rejected_not_allocated() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(send_text(&a, &"x".repeat(MAX_TEXT + 1)).is_err());
        // A hostile length prefix must be refused before the buffer is allocated.
        let mut w = &a;
        w.write_all(&u32::try_from(MAX_TEXT + 1).unwrap().to_be_bytes()).unwrap();
        assert!(recv_text(&b).is_err());
    }

    #[test]
    fn a_header_from_a_build_that_sent_a_pane_token_still_decodes_and_the_token_is_dropped() {
        // Builds of this branch before the hookless capture sent `pane_token`. Serde ignores an
        // unknown key, so the handoff stays compatible both ways.
        let text = encode(&meta(0)).unwrap();
        assert!(!text.contains("pane_token"), "the token is not written any more");
        let old = text.replacen('{', &format!("{{\"pane_token\":\"{}\",", "ab".repeat(16)), 1);
        assert_eq!(decode::<PaneMeta>(&old).unwrap(), meta(0));
    }
}
