//! Session handoff: move LIVE pty master fds from a running stdusk to a freshly-launched one over
//! a unix socket, so updating the app restarts the UI without killing the shells. The shell keeps
//! running because its master fd is never closed - it just changes owner; the child reparents to
//! launchd, which is fine (we track it by process group, not parentage).
//!
//! Wire format, one message per pane: a `u32` big-endian length, that many bytes of JSON metadata,
//! and exactly ONE fd in the `SCM_RIGHTS` control message. One fd per message keeps the ancillary
//! buffer a fixed compile-time size (`cmsg_space!(ScmRights(1))`) - a batched send would need
//! runtime cmsg sizing for no real gain, since a window has a handful of panes.
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
//! predecessor                                  successor (launched with --adopt SOCK)
//!   bind SOCK, `open -n -a <bundle>` --------->  connect SOCK
//!   accept (with a timeout)
//!   header: TOML {version, panes, session} --->  check version + pane count
//!   one message per pane, LEAF ORDER --------->  PtyTerm::adopt each, rebuild the tabs
//!   wait for the ACK                    <-----   "adopted" (only once every pane is LIVE)
//!   mark_handed_off + close, nothing else
//! ```
//!
//! The ACK is the whole safety story: until it arrives the predecessor has changed nothing, so any
//! failure (timeout, refused connect, version mismatch, short read) degrades to "the restart didn't
//! happen" - the window stays up and the shells stay ours. `mark_handed_off` (which disarms `kill`
//! and `Drop`) is only ever reached after the successor confirmed live panes, and the close must be
//! the LAST thing that happens: our reader threads keep stealing pty bytes until we exit.
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use serde::{Deserialize, Serialize};

use crate::session::SavedSession;

/// Cap on a single pane's metadata, so a corrupt or hostile length can't make us allocate wildly.
const MAX_META: usize = 64 * 1024;

/// Cap on a text message (the header carries the whole session; the ACK is one word).
const MAX_TEXT: usize = 1024 * 1024;

/// Cap on the pane count a header may claim, so a hostile number can't drive a huge allocation.
const MAX_PANES: usize = 512;

/// Wire-format version. Bump on ANY change to the framing, the message order, or the metadata
/// fields. A mismatch ABORTS the handoff: an older build must never adopt fds under a contract it
/// cannot interpret, and a rollback is exactly when that happens.
const PROTOCOL: u32 = 1;

/// What the successor answers once every pane is adopted and live. Nothing else counts as consent.
const ACK: &str = "adopted";

/// Per-message socket timeout. Generous (the peer is a process mid-launch) but finite: a wedged
/// successor must not park the predecessor's UI thread forever.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the predecessor waits for the successor to connect. `open` + launchd + eframe startup
/// is ~1-3s warm; this is the give-up ceiling, after which the handoff aborts and nothing changes.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(12);

/// Set once a successor CONFIRMED the handoff. Process-global because it outlives the app struct:
/// `main` reads it on the way out to decide whether the single-instance socket is still ours to
/// unlink - after a handoff the successor has rebound that path and owns it.
static GAVE_SOCKET: AtomicBool = AtomicBool::new(false);

/// Did a successor take over our single-instance socket? (See [`GAVE_SOCKET`].)
pub(crate) fn gave_instance_socket() -> bool {
    GAVE_SOCKET.load(Ordering::SeqCst)
}

/// The handoff header: what the successor needs before a single fd arrives - the contract version,
/// how many pane messages follow, and the session (tab titles/colors/pinning + the pane trees) the
/// fds slot into.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Header {
    pub(crate) version: u32,
    /// Pane messages that follow, in leaf order across `session.tabs` (A before B per tab).
    pub(crate) panes: usize,
    pub(crate) session: SavedSession,
}

/// One pane's metadata, riding with its master fd.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PaneMeta {
    /// Process GROUP the successor becomes responsible for killing. It comes across the wire
    /// because the successor is NOT the shell's parent and cannot look the group up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pgid: Option<u32>,
    /// The pane's cwd at handover. The adopted shell won't re-emit OSC 7 until its next prompt, so
    /// this is the only thing the successor can seed the pane's cwd (and its basename auto-title)
    /// from. The layout still comes from the header's session tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    /// Seconds the shell had already been alive, so the crash-loop guard isn't fooled by a handover.
    pub(crate) alive_secs: f32,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

/// Panes handed to us by a predecessor, received BEFORE the window exists (the transport needs no
/// egui context, and having the fds in hand first means the app can adopt them during startup).
/// The socket is kept open: the ACK is only sent once every pane is a live `PtyTerm`.
pub(crate) struct Incoming {
    sock: UnixStream,
    session: SavedSession,
    panes: Vec<(PaneMeta, OwnedFd)>,
}

impl Incoming {
    /// Connect to a predecessor's handoff socket and read the header + every pane fd.
    pub(crate) fn receive(path: &Path) -> std::io::Result<Self> {
        let sock = connect(path)?;
        let (session, panes) = recv_session(&sock)?;
        Ok(Self { sock, session, panes })
    }

    pub(crate) fn session(&self) -> &SavedSession {
        &self.session
    }

    /// The received panes in leaf order across tabs. Taken (not borrowed) because each `OwnedFd`
    /// moves into a `PtyTerm`.
    pub(crate) fn take_panes(&mut self) -> Vec<(PaneMeta, OwnedFd)> {
        std::mem::take(&mut self.panes)
    }

    /// Tell the predecessor it may let go. MUST be called only after every pane is live: this is
    /// what makes it stop guarding the shells, and nothing can be undone afterwards.
    pub(crate) fn ack(&self) -> std::io::Result<()> {
        send_text(&self.sock, ACK)
    }
}

/// Is a handoff even possible? Only from inside an `.app` bundle: `open` on the bundle is the
/// verified way to get a window, so a dev build (`cargo run`) has no successor to hand anything to
/// and must keep the honest "this terminates your shells" restart.
pub(crate) fn available() -> bool {
    std::env::current_exe().ok().as_deref().and_then(crate::update::bundle_path).is_some()
        && sock_path().is_some()
}

/// What a successor should do when the handoff it was launched for did NOT complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// A live primary still owns the session (it never got our ACK, so it kept every shell):
    /// a second window would only fight it for the instance socket. Exit and let it explain.
    ExitQuietly,
    /// Nobody is running: never leave the user staring at nothing after clicking Restart - open a
    /// normal window with the ordinary session restore.
    FreshWindow,
}

pub(crate) fn fallback(primary_alive: bool) -> Fallback {
    if primary_alive { Fallback::ExitQuietly } else { Fallback::FreshWindow }
}

/// Pane messages a header must be followed by: one per leaf across every tab (a tab saved before
/// split-restore has no tree and means a single pane).
pub(crate) fn expected_panes(session: &SavedSession) -> usize {
    session
        .tabs
        .iter()
        .map(|t| t.pane.as_ref().map_or(1, crate::session::SavedPane::leaf_count))
        .sum()
}

/// `alive_secs` off the wire as a `Duration`. Negative / NaN / absurd values collapse to zero
/// rather than panicking (`Duration::from_secs_f32` panics on both) - it only feeds the crash-loop
/// guard, so "assume freshly started" is the safe reading.
pub(crate) fn alive_duration(secs: f32) -> Duration {
    Duration::try_from_secs_f32(secs).unwrap_or(Duration::ZERO)
}

/// Grid geometry off the wire, clamped: a pty is at least 1x1, and a hostile `cols` would otherwise
/// have `Term::new` allocate a grid of that width.
pub(crate) fn grid_dims(cols: usize, rows: usize) -> (usize, usize) {
    const MAX_DIM: usize = 4096;
    (cols.clamp(1, MAX_DIM), rows.clamp(1, MAX_DIM))
}

/// Where the handoff socket lives: the runtime dir when set, else the config dir (per-user via
/// `$HOME`, never a world-writable /tmp path - same rule as the single-instance socket). The pid
/// keeps two overlapping restarts from colliding and makes a leftover file obviously stale.
pub(crate) fn sock_path() -> Option<PathBuf> {
    let name = format!("stdusk-handoff-{}.sock", std::process::id());
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(dir).join(name));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/stdusk").join(name))
}

/// Bind the handoff socket (creating its dir, clearing a stale file first).
pub(crate) fn listen(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
}

/// Wait for the successor to connect, giving up after `timeout`. Polled rather than blocking so the
/// give-up is real: a successor that never launches (or is an older build that ignores `--adopt`)
/// must not park us forever.
pub(crate) fn accept_within(
    listener: &UnixListener,
    timeout: Duration,
) -> std::io::Result<UnixStream> {
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
/// order across tabs - see [`expected_panes`]), then WAIT for the successor's ACK.
///
/// `Ok(())` therefore means "the successor has every pane live and confirmed it". Every other
/// outcome means nothing was handed over: the caller still owns its shells and must keep them.
pub(crate) fn send_session(
    sock: &UnixStream,
    session: &SavedSession,
    panes: &[(PaneMeta, BorrowedFd<'_>)],
) -> std::io::Result<()> {
    if panes.len() != expected_panes(session) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pane count does not match the session layout",
        ));
    }
    let header = Header { version: PROTOCOL, panes: panes.len(), session: session.clone() };
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
pub(crate) fn recv_session(
    sock: &UnixStream,
) -> std::io::Result<(SavedSession, Vec<(PaneMeta, OwnedFd)>)> {
    let header: Header = decode(&recv_text(sock)?)?;
    if header.version != PROTOCOL {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("handoff protocol {} != ours {PROTOCOL}", header.version),
        ));
    }
    if header.panes > MAX_PANES || header.panes != expected_panes(&header.session) {
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

/// TOML for a wire struct (`toml` is already a dependency for the config/session files; pulling in
/// serde_json just for this would be a new dep for the same job).
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

/// Send one pane: its `meta` JSON plus the live pty master `fd`.
pub(crate) fn send_pane(sock: &UnixStream, meta: &str, fd: BorrowedFd<'_>) -> std::io::Result<()> {
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
pub(crate) fn recv_pane(sock: &UnixStream) -> std::io::Result<(String, OwnedFd)> {
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

/// Launch the successor. `open` (not the inner binary) because launchd is what actually yields a
/// window, `-n` because we are still running and would otherwise just activate ourselves, and
/// `--args` to hand it the socket. A `--state-dir` dev instance forwards its isolation so the
/// successor keeps reading the same throwaway config instead of the real install's.
fn spawn_successor(app: &Path, sock: &Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-n").arg("-a").arg(app).arg("--args").arg("--adopt").arg(sock);
    if let Some(dir) = state_dir_arg() {
        cmd.arg("--state-dir").arg(dir);
    }
    let status = cmd.status()?;
    if status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(format!("open exited with {status}")))
}

/// This process's `--state-dir DIR`, if it was launched isolated.
fn state_dir_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--state-dir").and_then(|i| args.get(i + 1).cloned())
}

impl crate::Stdusk {
    /// Hand this window's LIVE panes to a freshly-launched stdusk.
    ///
    /// `Ok(())` means the successor confirmed every pane is live AND our panes are disarmed: the
    /// caller must then close the window WITHOUT killing anything, and must do nothing else - our
    /// reader threads steal pty bytes from the successor until the process exits.
    ///
    /// `Err(reason)` means NOTHING moved. The shells are still ours, still armed; the caller keeps
    /// the window up. A failed handoff has to look like the restart never happened.
    pub(crate) fn hand_off(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("no exe path ({e})"))?;
        let app = crate::update::bundle_path(&exe).ok_or("not running from an .app bundle")?;
        let path = sock_path().ok_or("no runtime dir for the handoff socket")?;
        let listener =
            listen(&path).map_err(|e| format!("cannot bind the handoff socket ({e})"))?;
        // Snapshot BEFORE the successor launches: same shape the session file gets, so the new
        // window rebuilds the identical layout and just adopts instead of spawning.
        let session = self.session_snapshot(ctx);
        spawn_successor(&app, &path).map_err(|e| format!("cannot launch the successor ({e})"))?;
        let sock = accept_within(&listener, LAUNCH_TIMEOUT).map_err(|e| e.to_string());
        let result = sock.and_then(|sock| {
            // `panes` borrows the pane trees (each entry holds a BorrowedFd), so it must be gone
            // before the disarm below can take `&mut self`.
            let panes = self.handoff_panes()?;
            send_session(&sock, &session, &panes).map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_file(&path);
        result?;
        // Confirmed: the successor owns these shells now. Disarm kill()/Drop for every pane, and
        // remember that our single-instance socket went with them.
        for tab in &mut self.tabs {
            for term in tab.root_mut().leaves_mut() {
                term.mark_handed_off();
            }
        }
        GAVE_SOCKET.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Every pane's master fd + metadata, in leaf order across tabs (`Pane::leaves` is A-before-B,
    /// the same order `SavedPane::from_tree` writes the header's trees in). A pane whose fd cannot
    /// be borrowed aborts the whole handoff: half a session is worse than no handoff at all.
    fn handoff_panes(&self) -> Result<Vec<(PaneMeta, BorrowedFd<'_>)>, String> {
        let mut panes = Vec::new();
        for tab in &self.tabs {
            for term in tab.root().leaves() {
                let (fd, pgid) = term.handoff_fd().ok_or("a pane's pty fd is not borrowable")?;
                panes.push((
                    PaneMeta {
                        pgid,
                        cwd: term.cwd(),
                        alive_secs: term.alive().as_secs_f32(),
                        cols: term.cols(),
                        rows: term.rows(),
                    },
                    fd,
                ));
            }
        }
        Ok(panes)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    use super::*;

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

    // --- protocol -----------------------------------------------------------------------------

    use crate::session::{SavedPane, SavedTab};

    /// A two-tab session: tab 0 is a Row split (two leaves), tab 1 a single pane. Three leaves in
    /// A-before-B order across tabs - the order the fds must pair with.
    fn three_leaf_session() -> SavedSession {
        SavedSession {
            tabs: vec![
                SavedTab {
                    title: Some("build".into()),
                    color: Some("#e06c75".into()),
                    cwd: Some("/left".into()),
                    pinned: true,
                    pane: Some(SavedPane::Split {
                        dir: crate::session::SavedSplitDir::Row,
                        ratio: 0.5,
                        a: Box::new(SavedPane::Leaf { cwd: Some("/left".into()) }),
                        b: Box::new(SavedPane::Leaf { cwd: Some("/right".into()) }),
                    }),
                },
                SavedTab {
                    cwd: Some("/solo".into()),
                    pane: Some(SavedPane::Leaf { cwd: Some("/solo".into()) }),
                    ..Default::default()
                },
            ],
            active: 1,
            window: None,
        }
    }

    fn meta(n: usize) -> PaneMeta {
        PaneMeta {
            pgid: Some(1000 + n as u32),
            cwd: Some(format!("/pane{n}")),
            alive_secs: 12.5,
            cols: 80 + n,
            rows: 24,
        }
    }

    #[test]
    fn header_round_trips_through_toml() {
        // The header nests the whole SavedSession (arrays of tables inside a table), so the encoding
        // has to survive that - not just the flat scalars.
        let h = Header { version: PROTOCOL, panes: 3, session: three_leaf_session() };
        let back: Header = decode(&encode(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        let m = meta(2);
        assert_eq!(decode::<PaneMeta>(&encode(&m).unwrap()).unwrap(), m);
    }

    #[test]
    fn expected_panes_counts_leaves_across_tabs() {
        assert_eq!(expected_panes(&three_leaf_session()), 3);
        assert_eq!(expected_panes(&SavedSession::default()), 0);
        // A tab saved before split-restore has no tree and means exactly one pane.
        let old = SavedSession {
            tabs: vec![SavedTab { cwd: Some("/tmp".into()), ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(expected_panes(&old), 1);
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
        let h = Header { version: PROTOCOL + 1, panes: 0, session: SavedSession::default() };
        send_text(&a, &encode(&h).unwrap()).unwrap();
        let err = recv_session(&b).expect_err("a version mismatch must abort");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("protocol"), "got {err}");
    }

    #[test]
    fn a_header_claiming_more_panes_than_the_layout_is_refused() {
        let (a, b) = UnixStream::pair().unwrap();
        let h = Header { version: PROTOCOL, panes: 9, session: three_leaf_session() };
        send_text(&a, &encode(&h).unwrap()).unwrap();
        assert!(recv_session(&b).is_err());
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

        let (got_session, got_panes) = recv_session(&b).unwrap();
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
        let session = SavedSession {
            tabs: vec![SavedTab {
                pane: Some(SavedPane::Leaf { cwd: None }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let sender = std::thread::spawn(move || {
            let panes = [(meta(0), rx.as_fd())];
            send_session(&a, &session, &panes)
        });
        let received = recv_session(&b);
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

    // --- LIVE checks (ignored: they run the real binary and open a real window) ----------------
    //
    // No headless harness can cover the receiving half - it only exists in a second PROCESS with a
    // window. Both tests below act as the predecessor (real pty, real socket, real protocol) and
    // let the real binary be the successor; the ACK is the assertion, since only a successor that
    // adopted every pane and built its tabs sends it. Run them after a build:
    //
    //     cargo build && cargo test -- --ignored --nocapture real_
    //
    // Both launch the successor with `--state-dir`, so they can never touch the user's config,
    // session, or single-instance socket.

    /// The cwd the live checks hand over: a real directory, so the successor's own session persist
    /// echoes it back only if the adopted pane really knows where it is.
    const LIVE_CWD: &str = "/usr/share/dict";

    /// The predecessor half of a live check: a real shell on a real pty, the socket bound, the
    /// successor started by `launch`, then the full exchange. `Ok(())` means it acknowledged.
    fn live_handoff(sock_file: &Path, launch: impl FnOnce(&Path)) -> std::io::Result<()> {
        let listener = listen(sock_file)?;
        let opts = crate::terminal::SpawnOpts {
            detect_progress: false,
            shell_integration: false,
            autosuggestions: false,
            scrollback_lines: 200,
            word_separators: " ".into(),
            bold_bright: false,
            cwd: None,
            profile: Some(crate::config::Profile {
                name: "live".into(),
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), "while :; do printf 'tick '; sleep 1; done".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                color: None,
            }),
        };
        // Dropping this at the end of the test kills the shell again - we never mark it handed off,
        // because the successor is about to be killed too.
        let term = crate::terminal::PtyTerm::spawn(80, 24, egui::Context::default(), &opts);
        launch(sock_file);
        let sock = accept_within(&listener, Duration::from_secs(30))?;
        let session = SavedSession {
            tabs: vec![SavedTab {
                title: Some("live".into()),
                pane: Some(SavedPane::Leaf { cwd: Some(LIVE_CWD.into()) }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (fd, pgid) = term.handoff_fd().expect("master fd");
        let panes = [(
            PaneMeta {
                pgid,
                cwd: Some(LIVE_CWD.into()),
                alive_secs: term.alive().as_secs_f32(),
                cols: term.cols(),
                rows: term.rows(),
            },
            fd,
        )];
        send_session(&sock, &session, &panes)
    }

    /// Poll the successor's OWN session file (`--state-dir` puts it under DIR) for the first tab's
    /// cwd. It is written from the live `PtyTerm::cwd()`, so it is the only externally observable
    /// proof that an adopted pane knows its directory - which is what the tab's name is derived from.
    fn adopted_session_cwd(dir: &Path) -> Option<String> {
        let file = dir.join(".config/stdusk/session.toml");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(body) = std::fs::read_to_string(&file)
                && let Ok(s) = toml::from_str::<SavedSession>(&body)
                && let Some(cwd) = s.tabs.first().and_then(|t| t.cwd.clone())
            {
                return Some(cwd);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The receiving half against the real binary: `--adopt` parsing, `Incoming::receive`, the
    /// adoption inside `Stdusk::new`, and the ACK that releases the shells. Also the user-visible
    /// half of it: an adopted pane must know its cwd immediately (the shell re-emits OSC 7 only at
    /// its next prompt, and the tab's auto-title is that cwd's basename).
    #[test]
    #[ignore = "launches the real binary and opens a window; run manually after cargo build"]
    fn real_successor_adopts_a_live_shell_and_acknowledges() {
        let dir = std::env::temp_dir().join(format!("stdusk-live-adopt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/stdusk");
        assert!(exe.exists(), "run `cargo build` first: {exe:?}");
        let mut child = None;
        let handed = live_handoff(&dir.join("handoff.sock"), |sock| {
            child = std::process::Command::new(&exe)
                .arg("--adopt")
                .arg(sock)
                .arg("--state-dir")
                .arg(&dir)
                .spawn()
                .ok();
        });
        let cwd = handed.is_ok().then(|| adopted_session_cwd(&dir)).flatten();
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&dir);
        handed.expect("the successor must acknowledge a live adoption");
        assert_eq!(cwd.as_deref(), Some(LIVE_CWD), "the adopted pane must know its cwd at once");
    }

    /// The LAUNCH line, end to end: a real `.app` bundle started the way `spawn_successor` starts it
    /// (`open -n -a <bundle> --args ...`) while we are still running. Two load-bearing assumptions
    /// live here and both have burned this repo: `open` on the BUNDLE is what yields a window (not
    /// exec'ing the inner binary), and `--args` really does reach the app's argv.
    #[test]
    #[ignore = "launches a real .app through launchd; run manually after cargo build"]
    fn real_bundle_launch_reaches_the_successors_argv() {
        let dir = std::env::temp_dir().join(format!("stdusk-live-bundle-{}", std::process::id()));
        let macos = dir.join("stdusk.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/stdusk");
        assert!(exe.exists(), "run `cargo build` first: {exe:?}");
        std::fs::copy(&exe, macos.join("stdusk")).unwrap();
        std::fs::write(
            dir.join("stdusk.app/Contents/Info.plist"),
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>",
                "<key>CFBundleName</key><string>stdusk</string>",
                "<key>CFBundleExecutable</key><string>stdusk</string>",
                "<key>CFBundleIdentifier</key><string>dev.stdusk.livetest</string>",
                "<key>CFBundleShortVersionString</key><string>0.0.0</string>",
                "</dict></plist>",
            ),
        )
        .unwrap();
        let bundle = dir.join("stdusk.app");

        // Mirrors `spawn_successor`, plus the `--state-dir` isolation it would have forwarded from
        // a dev instance's own argv (a test harness cannot carry that flag).
        let handed = live_handoff(&dir.join("handoff.sock"), |sock| {
            let ok = std::process::Command::new("open")
                .arg("-n")
                .arg("-a")
                .arg(&bundle)
                .arg("--args")
                .arg("--adopt")
                .arg(sock)
                .arg("--state-dir")
                .arg(&dir)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "open must launch the bundle");
        });
        // launchd owns the process, so it is killed by the argv `open` handed it.
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(bundle.to_string_lossy().to_string())
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        handed.expect("a bundle launched through open must adopt and acknowledge");
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
}
