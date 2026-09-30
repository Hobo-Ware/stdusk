//! Real terminal. portable-pty spawns the shell; a reader thread feeds bytes through the vte
//! ANSI parser into a shared alacritty_terminal `Term`. The egui thread reads the grid to
//! render, resizes, scrolls, and writes keystrokes/paste back to the pty.
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, Processor, Rgb};
use base64::Engine;
use eframe::egui::Color32;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::agent_pane::{AgentPane, AgentSetup, Fallback, PendingInput};
use crate::agent_restore::{PaneNotice, SkipReason};
use crate::agent_track::Status;
use crate::agents::{AgentSession, Seen};
use crate::colors;
use crate::mouse::MouseReporting;
use crate::osc::{OscEvent, OscScanner, ShellEvent};
use crate::progress::{Progress, ProgressScanner};

/// Last-command state from OSC 133 shell integration, shown as the tab's left-edge indicator.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum CmdState {
    #[default]
    Idle, // no command run / cancelled (no indicator)
    Running,
    Ok,
    Fail,
}

/// Map an OSC 133 exit code to a tab state. A signal termination (128+n, e.g. 130 = Ctrl+C /
/// SIGINT, 143 = SIGTERM) means the user cancelled - that's not an error, so clear the indicator
/// instead of flagging red.
pub(crate) fn cmd_from_exit(code: Option<i32>) -> CmdState {
    match code.unwrap_or(0) {
        0 => CmdState::Ok,
        129..=159 => CmdState::Idle,
        _ => CmdState::Fail,
    }
}

/// The dead shell's exit report, set once by the reader thread when the pty EOFs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ExitInfo {
    pub(crate) code: i32,
    pub(crate) uptime_secs: f32, // spawn -> exit; feeds the crash-loop guard
}

/// `terminal.on_exit` parsed: what happens to a pane when its shell exits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OnExit {
    Close,
    Keep,
    Restart,
}

pub(crate) fn on_exit_mode(s: &str) -> OnExit {
    match s.to_ascii_lowercase().as_str() {
        "keep" => OnExit::Keep,
        "restart" => OnExit::Restart,
        _ => OnExit::Close, // default
    }
}

/// An exit within this many seconds of spawn counts as a crash (restart-mode loop guard).
pub(crate) const RAPID_EXIT_SECS: f32 = 2.0;

/// Coalesce output-driven repaints: the reader `read()`s the pty in tiny pieces (the macOS pty
/// hands back <=1KB chunks, so a TUI's single-write clear+redraw frame arrives as ~28 reads in
/// ~150us - measured). Requesting an immediate repaint per read lets the UI snapshot a grid
/// mid-burst (blank right after the ESC[2J clear, or half-repainted) - the arrow-key nav flicker.
/// Deferring the repaint by a sub-frame window collapses the whole burst into one paint of the
/// settled grid; a long stream still paints every window (progressive, not stalled).
const REPAINT_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(4);

/// How long the frame that sent a keystroke waits for its echo before painting. Without it that
/// frame presents the pre-echo grid and the echo frame queues 1-3 vsyncs behind it (see LEDGER).
/// Shells echo in ~0.1-1.5 ms; only keys with no output at all pay the full bound.
pub(crate) const ECHO_WAIT: std::time::Duration = std::time::Duration::from_millis(2);

/// What the UI applies to an exited pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExitAction {
    ClosePane,
    Keep,
    Restart,
}

/// Decide the action for an exited pane. `rapid_exits` counts consecutive deaths within
/// `RAPID_EXIT_SECS` of spawn; a restart-mode shell that dies rapidly twice in a row is
/// crash-looping, so it falls back to Keep instead of respawning forever.
pub(crate) fn exit_action(mode: OnExit, uptime_secs: f32, rapid_exits: u32) -> ExitAction {
    match mode {
        OnExit::Close => ExitAction::ClosePane,
        OnExit::Keep => ExitAction::Keep,
        OnExit::Restart => {
            if uptime_secs < RAPID_EXIT_SECS && rapid_exits >= 1 {
                ExitAction::Keep
            } else {
                ExitAction::Restart
            }
        }
    }
}

/// One styled cell for the renderer. `bg == None` means the terminal default (transparent).
/// `wide` marks a double-width glyph (CJK/emoji) - the renderer draws it across two cells; the
/// spacer cell that follows carries `c == '\0'` so no glyph is drawn there (bg/selection stay).
/// `bold` is the raw SGR BOLD flag - the renderer switches to the real bold face when one is
/// registered; independent of the `bold_bright` color treatment. `dim` is SGR 2 (faint): the
/// renderer blends the text toward the background, which is how TUIs draw hint/ghost text.
#[allow(clippy::struct_excessive_bools)] // independent SGR attributes, not a mode
pub(crate) struct CellSnap {
    pub(crate) c: char,
    pub(crate) fg: Color32,
    pub(crate) bg: Option<Color32>,
    pub(crate) selected: bool,
    pub(crate) wide: bool,
    pub(crate) bold: bool,
    pub(crate) dim: bool,
}

/// Map a grid cell's char + flags to what the renderer draws: spacer cells (the second column
/// of a wide char, incl. the leading spacer before a line-wrapped one) emit no glyph ('\0');
/// a `WIDE_CHAR` cell keeps its glyph and is marked wide so it can span two cells.
pub(crate) fn snap_glyph(c: char, flags: Flags) -> (char, bool) {
    if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        ('\0', false)
    } else {
        (c, flags.contains(Flags::WIDE_CHAR))
    }
}

/// A frame's worth of visible grid, ready to paint.
pub(crate) struct GridSnap {
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    pub(crate) cells: Vec<CellSnap>,           // row-major, rows*cols
    pub(crate) cursor: Option<(usize, usize)>, // (row, col); None while scrolled into history
    pub(crate) top_line: i32, // buffer line of viewport row 0 (for mouse->grid mapping)
}

/// Per-tab observable state, written by the reader thread, read by the UI.
#[allow(clippy::struct_excessive_bools)] // independent per-pane flags, not a mode
#[derive(Default)]
pub(crate) struct TabState {
    pub(crate) progress: Progress,
    pub(crate) cwd: Option<String>,
    pub(crate) clipboard: Option<String>, // OSC 52 copy request, consumed by the UI thread
    pub(crate) cmd: CmdState,             // OSC 133 last-command state (tab dot)
    pub(crate) bell: bool,                // BEL rung since last consumed (drives the visual flash)
    pub(crate) activity: bool,            // output since last consumed (notify-on-activity)
    /// Any pty output at all since this `PtyTerm` was built - never cleared, unlike `activity`.
    /// The adopt redraw nudger's stop condition: an adopted pane starts with an empty grid, and
    /// this is what says the shell has actually painted something into it.
    pub(crate) saw_output: bool,
    /// Output chunks applied to the grid so far (never reset). Typed input waits on it for the echo.
    pub(crate) output_gen: u64,
    /// The first status stdusk's own shell hook reported (OSC 133 `D` with the own mark) since the
    /// UI last took one. Only the first after the agent decides its record, so later ones are not
    /// kept.
    pub(crate) first_status: Option<Status>,
    pub(crate) done_notify: Option<i32>, // a long command just finished (exit code); UI consumes it
    pub(crate) exited: Option<ExitInfo>, // the shell exited (pty EOF + reaped); UI applies on_exit
    pub(crate) title_osc: Option<String>, // OSC 0/2 window title (None = unset / reset)
    pub(crate) theme_reports: bool,
}

/// Grid sizing. History (scrollback) comes from `Config::scrolling_history`, not here.
struct Dims {
    cols: usize,
    rows: usize,
}
impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A pty-bound answer to a terminal query: DA/DSR/DECRQM reports (`PtyWrite`) and OSC 4/10/11/12
/// color reads (`ColorRequest`). `send_event` fires inside the term lock (mid-`advance`), so
/// answers are queued here and written after the parse pass - no blocking IO under the grid lock.
enum Reply {
    Text(String),
    Color(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
}

/// Event sink for the alacritty `Term`, fired from the reader thread mid-`advance`:
/// - `Bell` -> flag for the UI flash.
/// - `Title`/`ResetTitle` -> the tab title. This is the only path that sees the xterm title
///   STACK (`CSI 22/23 t`): copilot sets its title via OSC 0 but restores it via a stack pop,
///   which the OSC scanner can't see - dropping these left "GitHub Copilot" stuck on the tab.
/// - `PtyWrite`/`ColorRequest` -> queued query answers. Unanswered queries are how TUI CLIs
///   mis-detect the theme (gemini assumes a dark bg when OSC 11 stays silent) or stall on
///   DA/DSR probes.
#[derive(Clone)]
struct EventProxy {
    state: Arc<Mutex<TabState>>,
    replies: Arc<Mutex<Vec<Reply>>>,
}
impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Bell => {
                if let Ok(mut s) = self.state.lock() {
                    s.bell = true;
                }
            }
            Event::Title(t) => {
                if let Ok(mut s) = self.state.lock() {
                    s.title_osc = (!t.is_empty()).then_some(t);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut s) = self.state.lock() {
                    s.title_osc = None;
                }
            }
            Event::PtyWrite(t) => {
                if let Ok(mut r) = self.replies.lock() {
                    r.push(Reply::Text(t));
                }
            }
            Event::ColorRequest(index, format) => {
                if let Ok(mut r) = self.replies.lock() {
                    r.push(Reply::Color(index, format));
                }
            }
            _ => {}
        }
    }
}

/// Everything a terminal spawn needs from the user config (one bag instead of a positional list).
#[allow(clippy::struct_excessive_bools)] // independent config toggles, not a mode
#[derive(Clone)]
pub(crate) struct SpawnOpts {
    pub(crate) detect_progress: bool,
    pub(crate) shell_integration: bool,
    pub(crate) autosuggestions: bool,
    pub(crate) scrollback_lines: usize,
    pub(crate) word_separators: String,
    pub(crate) bold_bright: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) profile: Option<crate::config::Profile>, // launch profile overrides (shell/args/cwd/env)
    /// Agent-resume wiring for this pane. `None` = the pane tracks no agent session.
    pub(crate) agent_tracking: Option<AgentSetup>,
}

/// When an adopted pane re-asks for a repaint, counting from adoption. The first ask is immediate;
/// the rest cover the window in which the predecessor's reader thread is still draining this pty and
/// eating the answer (it exits right after our ACK). Each retry is skipped once output has landed.
const REDRAW_RETRY_DELAYS: [std::time::Duration; 4] = [
    std::time::Duration::ZERO,
    std::time::Duration::from_millis(250),
    std::time::Duration::from_millis(700),
    std::time::Duration::from_millis(1500),
];

/// The row count to bounce off so putting the real one back is a genuine CHANGE - which is the only
/// thing that makes a pty deliver SIGWINCH. One row less, except on a 1-row pty (0 would mean
/// "unset").
fn wiggle_rows(rows: usize) -> usize {
    if rows > 1 { rows - 1 } else { rows + 1 }
}

/// May this redraw attempt send `^L` (0x0c) into an adopted pane?
///
/// `^L` is only ever CONSUMED by a shell sitting at its prompt - zsh's zle and bash's readline
/// redraw the prompt on it. Everywhere else it is damage:
/// - on the ALT screen the byte belongs to the app (a literal insert in vim's insert mode);
/// - with a command RUNNING and not reading stdin, the tty line discipline ECHOES it as a literal
///   `^L` and parks the byte in the input queue. That was the 1.6.2 bug: a pane running a progress
///   loop came back showing `^L` and nothing else. Such a pane needs nothing from us anyway - its
///   own next output tick repaints it.
///
/// `running: None` means nobody could tell (a 1.6.2 predecessor did not send the field, the shell
/// emits no OSC 133, and the tty could not be asked). Then the FIRST attempt stays silent and only a
/// pane that produced no output during the grace period is treated as an idle prompt - the retry loop
/// bails out as soon as `saw_output` flips, so a producing pane is never typed into.
fn allow_ctrl_l(alt_screen: bool, running: Option<bool>, attempt: usize, replayed: bool) -> bool {
    // A replayed pane already shows its screen - and zle's clear-screen would WIPE it to redraw a
    // bare prompt, throwing away the very content the replay restored.
    if alt_screen || replayed {
        return false;
    }
    match running {
        Some(running) => !running,
        None => attempt > 0,
    }
}

/// Is a foreground command running on this pty, as the TTY sees it? With job control a foreground
/// job gets its own process group (`tcsetpgrp`), so the tty's foreground group is the shell's own
/// group ONLY when the shell is at its prompt. `None` when it cannot be told: no known pgid, or the
/// fd is not a tty (a test pipe).
///
/// Verified on a real pty: a pane running `sleep` reports a foreground group that is not the shell's,
/// a pane blocked in a builtin `read` reports the shell's own
/// (`real_pty_the_tty_reports_whether_a_command_is_running`).
fn foreground_command(fd: std::os::fd::BorrowedFd<'_>, shell_pgid: Option<u32>) -> Option<bool> {
    Some(foreground_pgid(fd)? != shell_pgid?)
}

/// The process group that owns the tty's foreground. `None` when the fd is not a tty.
fn foreground_pgid(fd: std::os::fd::BorrowedFd<'_>) -> Option<u32> {
    let fg = rustix::termios::tcgetpgrp(fd).ok()?;
    Some(fg.as_raw_nonzero().get().cast_unsigned())
}

/// Deliver a SIGWINCH to whatever is running on this pty, so a full-screen app repaints its screen.
/// Adopting a live shell gives us an EMPTY `Term` (the predecessor's grid cannot be moved), and this
/// is the only signal a TUI redraws on.
///
/// It has to WIGGLE the row count and put it back: a pty signals only when the size actually
/// CHANGES, so re-sending the current size - which the old "nudge" did - was silently nothing.
/// Verified on macOS with a real pty (`real_pty_only_a_size_change_delivers_sigwinch`); Linux's
/// `tty_do_resize` carries the same "don't signal if the size is unchanged" guard.
fn nudge_winsize(fd: std::os::fd::BorrowedFd<'_>, cols: usize, rows: usize) {
    set_winsize(fd, cols, wiggle_rows(rows));
    set_winsize(fd, cols, rows);
}

/// Push a window size to a pty master fd - the ioctl that delivers SIGWINCH to the tty's foreground
/// process group.
fn set_winsize(fd: std::os::fd::BorrowedFd<'_>, cols: usize, rows: usize) {
    let _ = rustix::termios::tcsetwinsize(
        fd,
        rustix::termios::Winsize {
            ws_row: rows as u16,
            ws_col: cols as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    );
}

/// Shell for a spawn: profile override first, then $SHELL, then /bin/zsh.
fn resolve_shell(profile: Option<&crate::config::Profile>, env_shell: Option<String>) -> String {
    profile.and_then(|p| p.shell.clone()).or(env_shell).unwrap_or_else(|| "/bin/zsh".into())
}

/// Working-dir candidate (tilde-expanded): the profile's cwd wins over the caller's.
/// `is_dir` validation stays at the spawn site.
fn resolve_cwd(
    profile: Option<&crate::config::Profile>,
    fallback: Option<String>,
) -> Option<String> {
    profile.and_then(|p| p.cwd.clone()).or(fallback).map(|d| crate::config::expand_tilde(&d))
}

pub(crate) struct PtyTerm {
    term: Arc<FairMutex<Term<EventProxy>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>, // shared with the reader thread (query replies)
    pty: Pty,                                  // owns the master fd; resize goes through it
    state: Arc<Mutex<TabState>>,
    cols: usize,
    rows: usize,
    shell_pid: Option<u32>, // for CLI-awareness process scanning (procwatch) + process-group kill
    bold_bright: bool,      // draw bold text in bright ANSI colors
    rapid_exits: u32,       // consecutive <RAPID_EXIT_SECS deaths, carried across respawns
    killed: bool,           // kill() ran (idempotency guard; Drop calls kill() too)
    /// May we reap this shell's session? A SPAWNED pane owns it outright. An ADOPTED pane does NOT
    /// until its ACK is delivered - until then the predecessor is still guarding those shells, and a
    /// successor that reaped them would kill the user's work while the old window says the restart
    /// was cancelled. A HANDED-OFF pane gives ownership away for good.
    owns_session: bool,
    started: std::time::Instant, // when THIS owner took the pty (spawn or adopt)
    uptime_base: std::time::Duration, // how long the shell had run under previous owners
    /// Liveness token for background helpers (the adopt redraw nudger). They hold a `Weak` to it, so
    /// dropping this pane stops them at once - and releases the pty fd they duplicated - instead of
    /// leaving them poking a pane the user already closed.
    alive_token: Arc<()>,
    /// The pane's agent state. `None` = agents off for this pane.
    agent: Option<AgentPane>,
    /// What restore says about this pane, drawn over it until the user acts in it. Never saved.
    notice: Option<PaneNotice>,
}

/// A live pty handed over by a predecessor stdusk, plus everything a freshly built `Term` cannot
/// deduce about it. Bundled rather than passed as seven arguments, and because every field is
/// untrusted wire data the caller has already clamped.
pub(crate) struct Adopted {
    pub(crate) fd: std::os::fd::OwnedFd,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
    /// Process GROUP to kill later: we are not the shell's parent, so we cannot look it up.
    pub(crate) pgid: Option<u32>,
    /// How long the shell had already been running, so the crash-loop guard doesn't read a
    /// long-lived shell as freshly spawned.
    pub(crate) alive: std::time::Duration,
    /// Was a full-screen app (vim / less / claude) holding the ALT screen at handover? Decides how
    /// the pane is asked to repaint - see [`PtyTerm::request_redraw`]. Our own `Term` starts blank
    /// and has parsed nothing, so this can only come from the predecessor.
    pub(crate) alt_screen: bool,
    /// Was a command RUNNING at handover (OSC 133), rather than the shell sitting at its prompt?
    /// `None` = nobody knows - a predecessor too old to send it, or a shell that emits no OSC 133 at
    /// all. See [`allow_ctrl_l`]: the answer decides whether the pane may be sent a `^L`.
    pub(crate) cmd_running: Option<bool>,
    /// The donor's screen as ANSI (see `screen`), replayed into our fresh grid so the pane shows what
    /// was already there. Empty when the predecessor sent none - a 1.6.2 build, or a blank pane.
    pub(crate) replay: Vec<u8>,
    /// The live OSC 0/2 window title, if the app had set one. Like `cwd` this cannot be re-derived:
    /// the app re-emits its title only when something changes, so without it a Claude/vim pane falls
    /// back to its cwd basename for the rest of the session.
    pub(crate) title_osc: Option<String>,
    /// Had the app enabled theme reports (mode 2031)? It enables them once at startup, so an adopted
    /// pane that forgot would never be told about a theme change again.
    pub(crate) theme_reports: bool,
}

/// How this pane's pty master is owned. A spawned pane keeps `portable_pty`'s master (its proven
/// resize path); an ADOPTED pane only ever had a bare fd handed to it over a socket, so it resizes
/// with `tcsetwinsize` directly. Both are just a pty master fd underneath.
enum Pty {
    Spawned(Box<dyn MasterPty + Send>),
    Adopted(std::os::fd::OwnedFd),
}

impl Pty {
    /// The master fd, for handing this pane to a successor process. `portable_pty` only exposes a
    /// bare `RawFd`, and there is no safe way to type one - `BorrowedFd::borrow_raw` is the single
    /// conversion available, hence the local allow (same justification as the killpg FFI below).
    #[allow(unsafe_code)]
    fn as_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        use std::os::fd::{AsFd, BorrowedFd};
        match self {
            // SAFETY: portable-pty owns this fd for as long as the master lives, and the returned
            // BorrowedFd is bound to `&self`, so it cannot outlive that owner.
            Self::Spawned(m) => m.as_raw_fd().map(|raw| unsafe { BorrowedFd::borrow_raw(raw) }),
            Self::Adopted(fd) => Some(fd.as_fd()),
        }
    }
}

/// Everything the reader thread owns. Bundled because it is built from two very different places:
/// a fresh spawn (with a `Child` to reap) and an ADOPTED pty handed over by a predecessor process
/// (no child - we are not its parent, so EOF is the only exit signal we get).
struct ReaderCtx {
    reader: Box<dyn Read + Send>,
    term: Arc<FairMutex<Term<EventProxy>>>,
    state: Arc<Mutex<TabState>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    replies: Arc<Mutex<Vec<Reply>>>,
    ctx: egui::Context,
    detect_progress: bool,
    /// `None` for an adopted pty: a non-child cannot be `wait()`ed, so its exit code is unknown
    /// (-1, the value a failed wait already yields) and EOF is the only signal that it died.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// How long this shell had already been alive when we adopted it, so `uptime_secs` (and the
    /// crash-loop guard reading it) stays honest across a handoff instead of resetting to zero.
    uptime_base: std::time::Duration,
    /// Typed at the first OSC 133 prompt mark (`ShellEvent::PromptStart`), if still waiting.
    pending: PendingInput,
    /// Only the pane shell may receive queued input after a prompt mark.
    prompt_owner: Option<(std::os::fd::OwnedFd, u32)>,
}

/// The pty pump: parse output into the `Term`, answer queries, track progress/OSC state, and on
/// EOF record the exit. Shared verbatim by `spawn` and `adopt` - the ONLY difference between them
/// is whether there is a child to reap.
fn spawn_reader(c: ReaderCtx) {
    let ReaderCtx {
        mut reader,
        term: term_reader,
        state: state_reader,
        writer: writer_reader,
        replies,
        ctx,
        detect_progress,
        mut child,
        uptime_base,
        pending,
        prompt_owner,
    } = c;
    thread::spawn(move || {
        let spawned = std::time::Instant::now(); // for ExitInfo.uptime_secs
        let mut parser: Processor = Processor::new();
        let mut prog = ProgressScanner::new(detect_progress);
        let mut osc = OscScanner::new();
        let mut theme_reports = crate::modes::ThemeReportScanner::new();
        let mut buf = [0u8; 8192];
        let mut cmd_started: Option<std::time::Instant> = None; // for notify-when-done
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let osc_events = osc.feed(chunk);
                    let theme_toggle = theme_reports.feed(chunk);
                    let prompt_started = osc_events
                        .iter()
                        .any(|e| matches!(e, OscEvent::Shell(ShellEvent::PromptStart)));
                    // Advance the terminal; then, still under the lock: answer queued
                    // queries (color reads may need app-set OSC 4 overrides from
                    // `term.colors()`), heal leaked modes, read the alt-screen flag.
                    let (alt, reply, healed_alt) = {
                        let mut term = term_reader.lock();
                        parser.advance(&mut *term, chunk);
                        let mut reply = Vec::new();
                        for r in replies.lock().unwrap().drain(..) {
                            match r {
                                Reply::Text(t) => reply.extend_from_slice(t.as_bytes()),
                                Reply::Color(i, format) => {
                                    // An app-set palette entry (OSC 4/10/11 set) wins;
                                    // otherwise report the live theme's color.
                                    let rgb = term.colors()[i].unwrap_or_else(|| {
                                        let c = colors::query_color(i);
                                        Rgb { r: c.r(), g: c.g(), b: c.b() }
                                    });
                                    reply.extend_from_slice(format(rgb).as_bytes());
                                }
                            }
                        }
                        // A TUI killed without cleanup (SIGKILL, crash) leaves the alt
                        // screen + a hidden cursor behind and the pane looks frozen. The
                        // prompt mark (OSC 133;A) proves the shell owns the pty again:
                        // reset exactly those two leaks. Deliberately NOT reset:
                        // bracketed paste (zsh arms it for its own prompt), DECCKM /
                        // kitty / modifyOtherKeys (`key_to_bytes` is a static table that
                        // never consults them), mouse modes (we send no reports).
                        let mut healed_alt = false;
                        if prompt_started {
                            if term.mode().contains(TermMode::ALT_SCREEN) {
                                term.swap_alt();
                                healed_alt = true;
                            }
                            if !term.mode().contains(TermMode::SHOW_CURSOR) {
                                term.set_private_mode(NamedPrivateMode::ShowCursor.into());
                            }
                        }
                        (term.mode().contains(TermMode::ALT_SCREEN), reply, healed_alt)
                    };
                    let text = String::from_utf8_lossy(chunk);
                    let mut progress = prog.feed(&text, alt);
                    let mut cwd_update = None;
                    let mut clip_update = None;
                    let mut cmd_update = None;
                    let mut notify = None; // Some(exit) when a long command just finished
                    let mut status = None; // the first own status in this chunk
                    for ev in osc_events {
                        match ev {
                            OscEvent::Progress(p) => progress = p, // OSC 9;4 wins over %-scrape
                            // Titles flow through the Term's Title/ResetTitle events
                            // (EventProxy), which also cover the CSI 22/23 t title stack.
                            OscEvent::Title(_) => {}
                            OscEvent::Cwd(c) => cwd_update = Some(c),
                            OscEvent::Clipboard(b64) => {
                                if let Ok(bytes) =
                                    base64::engine::general_purpose::STANDARD.decode(b64)
                                    && let Ok(s) = String::from_utf8(bytes)
                                {
                                    clip_update = Some(s);
                                }
                            }
                            OscEvent::Shell(s) => match s {
                                ShellEvent::CommandStart => {
                                    cmd_update = Some(CmdState::Running);
                                    cmd_started = Some(std::time::Instant::now());
                                }
                                ShellEvent::CommandEnd { code, own } => {
                                    cmd_update = Some(cmd_from_exit(code));
                                    // Another shell integration (iTerm2's) marks every prompt, so
                                    // only our own mark can say a command ran and how it ended.
                                    if own && let Some(code) = code {
                                        status.get_or_insert(Status {
                                            code,
                                            at: std::time::Instant::now(),
                                        });
                                    }
                                    // Flag a "done" notification only for long-running commands.
                                    // Notify only for commands that ran a while (a "long" job).
                                    if cmd_started.take().is_some_and(|t| {
                                        t.elapsed() >= std::time::Duration::from_secs(10)
                                    }) {
                                        notify = Some(code.unwrap_or(0));
                                    }
                                }
                                // PromptStart: keep the last result visible at the prompt.
                                ShellEvent::PromptStart => {}
                            },
                        }
                    }
                    {
                        let mut s = state_reader.lock().unwrap();
                        s.progress = progress;
                        s.activity = true; // any output chunk counts (notify-on-activity)
                        s.saw_output = true;
                        s.output_gen = s.output_gen.wrapping_add(1);
                        if prompt_started {
                            s.theme_reports = false;
                        }
                        if let Some(on) = theme_toggle {
                            s.theme_reports = on;
                        }
                        if let Some(c) = cwd_update {
                            s.cwd = Some(c);
                        }
                        if let Some(c) = clip_update {
                            s.clipboard = Some(c);
                        }
                        if let Some(c) = cmd_update {
                            s.cmd = c;
                        }
                        if let Some(code) = notify {
                            s.done_notify = Some(code);
                        }
                        if let Some(status) = status {
                            s.first_status.get_or_insert(status);
                        }
                    }
                    // Query answers go straight back to the pty; after an alt-screen
                    // heal, a Ctrl-L asks the shell to repaint the prompt it may have
                    // drawn on the (now abandoned) alt grid.
                    if !reply.is_empty() || healed_alt {
                        let mut w = writer_reader.lock().unwrap();
                        let _ = w.write_all(&reply);
                        if healed_alt {
                            let _ = w.write_all(b"\x0c");
                        }
                        let _ = w.flush();
                    }
                    // A startup child can emit prompt marks too. Keep its input untouched.
                    if prompt_started
                        && prompt_owner
                            .as_ref()
                            .is_some_and(|(fd, pid)| foreground_pgid(fd.as_fd()) == Some(*pid))
                        && let Some(bytes) = pending.take()
                    {
                        let mut w = writer_reader.lock().unwrap();
                        let _ = w.write_all(&bytes);
                        let _ = w.flush();
                    }
                    // Defer (don't paint per read): coalesce the burst so a clear+redraw
                    // lands atomically before the UI snapshots. See REPAINT_COALESCE_WINDOW.
                    ctx.request_repaint_after(REPAINT_COALESCE_WINDOW);
                }
            }
        }
        // EOF/err: the shell's side of the pty closed, so the shell is gone. A spawned pane reaps
        // the real code; an adopted one has no child to wait on and reports -1.
        let code = child.as_mut().map_or(-1, |ch| ch.wait().map_or(-1, |st| st.exit_code() as i32));
        state_reader.lock().unwrap().exited =
            Some(ExitInfo { code, uptime_secs: (uptime_base + spawned.elapsed()).as_secs_f32() });
        ctx.request_repaint();
    });
}

impl PtyTerm {
    pub(crate) fn spawn(cols: usize, rows: usize, ctx: egui::Context, opts: &SpawnOpts) -> Self {
        let SpawnOpts {
            detect_progress,
            shell_integration,
            autosuggestions,
            cwd,
            profile,
            agent_tracking,
            ..
        } = opts.clone();
        let agent = agent_tracking.map(AgentPane::new);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let shell = resolve_shell(profile.as_ref(), std::env::var("SHELL").ok());
        let mut cmd = CommandBuilder::new(&shell);
        // Advertise color support like Tabby does (session.ts sets the same trio). Launched from
        // Finder there is NO inherited TERM, so chalk/supports-color-style detection in child
        // programs (Claude CLI etc.) silently disables ANSI colors without these.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "stdusk");
        // COLORFGBG advertises light/dark (fg;bg ANSI indices) for CLIs that read the env
        // instead of querying OSC 11 (vim, some node TUIs). Spawn-time snapshot; live OSC
        // queries always answer with the theme active at reply time.
        cmd.env("COLORFGBG", colors::colorfgbg());
        if let Some(dir) =
            resolve_cwd(profile.as_ref(), cwd).filter(|d| std::path::Path::new(d).is_dir())
        {
            cmd.cwd(dir);
        }
        // Spawn login+interactive (so PATH-setting profile files run) + optional OSC 133 hooks.
        let marks_prompts =
            crate::shell::configure(&mut cmd, &shell, shell_integration, autosuggestions);
        // A shell that marks its prompts releases the queued input itself. Only one that cannot
        // needs the fallback timer.
        let agent = agent.map(|a| a.shell_marks_prompts(marks_prompts));
        if let Some(p) = &profile {
            for a in &p.args {
                cmd.arg(a);
            }
            for (k, v) in &p.env {
                cmd.env(k, v);
            }
        }
        // The child handle moves into the reader thread so the real exit status can be reaped
        // when the pty EOFs (dropping it would lose the exit code to a detached zombie wait).
        let child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let shell_pid = child.process_id();
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().expect("reader");
        // Shared with the reader thread, which writes query answers back to the pty.
        let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));

        let state = Arc::new(Mutex::new(TabState::default()));
        let replies = Arc::new(Mutex::new(Vec::new()));
        let term_config = Config {
            scrolling_history: opts.scrollback_lines,
            semantic_escape_chars: opts.word_separators.clone(),
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &Dims { cols, rows },
            EventProxy { state: state.clone(), replies: replies.clone() },
        )));

        let pty = Pty::Spawned(pair.master);
        let prompt_owner = agent
            .as_ref()
            .and_then(|_| pty.as_fd().and_then(|fd| fd.try_clone_to_owned().ok()).zip(shell_pid));
        spawn_reader(ReaderCtx {
            reader,
            term: term.clone(),
            state: state.clone(),
            writer: writer.clone(),
            replies,
            ctx,
            detect_progress,
            child: Some(child),
            uptime_base: std::time::Duration::ZERO,
            pending: agent.as_ref().map(AgentPane::pending).unwrap_or_default(),
            prompt_owner,
        });

        Self {
            term,
            writer,
            pty,
            state,
            cols,
            rows,
            shell_pid,
            bold_bright: opts.bold_bright,
            rapid_exits: 0,
            killed: false,
            owns_session: true, // a shell we spawned is ours to reap
            started: std::time::Instant::now(),
            uptime_base: std::time::Duration::ZERO,
            alive_token: Arc::new(()),
            agent,
            notice: None,
        }
    }

    /// Take over a LIVE pty handed to us by a predecessor stdusk (see `handoff`). No spawn happens:
    /// the shell keeps running, unaware that its master fd changed owner. Everything our fresh
    /// `Term` cannot deduce rides in [`Adopted`].
    ///
    /// The grid starts EMPTY: alacritty's `Term` has no serialization, so neither the screen nor the
    /// scrollback survives. [`Self::request_redraw`] is what puts content back.
    ///
    /// Only ONE reader may pump a pty: bytes are consumed, not broadcast. While the predecessor's
    /// reader thread is still alive it competes for output and whatever it wins is LOST to us, so
    /// the handover must be the last thing the predecessor does before exiting - and why the redraw
    /// request is repeated until output actually lands here.
    pub(crate) fn adopt(
        ctx: egui::Context,
        handover: Adopted,
        opts: &SpawnOpts,
    ) -> std::io::Result<Self> {
        let Adopted {
            fd,
            cols,
            rows,
            pgid,
            alive,
            alt_screen,
            cmd_running,
            replay,
            title_osc,
            theme_reports,
        } = handover;
        let redraw_ctx = ctx.clone(); // the reader thread takes `ctx`; the nudger needs one too
        // Separate dups for the reader thread and the writer: both sides of the same pty master,
        // independently owned, exactly like `try_clone_reader` + `take_writer` give us on a spawn.
        let reader = std::fs::File::from(fd.try_clone()?);
        let writer: Box<dyn Write + Send> = Box::new(std::fs::File::from(fd.try_clone()?));
        let writer = Arc::new(Mutex::new(writer));

        // Seed the cwd AND the OSC title from the handover instead of waiting for the shell to
        // re-emit them: it re-sends OSC 7 only at its next prompt and an app re-sends its title only
        // when the title changes, so anything reading `cwd()`/`title_osc()` (the tab's auto-title,
        // "new tab here") would otherwise be blind for the rest of the session.
        let state = Arc::new(Mutex::new(TabState {
            cwd: opts.cwd.clone(),
            title_osc,
            theme_reports,
            ..TabState::default()
        }));
        let replies = Arc::new(Mutex::new(Vec::new()));
        let term_config = Config {
            scrolling_history: opts.scrollback_lines,
            semantic_escape_chars: opts.word_separators.clone(),
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &Dims { cols, rows },
            EventProxy { state: state.clone(), replies: replies.clone() },
        )));
        // Enter the alt screen ourselves when the handed-over app owned it: its repaint then lands
        // on the alt grid, and the `ESC[?1049l` it sends when it exits restores our (empty) primary
        // one instead of being a no-op that leaves its leftovers under the shell's next prompt. A
        // stale flag (the app quit mid-handover) is healed by the prompt-mark path in `spawn_reader`.
        if alt_screen {
            term.lock().swap_alt();
        }
        // Repaint the donor's screen BEFORE the reader starts, so the very first frame already shows
        // it and live output simply continues from there. A separate parser instance is fine - the
        // dump is self-contained, so it can never leave a half-parsed escape behind for the reader.
        if !replay.is_empty() {
            let mut replay_parser: Processor = Processor::new();
            replay_parser.advance(&mut *term.lock(), &replay);
        }

        spawn_reader(ReaderCtx {
            reader: Box::new(reader),
            term: term.clone(),
            state: state.clone(),
            writer: writer.clone(),
            replies,
            ctx,
            detect_progress: opts.detect_progress,
            child: None, // not our child: EOF is the only exit signal, code stays unknown
            uptime_base: alive,
            pending: PendingInput::default(), // a live handoff resumes nothing
            prompt_owner: None,
        });

        let me = Self {
            term,
            writer,
            pty: Pty::Adopted(fd),
            state,
            cols,
            rows,
            shell_pid: pgid,
            bold_bright: opts.bold_bright,
            rapid_exits: 0,
            killed: false,
            owns_session: false, // not ours until the ACK is delivered (see `arm_teardown`)
            started: std::time::Instant::now(),
            uptime_base: alive,
            alive_token: Arc::new(()),
            // The agent runs on in the adopted shell, so the pane tracks it when tracking is on.
            agent: opts.agent_tracking.as_ref().map(|_| AgentPane::adopted()),
            notice: None,
        };
        me.request_redraw(alt_screen, cmd_running, !replay.is_empty(), redraw_ctx);
        Ok(me)
    }

    /// Ask whatever is running on an adopted pty to paint the screen again, and keep asking while
    /// nothing arrives. Without this the pane shows an empty grid with a cursor until the user hits
    /// Enter - the shells are live, they just have no reason to speak.
    ///
    /// The size wiggle goes to EVERY adopted pane (harmless, and the only thing a full-screen app
    /// redraws on). The `^L` is the delicate part - see [`allow_ctrl_l`]: only a shell sitting at its
    /// prompt consumes it as clear-screen-and-redraw. Sent to a pane with a command running, the
    /// line discipline just ECHOES it as a literal `^L` and leaves the byte in the input queue,
    /// which is exactly what showed up at the top-left of two panes after a restart.
    ///
    /// The retries exist because a single ask is genuinely unreliable: until the predecessor process
    /// exits, ITS reader thread is still draining this pty, and anything it wins is lost to us. It
    /// closes right after our ACK, so the window is short - but it is exactly the window we ask in.
    /// They run on their own short-lived thread (the UI thread must not sleep), which holds a DUP of
    /// the master for up to [`REDRAW_RETRY_DELAYS`]'s total - closing the pane during that window
    /// defers the master's SIGHUP by that much, while `kill()` still reaps the group at once.
    fn request_redraw(
        &self,
        alt_screen: bool,
        cmd_running: Option<bool>,
        replayed: bool,
        ctx: egui::Context,
    ) {
        let (state, writer) = (self.state.clone(), self.writer.clone());
        let Some(fd) = self.pty.as_fd().and_then(|fd| fd.try_clone_to_owned().ok()) else {
            return; // no fd to wiggle: nothing to ask, and nothing to retry
        };
        // What the predecessor DECLARED (OSC 133) wins; without it, ask the tty itself.
        let running = cmd_running.or_else(|| foreground_command(fd.as_fd(), self.shell_pid));
        let (cols, rows) = (self.cols, self.rows);
        let alive = Arc::downgrade(&self.alive_token);
        thread::spawn(move || {
            for (attempt, wait) in REDRAW_RETRY_DELAYS.iter().enumerate() {
                thread::sleep(*wait);
                if alive.upgrade().is_none() {
                    return; // the pane is gone: stop poking its pty and let the dup go
                }
                // Something painted: the pane is no longer blank, so stop poking it - and never
                // type into a pane that has since started producing output on its own.
                if attempt > 0 && state.lock().is_ok_and(|s| s.saw_output) {
                    return;
                }
                nudge_winsize(fd.as_fd(), cols, rows);
                if allow_ctrl_l(alt_screen, running, attempt, replayed)
                    && let Ok(mut w) = writer.lock()
                {
                    let _ = w.write_all(b"\x0c");
                    let _ = w.flush();
                }
                ctx.request_repaint();
            }
        });
    }

    /// The record to save for this pane.
    pub(crate) fn agent_saved(&self) -> Option<crate::session::SavedAgent> {
        self.agent.as_ref()?.saved()
    }

    /// Advance this pane's record by one scan result, taken at `taken`, and the status the shell
    /// reported since the last one.
    pub(crate) fn step_agent(&mut self, seen: Seen, taken: std::time::Instant) {
        let status = self.take_status();
        if let Some(agent) = &mut self.agent {
            agent.scan(status, seen, taken);
        }
    }

    /// Apply the status that no scan has seen yet. The egui frame in main.rs calls this, also
    /// before a final snapshot, so the saved record knows a crash within a frame of it. `true` if
    /// one came in.
    pub(crate) fn apply_pending_status(&mut self) -> bool {
        let Some(agent) = &mut self.agent else { return false };
        let status = self.state.lock().unwrap().first_status.take();
        agent.apply_status(status);
        status.is_some()
    }

    fn take_status(&self) -> Option<Status> {
        self.state.lock().unwrap().first_status.take()
    }

    /// Hold a session that stdusk reopened until an agent binds to it (see `agent_track`).
    pub(crate) fn restore_agent(&mut self, session: AgentSession, crash_hint: bool) {
        if let Some(agent) = &mut self.agent {
            agent.restore(session, crash_hint);
        }
    }

    /// Hold the session of a live handoff, whose agent keeps running (see `agent_track`).
    pub(crate) fn adopt_agent(&mut self, session: AgentSession, crash_hint: bool) {
        if let Some(agent) = &mut self.agent {
            agent.adopt(session, crash_hint);
        }
    }

    /// The fallback for a shell that marks no prompt (see [`AgentPane::fallback_input`]).
    pub(crate) fn release_pending_after_wait(&mut self) {
        let at_prompt = || {
            self.pty.as_fd().and_then(|fd| foreground_command(fd, self.shell_pid)) == Some(false)
        };
        match self.agent.as_mut().and_then(|a| a.fallback_input(at_prompt)) {
            Some(Fallback::Type(bytes)) => self.write_input(&bytes),
            // The pane says what happened, in place of a notice that promised a typed command.
            Some(Fallback::Drop) => {
                if let Some(saved) = self.agent_saved() {
                    self.notice = Some(PaneNotice::NotResumed {
                        kind: saved.kind,
                        reason: SkipReason::ShellBusy,
                    });
                }
            }
            None => {}
        }
    }

    /// How long this shell has been running, counting time under previous owners - what a further
    /// handoff must carry so the crash-loop guard never reads a long-lived shell as freshly spawned.
    pub(crate) fn alive(&self) -> std::time::Duration {
        self.uptime_base + self.started.elapsed()
    }

    /// The master fd to hand to a successor, and the process group it must inherit responsibility
    /// for. `None` when the fd cannot be borrowed (non-unix), which aborts the handoff for this pane.
    pub(crate) fn handoff_fd(&self) -> Option<(std::os::fd::BorrowedFd<'_>, Option<u32>)> {
        self.pty.as_fd().map(|fd| (fd, self.shell_pid))
    }

    /// Mark this pane as handed over: the successor owns the shell now, so `kill()` and `Drop` must
    /// NOT reap it. Without this the handoff is a silent no-op - the shells would be killed on our way
    /// out, milliseconds after the successor adopted them. Only ever called after a delivered ACK.
    pub(crate) fn mark_handed_off(&mut self) {
        self.owns_session = false;
    }

    /// Take ownership of an ADOPTED pane's session - the mirror of [`Self::mark_handed_off`], and the
    /// only thing that arms teardown for it. Called once the successor's ACK is delivered, because
    /// that is the moment the predecessor stops guarding these shells. Before it, a drop (a panic
    /// during startup, a failed ack, the user quitting the new window) must leave them alone: they
    /// are still the predecessor's, and it is still up.
    pub(crate) fn arm_teardown(&mut self) {
        self.owns_session = true;
    }

    /// PID of the tab's shell process - the root for CLI-awareness descendant scans.
    pub(crate) fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    /// The tty's foreground process group. A job the shell starts leads its own group, so this is
    /// the pid of the foreground job's first process. It picks the agent that the user works in
    /// when two agents of one kind run in the pane (see `procwatch::nearest_agent`).
    pub(crate) fn foreground_pgid(&self) -> Option<u32> {
        foreground_pgid(self.pty.as_fd()?)
    }

    /// Terminate the shell's whole pty SESSION - the shell AND every job it started - so nothing
    /// leaks as an orphan. See [`kill_pty_session`] for why the session, not just the process group.
    /// Idempotent (the `killed` guard) and safe on an already-dead session. No-op off unix, when the
    /// pid is unknown, or when this pane's session is not ours to reap.
    pub(crate) fn kill(&mut self) {
        // Not ours to reap: either handed to a successor, or adopted and not yet confirmed as ours.
        // Reaping in the first case makes the handoff a silent no-op; in the second it kills shells
        // the predecessor is still guarding.
        if self.killed || !self.owns_session {
            return;
        }
        self.killed = true;
        #[cfg(unix)]
        if let Some(pid) = self.shell_pid {
            kill_pty_session(pid as i32);
        }
    }

    pub(crate) fn theme_reports(&self) -> bool {
        self.state.lock().unwrap().theme_reports
    }

    pub(crate) fn report_theme(&mut self, dark: bool) {
        if self.theme_reports() {
            // stdusk's own report, not the user's typing: it must keep the restore notice and the
            // queued resume command.
            self.write_input(crate::modes::theme_report(dark));
        }
    }

    /// Pairs with [`Self::wait_for_output_after`]: read it before sending input.
    pub(crate) fn output_gen(&self) -> u64 {
        self.state.lock().unwrap().output_gen
    }

    /// Block (at most `max`) until the reader has applied output newer than `since`, so the frame
    /// that sent a keystroke paints its echo. The grid is advanced before `output_gen` moves, so on
    /// `true` the echo is already in the next `grid_snapshot`. `false` = no output within `max`.
    pub(crate) fn wait_for_output_after(&self, since: u64, max: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            if self.output_gen() != since {
                return true;
            }
            if start.elapsed() >= max {
                return false;
            }
            thread::sleep(std::time::Duration::from_micros(50));
        }
    }

    /// Input from the user. It ends the restore notice, and it drops a queued resume command, which
    /// would garble a line the user is typing.
    pub(crate) fn send(&mut self, bytes: &[u8]) {
        if let Some(agent) = &self.agent {
            agent.discard_pending();
        }
        self.notice = None;
        self.write_input(bytes);
    }

    /// Write to the pty for stdusk itself (the fallback that types a resume command). Unlike
    /// [`PtyTerm::send`], it says nothing about the user's own input.
    fn write_input(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub(crate) fn notice(&self) -> Option<PaneNotice> {
        self.notice
    }

    pub(crate) fn set_notice(&mut self, notice: PaneNotice) {
        self.notice = Some(notice);
    }

    /// The user clicked in the pane.
    pub(crate) fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Paste text, wrapped in bracketed-paste markers when the app enabled that mode.
    pub(crate) fn paste(&mut self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.send(b"\x1b[200~");
            self.send(text.as_bytes());
            self.send(b"\x1b[201~");
        } else {
            self.send(text.as_bytes());
        }
    }

    /// Resize the pty + terminal grid to a new cell geometry (no-op if unchanged).
    pub(crate) fn resize(&mut self, cols: usize, rows: usize) {
        if (cols == self.cols && rows == self.rows) || cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.set_pty_size(cols, rows);
        self.term.lock().resize(Dims { cols, rows });
    }

    /// Push the window size to the pty master, which is what delivers SIGWINCH to the foreground
    /// process group. Spawned panes keep `portable_pty`'s proven path; an adopted pane only has a
    /// bare fd, so it goes through `tcsetwinsize` (the same ioctl underneath).
    fn set_pty_size(&self, cols: usize, rows: usize) {
        match &self.pty {
            Pty::Spawned(master) => {
                let _ = master.resize(PtySize {
                    rows: rows as u16,
                    cols: cols as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            Pty::Adopted(fd) => set_winsize(fd.as_fd(), cols, rows),
        }
    }

    pub(crate) fn scroll(&self, delta_lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(delta_lines));
    }

    pub(crate) fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// (display_offset, history_size) - for drawing the scrollbar.
    pub(crate) fn scroll_state(&self) -> (usize, usize) {
        let t = self.term.lock();
        let g = t.grid();
        (g.display_offset(), g.history_size())
    }

    /// Drop the scrollback history, keeping the visible screen (palette "Clear Scrollback").
    pub(crate) fn clear_scrollback(&self) {
        self.term.lock().grid_mut().clear_history();
    }

    /// Full wipe (Cmd+K / "Clear Terminal"): blank the viewport AND drop the history. The
    /// blank comes first on purpose - the caller follows with Ctrl-L, whose `ESC[2J` handler
    /// (alacritty `clear_viewport`) scrolls any still-occupied viewport lines INTO history,
    /// which would undo the wipe. Refused (`false`) on the alt screen: `grid_mut()` is the
    /// ALT grid there - wiping vim's display and mailing it a `^L` (a literal insert in
    /// insert mode) helps nobody. The caller sends Ctrl-L only when this returns `true`.
    pub(crate) fn clear_all(&self) -> bool {
        let mut t = self.term.lock();
        if t.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        let g = t.grid_mut();
        g.reset_region(..);
        g.clear_history();
        true
    }

    /// Jump the viewport to an absolute history offset (0 = bottom/live).
    pub(crate) fn scroll_to_offset(&self, target: usize) {
        let cur = self.term.lock().grid().display_offset();
        let delta = target as i32 - cur as i32;
        if delta != 0 {
            self.scroll(delta);
        }
    }

    pub(crate) fn progress(&self) -> Progress {
        self.state.lock().unwrap().progress
    }

    pub(crate) fn cmd_state(&self) -> CmdState {
        self.state.lock().unwrap().cmd
    }

    /// Take the pending bell (BEL rung since last call), for a one-shot visual flash.
    pub(crate) fn take_bell(&self) -> bool {
        std::mem::take(&mut self.state.lock().unwrap().bell)
    }

    /// Take a pending "long command finished" notification (exit code), if any.
    pub(crate) fn take_done_notify(&self) -> Option<i32> {
        self.state.lock().unwrap().done_notify.take()
    }

    /// Take the pending output-activity flag (any output since last call) - notify-on-activity.
    pub(crate) fn take_activity(&self) -> bool {
        std::mem::take(&mut self.state.lock().unwrap().activity)
    }

    pub(crate) fn cwd(&self) -> Option<String> {
        self.state.lock().unwrap().cwd.clone()
    }

    /// Exit report for a dead shell (pty EOF observed + reaped), if any. Stays set until the
    /// pane is respawned or closed - the egui frame in main.rs reads it to apply `on_exit`.
    pub(crate) fn exited(&self) -> Option<ExitInfo> {
        self.state.lock().unwrap().exited
    }

    /// The shell's OSC 0/2 window title, if it set one (an empty title resets to None).
    pub(crate) fn title_osc(&self) -> Option<String> {
        self.state.lock().unwrap().title_osc.clone()
    }

    /// Consecutive rapid deaths so far (crash-loop guard); carried across in-place respawns.
    pub(crate) fn rapid_exits(&self) -> u32 {
        self.rapid_exits
    }

    pub(crate) fn set_rapid_exits(&mut self, n: u32) {
        self.rapid_exits = n;
    }

    /// Take a pending OSC 52 clipboard payload (set by the shell), if any.
    pub(crate) fn take_clipboard(&self) -> Option<String> {
        self.state.lock().unwrap().clipboard.take()
    }

    /// Snapshot the visible viewport (honoring scrollback offset) with colors + cursor.
    pub(crate) fn grid_snapshot(&self) -> GridSnap {
        let term = self.term.lock();
        let selection = term.selection.as_ref().and_then(|s| s.to_range(&term));
        let show_cursor = term.mode().contains(TermMode::SHOW_CURSOR);
        let grid = term.grid();
        // The GRID's dimensions are authoritative - an app can resize it (CSI 8) independently of
        // our last pty resize, and returning self.cols/rows here would make the renderer index
        // out of bounds into `cells`.
        let (cols, rows) = (grid.columns(), grid.screen_lines());
        let mut cells = Vec::with_capacity(rows * cols);
        let mut top_line = -(grid.display_offset() as i32); // fallback; overwritten by first cell
        // display_iter walks the visible region row-major, accounting for scroll offset.
        for (i, indexed) in grid.display_iter().enumerate() {
            if i == 0 {
                top_line = indexed.point.line.0;
            }
            let cell = indexed.cell;
            let inverse = cell.flags.contains(Flags::INVERSE);
            let (fg_c, bg_c) = if inverse { (cell.bg, cell.fg) } else { (cell.fg, cell.bg) };
            let bg = if !inverse && colors::is_default_bg(cell.bg) {
                None
            } else {
                Some(colors::to_color32(bg_c))
            };
            let selected = selection.as_ref().is_some_and(|r| r.contains(indexed.point));
            let bold = cell.flags.contains(Flags::BOLD);
            let bright = self.bold_bright && bold;
            let dim = cell.flags.contains(Flags::DIM);
            let (c, wide) = snap_glyph(cell.c, cell.flags);
            cells.push(CellSnap {
                c,
                fg: colors::cell_fg(fg_c, bright),
                bg,
                selected,
                wide,
                bold,
                dim,
            });
        }
        // Cursor only shown at the bottom (not scrolled into history) AND while the app
        // hasn't hidden it (DECTCEM `CSI ?25l` - vim/copilot hide it for their own UI).
        let cursor = if show_cursor && grid.display_offset() == 0 {
            let cp = grid.cursor.point;
            Some((
                (cp.line.0.max(0) as usize).min(rows.saturating_sub(1)),
                cp.column.0.min(cols.saturating_sub(1)),
            ))
        } else {
            None
        };
        GridSnap { cols, rows, cells, cursor, top_line }
    }

    /// This pane's screen as ANSI, for a successor process to REPLAY into its fresh grid (see
    /// `screen`). Empty when there is nothing on screen worth sending.
    ///
    /// Reads the ACTIVE grid, so a pane on the alt screen dumps what the app has drawn there (and
    /// carries no history - the alt grid has none). Includes a bounded tail of scrollback so a loop
    /// that printed more than one screen still comes back with its context.
    pub(crate) fn screen_dump(&self) -> Vec<u8> {
        let term = self.term.lock();
        let grid = term.grid();
        // Grid dimensions are authoritative (an app may have resized it via CSI 8).
        let (cols, rows) = (grid.columns(), grid.screen_lines());
        let history = i32::try_from(crate::screen::MAX_HISTORY_LINES).unwrap_or(i32::MAX);
        let top = grid.topmost_line().0.max(-history);
        let bot = grid.bottommost_line().0;
        let mut lines = Vec::with_capacity((bot - top + 1).max(0) as usize);
        for l in top..=bot {
            let row = &grid[Line(l)];
            lines.push(
                (0..cols)
                    .map(|c| {
                        let cell = &row[Column(c)];
                        crate::screen::Cell {
                            c: cell.c,
                            fg: cell.fg,
                            bg: cell.bg,
                            flags: cell.flags,
                        }
                    })
                    .collect(),
            );
        }
        let cp = grid.cursor.point;
        let cursor = (
            (cp.line.0.max(0) as usize).min(rows.saturating_sub(1)),
            cp.column.0.min(cols.saturating_sub(1)),
        );
        crate::screen::encode(&lines, rows, cursor)
    }

    /// Whether the terminal is on the alternate screen (vim/less/...), e.g. to suppress the
    /// multiline-paste warning like Tabby does.
    pub(crate) fn is_alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the app enabled DECCKM (application cursor keys) - arrows/Home/End should then be
    /// sent as SS3 (`ESC O x`) rather than CSI (`ESC [ x`). Grab-copy-drop the lock.
    pub(crate) fn app_cursor(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// The mouse-reporting modes the app has enabled, snapshotted from the `Term`, so the UI can
    /// route wheel/pointer events to the pty instead of scrolling/selecting locally.
    pub(crate) fn mouse_reporting(&self) -> MouseReporting {
        let term = self.term.lock();
        let mode = term.mode();
        MouseReporting {
            report_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        }
    }

    /// Begin a text selection anchored at a grid point (mapped from mouse coords).
    pub(crate) fn start_selection(&self, line: i32, col: usize, right: bool) {
        let point = Point::new(Line(line), Column(col));
        let side = if right { Side::Right } else { Side::Left };
        self.term.lock().selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    /// Extend the in-progress selection to a new grid point (drag).
    pub(crate) fn update_selection(&self, line: i32, col: usize, right: bool) {
        let point = Point::new(Line(line), Column(col));
        let side = if right { Side::Right } else { Side::Left };
        if let Some(sel) = self.term.lock().selection.as_mut() {
            sel.update(point, side);
        }
    }

    /// Select the word under a point (double-click), using alacritty's semantic rules.
    pub(crate) fn select_word(&self, line: i32, col: usize) {
        let point = Point::new(Line(line), Column(col));
        self.term.lock().selection =
            Some(Selection::new(SelectionType::Semantic, point, Side::Left));
    }

    /// Select the whole line under a point (triple-click).
    pub(crate) fn select_line(&self, line: i32, col: usize) {
        let point = Point::new(Line(line), Column(col));
        self.term.lock().selection = Some(Selection::new(SelectionType::Lines, point, Side::Left));
    }

    pub(crate) fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// Select the entire buffer (scrollback + screen), for Cmd+A then copy.
    pub(crate) fn select_all(&self) {
        let mut t = self.term.lock();
        let (top, bot, cols) = {
            let g = t.grid();
            (g.topmost_line().0, g.bottommost_line().0, g.columns())
        };
        let start = Point::new(Line(top), Column(0));
        let end = Point::new(Line(bot), Column(cols.saturating_sub(1)));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        t.selection = Some(sel);
    }

    /// Visible row count (for page scrolling).
    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// Visible column count (handed to a successor so an adopted pty keeps its geometry).
    pub(crate) fn cols(&self) -> usize {
        self.cols
    }

    /// Selected text (for Cmd+C), or None when there's no non-empty selection.
    pub(crate) fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    /// Whole buffer (scrollback + screen) as `(alacritty Line, trailing-trimmed text)` pairs,
    /// top-to-bottom - the input to scrollback search.
    pub(crate) fn buffer_lines(&self) -> Vec<(i32, String)> {
        let term = self.term.lock();
        let grid = term.grid();
        // Grid dimensions are authoritative (an app may have resized it via CSI 8).
        let cols = grid.columns();
        let (top, bot) = (grid.topmost_line().0, grid.bottommost_line().0);
        let mut out = Vec::with_capacity((bot - top + 1).max(0) as usize);
        for l in top..=bot {
            let row = &grid[Line(l)];
            let mut s = String::with_capacity(cols);
            for c in 0..cols {
                s.push(row[Column(c)].c);
            }
            out.push((l, s.trim_end().to_string()));
        }
        out
    }

    /// Highlight a search match by reusing the selection range (so `grid_snapshot` paints it).
    pub(crate) fn highlight_match(&self, m: crate::search::Match) {
        let start = Point::new(Line(m.line), Column(m.col));
        let end = Point::new(Line(m.line), Column(m.col + m.len.saturating_sub(1)));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        self.term.lock().selection = Some(sel);
    }

    /// Scroll the viewport so buffer `line` sits at the top (clamped to available history).
    pub(crate) fn scroll_to_line(&self, line: i32) {
        let (_, history) = self.scroll_state();
        let target = (-line).clamp(0, history as i32) as usize;
        self.scroll_to_offset(target);
    }
}

/// Last-resort cleanup: a dropped pane/tab/app MUST NOT leak its shell tree. Drop runs on tab
/// close (the tab is removed), pane close (the leaf is dropped), respawn (the old term is
/// replaced), and app exit (the app -> tabs -> terms drop chain), so a single Drop covers every
/// path. `kill()` is idempotent, so an explicit earlier `kill()` won't double-signal here.
impl Drop for PtyTerm {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Terminate everything the pane's pty was hosting: the shell AND every job it started.
///
/// `leader` is the shell's pid, which is also its process-GROUP id and its SESSION id (portable-pty
/// `setsid`s it). Killing the process group alone is NOT enough, and that was the audited leak: an
/// interactive shell puts every foreground job in its OWN process group, so `killpg(shell)` reaches
/// the shell by itself. Measured on live `claude` panes - `pgid == claude's own pid`, `sid == the
/// shell's pid` - and reproduced with a real pty: a job that ignores SIGHUP/SIGTERM (which
/// node-based CLIs routinely do) outlived its tab. [`crate::procwatch::pty_victims`] is the boundary
/// that does hold - the pty session AND the descendant closure, since Claude Code's background jobs
/// `setsid` out of the session but stay in the tree.
///
/// Escalation is SIGTERM -> short grace -> SIGKILL, applied to the group and to every victim captured
/// UP FRONT - before the first signal, because SIGTERM breaks the parent links the sweep reads. A
/// process forked DURING the grace can be missed (one snapshot, not a loop of them), and a job whose
/// own parent already exited is in neither the session nor the tree and is unreachable. Both are
/// documented limits, not oversights.
///
/// Guards, because `leader` can be a pid we no longer own (an adopted pane's pgid comes off a socket
/// and nothing keeps the pid reserved): never a pid <= 0, never OUR own session (checked again per
/// victim, so a mis-snapshot cannot reach the process running us), and a LIVE `leader` must still be a
/// session leader - a recycled pid almost never is. When `leader` is already gone the sweep still runs,
/// since that is exactly the orphaned-jobs case; the residual risk is a recycled pid that has itself
/// become a session leader, which needs ~99k pids of wraparound first.
#[cfg(unix)]
#[allow(unsafe_code)]
fn kill_pty_session(leader: i32) {
    if leader <= 0 {
        return;
    }
    // SAFETY (all `unsafe` in this fn): thin POSIX FFI - getsid/kill/killpg with plain int args.
    // `kill(pid, 0)` and `killpg(pid, 0)` send nothing; they only probe existence.
    let sid = |pid: i32| unsafe { libc::getsid(pid) };
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
    let our_sid = sid(0);
    if our_sid == leader {
        return; // our own session: never, whatever the bookkeeping says
    }
    if alive(leader) && sid(leader) != leader {
        return; // not a session leader (any more): not the shell we spawned
    }
    let mut victims = crate::procwatch::pty_victims(leader as u32);
    let me = std::process::id();
    victims.retain(|&p| p != 1 && p != me && sid(p as i32) != our_sid);

    unsafe { libc::killpg(leader, libc::SIGTERM) };
    for &p in &victims {
        unsafe { libc::kill(p as i32, libc::SIGTERM) };
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Probe the captured list rather than re-enumerating: a refresh per poll would cost more
        // than the whole teardown budget.
        victims.retain(|&p| alive(p as i32));
        if victims.is_empty() && unsafe { libc::killpg(leader, 0) } == -1 {
            return;
        }
        if std::time::Instant::now() >= deadline {
            unsafe { libc::killpg(leader, libc::SIGKILL) };
            for &p in &victims {
                unsafe { libc::kill(p as i32, libc::SIGKILL) };
            }
            return;
        }
    }
}

/// Reap a session a predecessor handed us that we could NOT adopt. Its master fd died with the failed
/// adoption, and the predecessor disarms every pane the moment our ACK goes out - so no `PtyTerm`
/// anywhere owns it and nothing else will ever reap it. The pane is unusable either way (we have no
/// fd left to reach that shell); leaving the tree running with no terminal and no owner is exactly the
/// dangling-process bug this teardown path exists to prevent.
/// `leader` is the pane's pgid off the wire, which is also its pty session id.
pub(crate) fn reap_orphaned_session(leader: u32) {
    #[cfg(unix)]
    kill_pty_session(leader as i32);
    #[cfg(not(unix))]
    let _ = leader;
}

#[cfg(test)]
mod tests {
    use super::{
        CmdState, ECHO_WAIT, ExitAction, Flags, OnExit, PtyTerm, REPAINT_COALESCE_WINDOW,
        SpawnOpts, cmd_from_exit, exit_action, on_exit_mode, resolve_cwd, resolve_shell,
        snap_glyph,
    };
    use crate::agent_track::Status;
    use crate::config::Profile;
    use crate::mouse::wheel_sgr;

    #[test]
    fn exit_code_to_state() {
        assert_eq!(cmd_from_exit(Some(0)), CmdState::Ok);
        assert_eq!(cmd_from_exit(None), CmdState::Ok);
        assert_eq!(cmd_from_exit(Some(1)), CmdState::Fail);
        assert_eq!(cmd_from_exit(Some(127)), CmdState::Fail);
        assert_eq!(cmd_from_exit(Some(130)), CmdState::Idle); // Ctrl+C (SIGINT)
        assert_eq!(cmd_from_exit(Some(143)), CmdState::Idle); // SIGTERM
    }

    fn profile(shell: Option<&str>, cwd: Option<&str>) -> Profile {
        Profile {
            name: "test".into(),
            shell: shell.map(Into::into),
            args: Vec::new(),
            cwd: cwd.map(Into::into),
            env: std::collections::BTreeMap::new(),
            color: None,
        }
    }

    #[test]
    fn profile_shell_wins_over_env_shell() {
        let p = profile(Some("/opt/fish"), None);
        assert_eq!(resolve_shell(Some(&p), Some("/bin/bash".into())), "/opt/fish");
    }

    #[test]
    fn shell_falls_back_to_env_then_zsh() {
        let no_shell = profile(None, None);
        assert_eq!(resolve_shell(Some(&no_shell), Some("/bin/bash".into())), "/bin/bash");
        assert_eq!(resolve_shell(None, Some("/bin/bash".into())), "/bin/bash");
        assert_eq!(resolve_shell(None, None), "/bin/zsh");
    }

    #[test]
    fn profile_cwd_wins_over_caller_cwd() {
        let p = profile(None, Some("/profile/dir"));
        assert_eq!(
            resolve_cwd(Some(&p), Some("/caller/dir".into())).as_deref(),
            Some("/profile/dir")
        );
        let no_cwd = profile(None, None);
        assert_eq!(
            resolve_cwd(Some(&no_cwd), Some("/caller/dir".into())).as_deref(),
            Some("/caller/dir")
        );
        assert_eq!(resolve_cwd(None, None), None);
    }

    #[test]
    fn profile_cwd_tilde_expands_to_home() {
        let home = std::env::var("HOME").unwrap();
        let p = profile(None, Some("~/Git"));
        assert_eq!(resolve_cwd(Some(&p), None), Some(format!("{home}/Git")));
    }

    #[test]
    fn on_exit_mode_parses_with_close_default() {
        assert_eq!(on_exit_mode("close"), OnExit::Close);
        assert_eq!(on_exit_mode("Keep"), OnExit::Keep);
        assert_eq!(on_exit_mode("restart"), OnExit::Restart);
        assert_eq!(on_exit_mode("nonsense"), OnExit::Close);
        assert_eq!(on_exit_mode(""), OnExit::Close);
    }

    #[test]
    fn exit_action_decision_table() {
        use ExitAction::{ClosePane, Keep, Restart};
        let cases = [
            (OnExit::Close, 100.0, 0, ClosePane),
            (OnExit::Close, 0.1, 5, ClosePane), // close ignores the loop guard
            (OnExit::Keep, 0.1, 0, Keep),
            (OnExit::Keep, 100.0, 3, Keep),
            (OnExit::Restart, 100.0, 0, Restart),
            (OnExit::Restart, 1.0, 0, Restart), // FIRST rapid death still restarts
            (OnExit::Restart, 1.0, 1, Keep),    // second in a row = crash loop -> keep
            (OnExit::Restart, 1.0, 7, Keep),
            (OnExit::Restart, 100.0, 3, Restart), // a long-lived run clears the concern
        ];
        for (mode, uptime, rapid, want) in cases {
            assert_eq!(exit_action(mode, uptime, rapid), want, "{mode:?} up={uptime} n={rapid}");
        }
    }

    /// Spawn a REAL pty running `/bin/sh -c <script>` (no integration hooks) on a 20x5 grid.
    fn e2e_term(script: &str) -> PtyTerm {
        e2e_term_sized(20, 5, script)
    }

    /// Poll `check` until it returns Some or the timeout hits.
    fn poll_term<T>(term: &PtyTerm, check: impl Fn(&PtyTerm) -> Option<T>) -> Option<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(v) = check(term) {
                return Some(v);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        None
    }

    /// Spawn + poll in one go (most e2e cases don't need the term afterwards).
    fn spawn_and_poll<T>(script: &str, check: impl Fn(&PtyTerm) -> Option<T>) -> Option<T> {
        let term = e2e_term(script);
        poll_term(&term, check)
    }

    #[test]
    fn real_pty_exit_reports_code_and_uptime() {
        let exit = spawn_and_poll("exit 3", PtyTerm::exited).expect("exit never observed");
        assert_eq!(exit.code, 3);
        assert!(
            exit.uptime_secs < 10.0,
            "uptime must reflect spawn->exit, got {}",
            exit.uptime_secs
        );
    }

    /// Poll [`descendants`] until `ready` accepts the tree (or ~8s pass), then return it. Waiting on
    /// the SHAPE, not a count, is what keeps a teardown probe from snapshotting a tree the shell was
    /// still in the middle of building.
    #[cfg(unix)]
    fn wait_for_descendants(
        root: u32,
        ready: impl Fn(&[(u32, i32, i32)]) -> bool,
    ) -> Vec<(u32, i32, i32)> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let kids = descendants(root);
            if ready(&kids) || std::time::Instant::now() >= deadline {
                return kids;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Every live descendant of `root`, with its process group and session, from the live table.
    #[cfg(unix)]
    #[allow(unsafe_code)] // getpgid/getsid: thin POSIX FFI, plain int args
    fn descendants(root: u32) -> Vec<(u32, i32, i32)> {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            false,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        let procs = crate::procwatch::snapshot(&sys);
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            for p in procs.iter().filter(|p| p.parent == Some(pid)) {
                // SAFETY: both are plain queries on a pid we just read from the process table.
                out.push((p.pid, unsafe { libc::getpgid(p.pid as i32) }, unsafe {
                    libc::getsid(p.pid as i32)
                }));
                stack.push(p.pid);
            }
        }
        out
    }

    /// A pane running a REAL interactive `zsh` - what production spawns, and the only shape in which
    /// job control (a process group per foreground job) actually happens. `-f` skips the user's rc
    /// files so the probe is the same on any machine.
    fn interactive_zsh() -> PtyTerm {
        let mut opts = crate::test_support::spawn_opts("/bin/zsh", &["-f"]);
        opts.scrollback_lines = 200;
        PtyTerm::spawn(80, 24, egui::Context::default(), &opts)
    }

    /// A shell line for a job that CANNOT be talked out of running: `trap ""` sets TERM/HUP to
    /// SIG_IGN, which survives the `exec`, and the exec keeps it ONE pid - so a teardown snapshot can
    /// never race a fork the probe was about to do.
    const STUBBORN_JOB: &[u8] = b"/bin/sh -c 'trap \"\" TERM HUP; exec sleep 300'\r";

    /// Wait until every pid in `pids` is gone, then SIGKILL whatever is left and return it - so a
    /// failing teardown assertion can never leak the probe processes it was measuring.
    #[cfg(unix)]
    #[allow(unsafe_code)] // kill(pid, 0) probes existence; SIGKILL cleans up our own spawns
    fn survivors_of(pids: &[u32]) -> Vec<u32> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let alive = loop {
            // SAFETY: signal 0 only probes existence.
            let alive: Vec<u32> =
                pids.iter().copied().filter(|&p| unsafe { libc::kill(p as i32, 0) } == 0).collect();
            if alive.is_empty() || std::time::Instant::now() >= deadline {
                break alive;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        for &p in &alive {
            // SAFETY: same thin FFI, on pids this test spawned itself.
            unsafe { libc::kill(p as i32, libc::SIGKILL) };
        }
        alive
    }

    #[test]
    #[cfg(unix)]
    fn real_pty_kill_reaps_a_foreground_job_in_its_own_process_group() {
        // The AUDITED LEAK, reproduced: an INTERACTIVE shell (what production spawns) puts every
        // foreground job in its OWN process group, so the old `killpg(shell)` reached the shell
        // alone. A job that ignores SIGHUP/SIGTERM - which node-based CLIs routinely do - then
        // outlived its tab forever. Measured on the user's live `claude` panes too: `pgid == the
        // claude pid`, `sid == the shell's pid`.
        let mut term = interactive_zsh();
        let shell = poll_term(&term, PtyTerm::shell_pid).expect("no shell pid");
        std::thread::sleep(std::time::Duration::from_millis(700)); // reach the prompt
        term.send(STUBBORN_JOB);
        let kids = wait_for_descendants(shell, |kids| {
            kids.iter().any(|&(pid, pgid, sid)| pgid == pid as i32 && sid == shell as i32)
        });
        assert!(
            !kids.is_empty(),
            "the job must be in its OWN group inside the shell's session, got {kids:?}"
        );

        term.kill();
        let pids: Vec<u32> = kids.iter().map(|&(p, _, _)| p).collect();
        let survivors = survivors_of(&pids);
        assert!(survivors.is_empty(), "a signal-ignoring foreground job leaked: {survivors:?}");
    }

    #[test]
    #[cfg(unix)]
    fn real_pty_kill_reaps_a_job_that_escaped_into_its_own_session() {
        // Concern C of the teardown audit, and the user's actual leak - MEASURED on their live tree
        // before it was written: Claude Code runs every Bash tool call through a `/bin/zsh` in a NEW
        // SESSION (`sid == its own pid`), so a backgrounded `deno task dev` sits outside the pane's
        // pty session entirely. Neither `killpg(shell)` nor a session-wide sweep can see it; the only
        // thing still tying it to the tab is the parent CHAIN, so teardown has to walk descendants
        // too - and snapshot them BEFORE signalling, because the first SIGTERM breaks those links.
        //
        // Shape reproduced exactly: shell -> holder (in the pane's session) -> escapee (its own
        // session, ignoring TERM/HUP so only a real reap can end it).
        let mut term = interactive_zsh();
        let shell = poll_term(&term, PtyTerm::shell_pid).expect("no shell pid");
        std::thread::sleep(std::time::Duration::from_millis(700)); // reach the prompt
        // `fork` first so the child is not a process-group leader: `setsid` fails for one. No `!`
        // anywhere in the line - an interactive zsh would history-expand it.
        term.send(
            b"/usr/bin/perl -e 'use POSIX; my $p = fork; if ($p == 0) { POSIX::setsid(); \
              $SIG{TERM} = \"IGNORE\"; $SIG{HUP} = \"IGNORE\"; sleep 300; } else { sleep 300; }'\r",
        );

        let escaped_from = |kids: &[(u32, i32, i32)]| {
            kids.iter().find(|&&(pid, _, sid)| sid == pid as i32 && sid != shell as i32).copied()
        };
        let kids = wait_for_descendants(shell, |kids| escaped_from(kids).is_some());
        let (escaped, _, _) = escaped_from(&kids).unwrap_or_else(|| {
            panic!("the probe never escaped into its own session, got {kids:?}")
        });

        term.kill();
        let pids: Vec<u32> = kids.iter().map(|&(p, _, _)| p).collect();
        let survivors = survivors_of(&pids);
        assert!(
            survivors.is_empty(),
            "a job that setsid'd out of the pty session leaked (escapee {escaped}): {survivors:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    #[allow(unsafe_code)] // getsid, and the guards under test are FFI-shaped
    fn kill_pty_session_refuses_our_own_session_and_a_non_leader_pid() {
        // Both pid-reuse guards, exercised against THIS process - if either fails, the test binary
        // (and whatever shell is running it) dies, which is the loudest possible assertion.
        // SAFETY: getsid(0) queries our own session; plain int arg.
        let our_sid = unsafe { libc::getsid(0) };
        assert!(our_sid > 0, "we must have a session to test the guard with");
        super::kill_pty_session(our_sid); // guard 1: never our own session
        let me = std::process::id() as i32;
        // A test binary is not a session leader (its shell / cargo is), which is exactly the shape a
        // RECYCLED pid has: alive, but not the session leader we recorded.
        // SAFETY: same as above.
        assert_ne!(unsafe { libc::getsid(me) }, me, "the test binary must not be a session leader");
        super::kill_pty_session(me); // guard 2: alive but not a session leader
        super::kill_pty_session(0); // and the pid <= 0 guard
        super::kill_pty_session(-1);
        // SAFETY: signal 0 only probes existence - we are still here.
        assert_eq!(unsafe { libc::kill(me, 0) }, 0, "we must have survived every guard");
    }

    #[test]
    #[cfg(unix)]
    #[allow(unsafe_code)] // killpg existence probe
    fn real_pty_an_adopted_pane_is_not_reaped_before_its_ack() {
        // Concern A of the teardown audit: the successor adopts every pane BEFORE it acknowledges, so
        // anything that drops those panes first (a panic in startup, a failed ack write, the user
        // quitting the new window) used to reap shells the predecessor was still guarding - killing
        // the user's work while the old window reported the restart as cancelled. An adopted pane is
        // therefore disarmed until `arm_teardown`.
        let mut donor = e2e_term("i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done");
        let pgid = poll_term(&donor, PtyTerm::shell_pid).expect("no shell pid") as i32;
        {
            let heir = adopt_from(&mut donor, false, Some(true), false);
            drop(heir); // no ACK was ever delivered
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        // SAFETY: signal 0 only probes existence.
        assert_eq!(
            unsafe { libc::killpg(pgid, 0) },
            0,
            "an unacknowledged adoption must NOT reap the predecessor's shells"
        );

        // ...and once the ACK has landed, the successor DOES own them: the mirror direction, so the
        // disarm can never silently become a leak.
        let mut heir = adopt_from(&mut donor, false, Some(true), false);
        heir.arm_teardown();
        heir.kill();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let gone = loop {
            // SAFETY: same probe.
            if unsafe { libc::killpg(pgid, 0) } == -1 {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        drop(heir);
        reap_probe(&donor);
        assert!(gone, "an ARMED adopted pane must reap its session");
    }

    #[test]
    #[cfg(unix)]
    #[allow(unsafe_code)] // killpg existence probe, mirroring kill_process_group
    fn real_pty_kill_terminates_the_process_group() {
        // The shell backgrounds a long sleep in its OWN process group (non-interactive `sh -c`
        // runs jobs in-group). kill() must take the WHOLE group down, not just the shell.
        let mut term = e2e_term("sleep 300 & sleep 60");
        let pid = poll_term(&term, PtyTerm::shell_pid).expect("no shell pid") as i32;
        // SAFETY: signal 0 only probes existence. The group is alive right after spawn.
        assert_eq!(unsafe { libc::killpg(pid, 0) }, 0, "group must be alive before kill()");
        term.kill();
        // kill() signalled the group; its members then die and get REAPED asynchronously (the
        // reader thread reaps the shell, init reaps the reparented sleep), so killpg's existence
        // probe only flips to ESRCH once the last zombie is gone - poll for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let gone = loop {
            // SAFETY: same probe as above.
            if unsafe { libc::killpg(pid, 0) } == -1 {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        assert!(gone, "process group must be dead after kill()");
        // `kill()` fires immediately after spawn, so `sh` can fork `sleep 300 &` (into its OWN group)
        // AFTER the teardown snapshot was taken - the documented one-snapshot window. The assertion
        // above is about the group; this keeps the race from leaking a `sleep` per run.
        super::reap_orphaned_session(pid as u32);
    }

    #[test]
    #[cfg(unix)]
    #[allow(unsafe_code)] // killpg existence probe, mirroring the test above
    fn real_pty_handed_off_pane_survives_kill_and_drop() {
        // The exact inverse of the test above, and the whole point of session handoff: once a pane
        // is marked handed off, NEITHER kill() nor Drop may reap its group - the successor owns it.
        let pid = {
            let mut term = e2e_term("sleep 300 & sleep 60");
            let pid = poll_term(&term, PtyTerm::shell_pid).expect("no shell pid") as i32;
            // SAFETY: signal 0 only probes existence.
            assert_eq!(unsafe { libc::killpg(pid, 0) }, 0, "group must be alive to start");
            term.mark_handed_off();
            term.kill(); // must be a no-op now
            pid
        }; // Drop runs here - also must not kill.
        std::thread::sleep(std::time::Duration::from_millis(300));
        // SAFETY: same probe.
        assert_eq!(
            unsafe { libc::killpg(pid, 0) },
            0,
            "handed-off group must STILL be alive after kill() + Drop"
        );
        // Clean up: nothing else will, precisely because we disarmed the teardown. By SESSION, not by
        // group - `sh` puts `sleep 300 &` in its own process group, so the killpg this used to do left
        // one `sleep` behind on every run. The audited leak, in the suite's own cleanup.
        super::reap_orphaned_session(pid as u32);
    }

    #[test]
    #[cfg(unix)]
    #[allow(unsafe_code)] // kill(pid, 0) existence probe
    fn a_pane_that_could_not_be_adopted_is_reaped_not_leaked() {
        // The one handoff branch that can strand a shell with NO owner at all: `PtyTerm::adopt` fails
        // (its `dup` of the handed-over master is lost with it), the successor falls back to a fresh
        // shell - and the predecessor disarms EVERY pane anyway the moment the ACK goes out. Nothing
        // is left holding that session, and the state it is left in is reproduced here exactly: a
        // disarmed pane, dropped, with a job that ignores TERM/HUP still running under it.
        let mut donor = interactive_zsh();
        let shell = poll_term(&donor, PtyTerm::shell_pid).expect("no shell pid");
        std::thread::sleep(std::time::Duration::from_millis(700)); // reach the prompt
        donor.send(STUBBORN_JOB);
        let kids = wait_for_descendants(shell, |kids| !kids.is_empty());
        assert!(!kids.is_empty(), "the probe job never started");
        let pids: Vec<u32> = kids.iter().map(|&(p, _, _)| p).collect();

        donor.mark_handed_off(); // the fd moved on: this pane must not reap anything
        drop(donor);
        std::thread::sleep(std::time::Duration::from_millis(400));
        // SAFETY: signal 0 only probes existence.
        assert!(
            pids.iter().all(|&p| unsafe { libc::kill(p as i32, 0) } == 0),
            "losing the pane is not a reap - that is precisely why the branch has to ask for one"
        );

        super::reap_orphaned_session(shell);
        let survivors = survivors_of(&pids);
        assert!(survivors.is_empty(), "an unadoptable pane's session leaked: {survivors:?}");
    }

    #[test]
    fn ctrl_l_only_goes_to_a_shell_that_is_at_its_prompt() {
        use super::allow_ctrl_l;
        // (alt_screen, running, attempt, replayed) -> may send ^L
        let cases = [
            // At a prompt: the one case that consumes it as clear-screen-and-redraw.
            ((false, Some(false), 0, false), true),
            ((false, Some(false), 3, false), true),
            // ...unless the screen was REPLAYED: zle's clear-screen would wipe it to redraw a bare
            // prompt, throwing away the content the replay just restored.
            ((false, Some(false), 0, true), false),
            // A command is running: the tty would ECHO it as a literal "^L" (the 1.6.2 bug).
            ((false, Some(true), 0, false), false),
            ((false, Some(true), 3, false), false),
            // Alt screen: the byte belongs to the app, whatever the command state says.
            ((true, Some(false), 0, false), false),
            ((true, Some(true), 1, false), false),
            ((true, None, 2, true), false),
            // Unknown (old predecessor / no OSC 133): silent first, then treat a pane that stayed
            // quiet through the grace period as an idle prompt.
            ((false, None, 0, false), false),
            ((false, None, 1, false), true),
            ((false, None, 1, true), false),
        ];
        for ((alt, running, attempt, replayed), want) in cases {
            assert_eq!(
                allow_ctrl_l(alt, running, attempt, replayed),
                want,
                "alt={alt} running={running:?} attempt={attempt} replayed={replayed}"
            );
        }
    }

    #[test]
    fn real_pty_only_a_size_change_delivers_sigwinch() {
        // The fact this whole redraw path rests on: a pty signals SIGWINCH only when the window size
        // actually CHANGES. `nudge_redraw` used to re-send the CURRENT size, which is silently
        // nothing - so an adopted full-screen app was never asked to repaint and the pane stayed
        // blank. Measured here rather than trusted: XNU's TIOCSWINSZ compares before signalling
        // (Linux's `tty_do_resize` has the same guard).
        //
        // `read` is a BUILTIN, so the shell itself stays the tty's foreground process group and the
        // signal reaches its trap; a `sleep` child would become the foreground group instead and
        // swallow it (SIGWINCH's default action is to ignore).
        let log = std::env::temp_dir().join(format!("stdusk-winch-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let term = e2e_term(&format!(
            "trap 'echo w >> {}' WINCH; printf ARMED; while :; do read line; done",
            log.display()
        ));
        // ARMED in the grid proves the trap is installed (it is set before the print).
        assert!(
            poll_term(&term, |t| grid_text(t).contains("ARMED").then_some(())).is_some(),
            "the probe shell never armed its trap"
        );
        let winches = || std::fs::read_to_string(&log).unwrap_or_default().lines().count();

        let fd = term.pty.as_fd().expect("a spawned pty has a master fd");
        super::set_winsize(fd, term.cols(), term.rows()); // the OLD nudge: same size
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(winches(), 0, "re-sending the same size must NOT signal - that was the bug");

        super::nudge_winsize(fd, term.cols(), term.rows());
        let signalled = poll_term(&term, |_| (winches() > 0).then_some(()));
        assert!(signalled.is_some(), "nudge_redraw must actually deliver a SIGWINCH");
        let _ = std::fs::remove_file(&log);
    }

    /// A donor pane running a "shell" that logs every byte it is sent as `GOT-<hex>` to the returned
    /// file, in RAW mode with echo off so only real input shows up (never the line discipline's echo).
    /// `ARMED` in the grid means the raw mode is in effect and it is safe to hand the pty over.
    /// A file, not pty output: the donor's reader drains the same master and can eat the replies.
    fn raw_byte_reporter(tag: &str) -> (PtyTerm, std::path::PathBuf) {
        let log =
            std::env::temp_dir().join(format!("stdusk-bytes-{tag}-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let term = e2e_term(&format!(
            "stty raw -echo; printf ARMED; \
             while :; do b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf 'GOT-%s ' \"$b\" >> '{}'; done",
            log.display()
        ));
        assert!(
            poll_term(&term, |t| grid_text(t).contains("ARMED").then_some(())).is_some(),
            "the probe shell never armed raw mode"
        );
        (term, log)
    }

    /// Tear down a probe pane after a handover test. `donor.kill()` is DISARMED by the
    /// `mark_handed_off` inside `adopt_from` (that is the whole point of the handoff), so the group
    /// has to be reaped explicitly - otherwise the shell and its pty outlive the test and starve the
    /// rest of the suite of ptys.
    fn reap_probe(donor: &PtyTerm) {
        #[cfg(unix)]
        if let Some(pid) = donor.shell_pid() {
            #[allow(unsafe_code)] // SAFETY: thin FFI to killpg with a pid we spawned ourselves
            unsafe {
                libc::killpg(pid as i32, libc::SIGKILL)
            };
        }
    }

    /// Hand `donor`'s live pty to a second `PtyTerm`, the way a successor process adopts it -
    /// including the screen dump that rides along with the fd. `replay: false` simulates a
    /// predecessor that sent none (a 1.6.2 build, or a blank pane).
    fn adopt_from(
        donor: &mut PtyTerm,
        alt_screen: bool,
        cmd_running: Option<bool>,
        replay: bool,
    ) -> PtyTerm {
        let pgid = poll_term(donor, PtyTerm::shell_pid).expect("no shell pid");
        let (fd, _) = donor.handoff_fd().expect("master fd must be borrowable");
        let owned = fd.try_clone_to_owned().expect("dup the master");
        donor.mark_handed_off();
        let mut opts = crate::test_support::spawn_opts("/bin/sh", &[]);
        opts.profile = None;
        PtyTerm::adopt(
            egui::Context::default(),
            super::Adopted {
                fd: owned,
                cols: donor.cols(),
                rows: donor.rows(),
                pgid: Some(pgid),
                alive: std::time::Duration::from_secs(9),
                alt_screen,
                cmd_running,
                replay: if replay { donor.screen_dump() } else { Vec::new() },
                title_osc: crate::handoff::clamp_title(donor.title_osc()),
                theme_reports: false,
            },
            &opts,
        )
        .expect("adopt")
    }

    #[test]
    fn real_pty_an_adopted_prompt_is_asked_to_repaint_without_a_keystroke() {
        // The reported bug: after a restart the shells were alive but every pane rendered empty
        // until the user pressed Enter. An adopted `Term` starts blank and a shell has no reason to
        // speak, so we ask: `^L` (0x0c) is what makes zle/readline redraw the prompt. Proven by the
        // byte actually arriving at the shell - 0x0c, unechoed, with nothing typed.
        //
        // `cmd_running: Some(false)` is what OSC 133 reports for a shell that finished a command,
        // i.e. the case this must serve. (The probe reads with `dd`, a foreground CHILD, so the tty
        // would report "a command is running" - a real zsh prompt reads with zle, in the shell's own
        // process group, which `real_pty_the_tty_reports_whether_a_command_is_running` covers.)
        let (mut donor, log) = raw_byte_reporter("prompt");
        let heir = adopt_from(&mut donor, false, Some(false), false);
        let got = || std::fs::read_to_string(&log).unwrap_or_default();
        let asked = poll_term(&heir, |_| got().contains("GOT-0c").then_some(()));
        assert!(asked.is_some(), "an adopted prompt must be asked to repaint, got {:?}", got());
        drop(heir);
        reap_probe(&donor);
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn real_pty_an_adopted_full_screen_app_is_never_sent_ctrl_l() {
        // The other half: inside a TUI, `^L` is the APP's byte to interpret (a literal insert in
        // vim's insert mode), so a pane on the alt screen is asked to repaint with SIGWINCH only.
        // Long enough to cover every retry the nudger makes.
        let (mut donor, log) = raw_byte_reporter("tui");
        let heir = adopt_from(&mut donor, true, Some(false), false);
        std::thread::sleep(std::time::Duration::from_millis(2600));
        let seen = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!seen.contains("GOT-"), "nothing may be typed into a TUI, got {seen:?}");
        assert!(heir.is_alt_screen(), "the handed-over alt screen must be entered here too");
        drop(heir);
        reap_probe(&donor);
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn real_pty_a_pane_with_a_command_running_is_never_sent_ctrl_l() {
        // The 1.6.2 regression: a pane running a producing process (a progress loop) came back
        // showing a literal `^L` at the top-left. Nobody consumes 0x0c there - the line discipline
        // ECHOES it, which is what put it on screen - and the pane needed nothing from us anyway:
        // its own next output tick repaints it.
        //
        // The probe keeps a foreground process that never reads stdin (`sleep`), echo left ON exactly
        // like a real shell, so a `^L` we send would show up as the literal the user saw.
        let mut donor = e2e_term("printf ARMED; while :; do printf 'WORK '; sleep 0.3; done");
        assert!(
            poll_term(&donor, |t| grid_text(t).contains("ARMED").then_some(())).is_some(),
            "the probe loop never started"
        );
        // `None` = the hardest case: a predecessor too old to send the field, or a shell with no
        // OSC 133 at all. The tty is asked instead, and the output-based bail-out backs it up.
        let heir = adopt_from(&mut donor, false, None, false);
        std::thread::sleep(std::time::Duration::from_millis(2600)); // past every retry
        let seen = grid_text(&heir);
        assert!(seen.contains("WORK"), "a producing pane repaints itself, got {seen:?}");
        assert!(!seen.contains("^L"), "a running command must never be sent ^L, got {seen:?}");
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_an_adopted_pane_still_shows_what_was_already_on_screen() {
        // The point of a resumable restart: a pane must come back showing the lines it had, not a
        // blank grid that starts filling from the next write. The donor prints distinctive lines and
        // then goes SILENT, so anything in the heir's grid can only have come from the replay - no
        // new output, no keystroke.
        let mut donor = e2e_term(
            "printf 'ALPHA-1\\r\\nBRAVO-2\\r\\nCHARLIE-3\\r\\n'; i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| grid_text(t).contains("CHARLIE-3").then_some(())).is_some(),
            "the donor never printed its lines"
        );
        let heir = adopt_from(&mut donor, false, Some(true), true);
        let seen = grid_text(&heir);
        for line in ["ALPHA-1", "BRAVO-2", "CHARLIE-3"] {
            assert!(seen.contains(line), "{line} must survive the handover, got {seen:?}");
        }
        // ...and on their own rows, in order, not concatenated into one.
        let snap = heir.grid_snapshot();
        let rows: Vec<String> = snap
            .cells
            .chunks(snap.cols)
            .map(|r| r.iter().map(|c| c.c).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(rows[0], "ALPHA-1");
        assert_eq!(rows[1], "BRAVO-2");
        assert_eq!(rows[2], "CHARLIE-3");
        drop(heir);
        reap_probe(&donor);
    }

    /// A donor on a REAL 80x24 pty (the 20x5 `e2e_term` grid is too small to say anything about row
    /// positions or scrollback).
    fn e2e_term_sized(cols: usize, rows: usize, script: &str) -> PtyTerm {
        let opts = crate::test_support::spawn_opts("/bin/sh", &["-c", script]);
        PtyTerm::spawn(cols, rows, egui::Context::default(), &opts)
    }

    /// Row-by-row text of a snapshot, trailing blanks trimmed.
    fn snap_rows(term: &PtyTerm) -> Vec<String> {
        let snap = term.grid_snapshot();
        snap.cells
            .chunks(snap.cols)
            .map(|r| r.iter().map(|c| c.c).collect::<String>().trim_end().to_string())
            .collect()
    }

    /// Full-width rows (no trailing trim) - what the renderer hands the link hit-test.
    fn snap_rows_padded(term: &PtyTerm) -> Vec<String> {
        let snap = term.grid_snapshot();
        snap.cells.chunks(snap.cols).map(|r| r.iter().map(|c| c.c).collect()).collect()
    }

    #[test]
    fn real_pty_a_path_wrapped_by_the_terminal_is_still_one_link() {
        // Grounds the rule `links::link_at` keys on: when the pty wraps a line, the last column of
        // the first row really does hold a glyph and the tail really does start at column 0. Half a
        // path was clickable before this - a narrow window broke every long link.
        let path = "/private/tmp/claude/scratchpad/og/scrobble-start.png";
        let term = e2e_term_sized(
            24,
            6,
            &format!("printf '{path}'; i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done"),
        );
        assert!(
            poll_term(&term, |t| snap_rows(t)[0].starts_with("/private").then_some(())).is_some(),
            "the probe never printed its path"
        );
        let rows = snap_rows_padded(&term);
        assert_eq!(rows[0].chars().count(), 24, "rows arrive padded to the full width");
        assert!(rows[0].chars().next_back().is_some_and(|c| c != ' '), "the wrap fills column 23");
        assert!(rows[1].starts_with("tchpad/og"), "the tail continues at column 0");

        let hit = crate::links::link_at(&rows, 1, 3).expect("the tail must resolve to a link");
        assert_eq!(hit.text, path, "hovering the tail must yield the whole path");
        assert_eq!(hit.spans.len(), 3, "the path covers three rows at 24 columns");
        reap_probe(&term);
    }

    #[test]
    fn real_pty_a_replayed_screen_keeps_every_row_where_it_was() {
        // The "only the last line came back?" question. Content is placed at rows 10-12 of a 24-row
        // grid (blank rows above AND below) with the cursor parked on row 20, so a replay that
        // COMPACTED the picture to the top - or dropped blank rows to save bytes - cannot pass.
        // CUP is 1-based: row 11 = index 10, col 3 = index 2.
        let mut donor = e2e_term_sized(
            80,
            24,
            "printf '\\033[11;3HROW-A\\033[12;3HROW-B\\033[13;3HROW-C\\033[21;6H'; \
             i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| snap_rows(t)
                .get(12)
                .is_some_and(|r| r.contains("ROW-C"))
                .then_some(()))
            .is_some(),
            "the donor never drew its rows"
        );
        let before = snap_rows(&donor);
        let before_cursor = donor.grid_snapshot().cursor;
        assert_eq!(before[10], "  ROW-A", "the probe itself must place row 10");
        assert_eq!(before_cursor, Some((20, 5)), "the probe must park the cursor on row 20");

        let heir = adopt_from(&mut donor, false, Some(true), true);
        assert_eq!(snap_rows(&heir), before, "every row must land where the donor had it");
        assert_eq!(heir.grid_snapshot().cursor, before_cursor, "the cursor must come back too");
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_a_carriage_return_progress_bar_replays_as_the_one_row_it_occupies() {
        // The "looks like only last line?" screenshot, reproduced. A `\r`-overwrite progress bar
        // rewrites ONE row: its earlier percentages never existed as separate rows, not even on the
        // donor. One line at the top with blank below is therefore the faithful answer, not a loss -
        // and this test is what says so, next to the row-position test above which proves that a
        // donor with content lower down comes back lower down.
        let mut donor = e2e_term_sized(
            80,
            24,
            "printf 'dry  [###-------] 12.3%% 8000/68461\\r'; \
             printf 'dry  [######----] 87.6%% 60000/68461 | ETA 8s\\r'; \
             i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| snap_rows(t)[0].contains("87.6%").then_some(())).is_some(),
            "the donor never drew its progress line"
        );
        let before = snap_rows(&donor);
        assert!(
            !before[0].contains("12.3%"),
            "the overwrite already erased the earlier percentage"
        );
        assert!(
            before[1..].iter().all(String::is_empty),
            "the donor's own grid has exactly one non-blank row, got {before:?}"
        );

        let heir = adopt_from(&mut donor, false, Some(true), true);
        assert_eq!(snap_rows(&heir), before, "the replay must be exactly what the donor had");
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_a_replayed_pane_can_be_scrolled_back_into_its_history() {
        // The claim that up to `screen::MAX_HISTORY_LINES` of scrollback survive: the donor prints
        // more lines than fit, so the early ones are already in ITS history, and the heir must be
        // able to scroll up to them - not just show the last screenful.
        let mut donor = e2e_term_sized(
            80,
            24,
            "i=1; while [ $i -le 40 ]; do printf 'LINE-%02d\\r\\n' $i; i=$((i+1)); done; \
             i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| snap_rows(t).iter().any(|r| r == "LINE-40").then_some(()))
                .is_some(),
            "the donor never printed 40 lines"
        );
        // 40 lines on a 24-row grid: 1..16 are in history, 17..40 are on screen.
        assert!(donor.scroll_state().1 >= 16, "the donor must have history to hand over");
        assert!(!snap_rows(&donor).iter().any(|r| r == "LINE-01"), "LINE-01 must be off-screen");

        let heir = adopt_from(&mut donor, false, Some(true), true);
        let (_, history) = heir.scroll_state();
        assert!(history >= 16, "the heir must have inherited scrollback, got {history} lines");
        let buffer: Vec<String> = heir.buffer_lines().into_iter().map(|(_, s)| s).collect();
        for want in ["LINE-01", "LINE-16", "LINE-17", "LINE-40"] {
            assert!(buffer.iter().any(|l| l == want), "{want} missing from the heir's buffer");
        }
        // ...and it is really HISTORY: scrolling up brings the early lines into view.
        heir.scroll(20);
        assert!(
            snap_rows(&heir).iter().any(|r| r == "LINE-01"),
            "scrolling up must reveal the oldest handed-over line"
        );
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_replayed_scrollback_stops_at_the_documented_cap() {
        // "Up to 200 lines of scrollback survive" is a promise made to users, so pin the actual
        // number: a donor with FAR more history hands over its newest `MAX_HISTORY_LINES` and drops
        // the rest, rather than either shipping 25k lines or quietly sending none.
        let mut donor = e2e_term_sized(
            80,
            24,
            "i=1; while [ $i -le 400 ]; do printf 'LINE-%03d\\r\\n' $i; i=$((i+1)); done; \
             i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| snap_rows(t).iter().any(|r| r == "LINE-400").then_some(()))
                .is_some(),
            "the donor never printed 400 lines"
        );
        assert!(donor.scroll_state().1 > 300, "the donor must have far more history than the cap");

        let heir = adopt_from(&mut donor, false, Some(true), true);
        let history = heir.scroll_state().1;
        assert_eq!(
            history,
            crate::screen::MAX_HISTORY_LINES,
            "the heir must inherit exactly the capped number of scrollback lines"
        );
        let buffer: Vec<String> = heir.buffer_lines().into_iter().map(|(_, s)| s).collect();
        assert!(buffer.iter().any(|l| l == "LINE-400"), "the newest line must survive");
        assert!(buffer.iter().any(|l| l == "LINE-200"), "well within the cap");
        assert!(!buffer.iter().any(|l| l == "LINE-001"), "beyond the cap must be dropped");
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_a_replayed_screen_keeps_its_colors_and_bold() {
        // The dump carries RAW cell colors and the style flags, so the heir re-renders them through
        // its own theme. Asserted against the DONOR's snapshot, cell for cell.
        let mut donor = e2e_term(
            "printf 'p\\033[1;31mRED\\033[0m\\033[44mBLU\\033[0m\\r\\n'; i=0; while [ $i -lt 40 ]; do sleep 1; i=$((i+1)); done",
        );
        assert!(
            poll_term(&donor, |t| grid_text(t).contains("RED").then_some(())).is_some(),
            "the donor never printed its colored line"
        );
        let before = donor.grid_snapshot();
        let heir = adopt_from(&mut donor, false, Some(true), true);
        let after = heir.grid_snapshot();
        let cells =
            |snap: &super::GridSnap| -> Vec<(char, egui::Color32, Option<egui::Color32>, bool)> {
                snap.cells[..12].iter().map(|c| (c.c, c.fg, c.bg, c.bold)).collect()
            };
        assert_eq!(cells(&after), cells(&before), "glyphs, colors and bold must all round-trip");
        drop(heir);
        reap_probe(&donor);
    }

    #[test]
    fn real_pty_the_tty_reports_whether_a_command_is_running() {
        // The signal that resolves the unknown case, measured rather than assumed: with job control a
        // foreground job gets its OWN process group, so the tty's foreground group is the shell's own
        // only at a prompt. `read` is a builtin (no child), `sleep` is not.
        let busy = e2e_term("printf ARMED; while :; do printf 'W '; sleep 0.3; done");
        let idle = e2e_term("printf ARMED; while :; do read line; done");
        for t in [&busy, &idle] {
            assert!(poll_term(t, |t| grid_text(t).contains("ARMED").then_some(())).is_some());
        }
        let ask = |t: &PtyTerm| {
            let fd = t.pty.as_fd().expect("a spawned pty has a master fd");
            poll_term(t, |t| super::foreground_command(fd, t.shell_pid()))
        };
        assert_eq!(ask(&busy), Some(true), "a foreground `sleep` means a command is running");
        assert_eq!(ask(&idle), Some(false), "a shell blocked in a builtin is at its prompt");
        // A pipe is not a tty, so there is nothing to ask - the caller must fall back.
        let (rx, _tx) = std::io::pipe().unwrap();
        assert_eq!(super::foreground_command(std::os::fd::AsFd::as_fd(&rx), Some(1)), None);
    }

    #[test]
    fn real_pty_adopted_fd_streams_output_and_reports_the_pgid() {
        // Hand a LIVE pty's master fd to a second PtyTerm and prove the adopting side is a working
        // terminal: it sees output the shell writes after the handover, and it carries the process
        // group across (we are not the shell's parent, so the pgid can only come from the wire).
        // Ticks CONTINUOUSLY: the donor's reader thread is still alive and competing for bytes
        // (see `adopt`), so a single line could be swallowed by it - a stream cannot be.
        let mut donor = e2e_term("while :; do printf 'TICK\\n'; sleep 0.2; done");
        let pgid = poll_term(&donor, PtyTerm::shell_pid).expect("no shell pid");
        let (fd, carried) = donor.handoff_fd().expect("master fd must be borrowable");
        assert_eq!(carried, Some(pgid), "the pgid must travel with the fd");

        let owned = fd.try_clone_to_owned().expect("dup the master");
        donor.mark_handed_off(); // the donor must not reap what it just gave away

        let mut opts = crate::test_support::spawn_opts("/bin/sh", &[]);
        opts.profile = None;
        let heir = PtyTerm::adopt(
            egui::Context::default(),
            super::Adopted {
                fd: owned,
                cols: 20,
                rows: 5,
                pgid: Some(pgid),
                alive: std::time::Duration::from_secs(42),
                alt_screen: false,
                cmd_running: Some(false),
                replay: Vec::new(),
                title_osc: None,
                theme_reports: false,
            },
            &opts,
        )
        .expect("adopt");

        // Seeing a tick that was written AFTER the handover proves the adopted fd is live.
        let seen = poll_term(&heir, |t| {
            let snap = t.grid_snapshot();
            let text: String = snap.cells.iter().map(|c| c.c).collect();
            text.contains("TICK").then_some(())
        });
        assert!(seen.is_some(), "adopted pty must stream the shell's ongoing output");
        assert_eq!(heir.shell_pid(), Some(pgid));
        drop(heir);
        donor.kill(); // disarmed, so clean up explicitly
        #[allow(unsafe_code)] // SAFETY: thin FFI, checked pid
        unsafe {
            libc::killpg(pgid as i32, libc::SIGKILL)
        };
    }

    #[test]
    fn real_pty_osc_title_propagates() {
        let title =
            spawn_and_poll("printf '\\033]0;from-the-shell\\007'; sleep 5", PtyTerm::title_osc);
        assert_eq!(title.as_deref(), Some("from-the-shell"));
    }

    #[test]
    fn real_pty_clear_all_wipes_history_and_viewport() {
        // 200 lines on a 5-row grid build real history; the full wipe must drop it all AND
        // blank the viewport (a following shell ESC[2J would scroll leftovers into history).
        let term = e2e_term("seq 1 200; sleep 5");
        poll_term(&term, |t| (t.scroll_state().1 >= 195).then_some(()))
            .expect("history never filled");
        assert!(term.clear_all(), "primary-screen wipe must be accepted");
        assert_eq!(term.scroll_state(), (0, 0));
        let snap = term.grid_snapshot();
        assert!(
            snap.cells.iter().all(|c| c.c == ' ' || c.c == '\0'),
            "viewport must be blank after the wipe"
        );
    }

    #[test]
    fn real_pty_clear_scrollback_keeps_the_screen() {
        // The history-only wipe drops the scrollback but leaves the visible rows untouched.
        let term = e2e_term("seq 1 200; sleep 5");
        poll_term(&term, |t| (t.scroll_state().1 >= 195).then_some(()))
            .expect("history never filled");
        term.clear_scrollback();
        assert_eq!(term.scroll_state(), (0, 0));
        let snap = term.grid_snapshot();
        assert!(
            snap.cells.iter().any(|c| c.c != ' ' && c.c != '\0'),
            "viewport content must survive a scrollback-only wipe"
        );
    }

    #[test]
    fn real_pty_clear_all_is_refused_on_the_alt_screen() {
        // `ESC[?1049h` enters the alt screen (vim/less territory): the wipe must refuse -
        // the app owns that grid, and the follow-up Ctrl-L would land in its input.
        let term = e2e_term("printf '\\033[?1049hEDITOR'; sleep 5");
        poll_term(&term, |t| {
            (t.is_alt_screen() && t.grid_snapshot().cells.iter().any(|c| c.c == 'E')).then_some(())
        })
        .expect("alt screen never entered");
        assert!(!term.clear_all(), "alt-screen wipe must be refused");
        let snap = term.grid_snapshot();
        assert!(
            snap.cells.iter().any(|c| c.c == 'E'),
            "alt-screen content must be untouched by a refused wipe"
        );
    }

    #[test]
    fn real_pty_output_sets_activity() {
        // Any output flags activity; take_activity consumes it (one-shot until more output).
        let term = e2e_term("printf 'hello'; sleep 5");
        poll_term(&term, |t| t.take_activity().then_some(())).expect("activity never flagged");
        assert!(!term.take_activity(), "take_activity must consume the flag");
    }

    #[test]
    fn real_pty_tracks_mouse_modes_and_wheel_sgr_round_trips() {
        // The app enables normal mouse tracking (?1000h) + SGR extended coords (?1006h): both
        // must show up in `mouse_reporting()`. Then a wheel-up SGR report we send must survive
        // the pty round-trip - `head` echoes it back with ESC mapped to 'E' so it lands in the
        // grid (raw ESC would be parsed as a control sequence, see the OSC 11 test). `head -c`
        // is sized to the exact report length (10 bytes) so head EOFs and flushes its stdio
        // buffer - a larger count would block-buffer forever on a short reply.
        let mut term = e2e_term(
            "stty raw -echo; printf '\\033[?1000h\\033[?1006h'; head -c 10 | tr '\\033' 'E'; \
             sleep 5",
        );
        poll_term(&term, |t| t.mouse_reporting().report_click.then_some(()))
            .expect("mouse reporting mode never tracked");
        let mr = term.mouse_reporting();
        assert!(mr.report_click && mr.sgr, "?1000 + ?1006 must both be tracked: {mr:?}");
        assert!(mr.reports_buttons());
        term.send(&wheel_sgr(1, 2, 4)); // wheel up at cell (2,4) -> ESC[<64;3;5M
        poll_term(&term, |t| grid_text(t).contains("E[<64;3;5M").then_some(()))
            .expect("wheel SGR report never reached the app");
    }

    #[test]
    fn real_pty_theme_reports_reach_an_app_that_enabled_them() {
        // The app enables mode 2031, reads exactly one report (9 bytes, ESC shown as 'E' so it
        // lands in the grid), then disables the mode again.
        let mut term = e2e_term(
            "stty raw -echo; printf '\\033[?2031h'; head -c 9 | tr '\\033' 'E'; \
             printf '\\033[?2031l'; sleep 5",
        );
        poll_term(&term, |t| t.theme_reports().then_some(())).expect("mode 2031 never tracked");
        term.report_theme(false);
        poll_term(&term, |t| grid_text(t).contains("E[?997;2n").then_some(()))
            .expect("the light report never reached the app");
        poll_term(&term, |t| (!t.theme_reports()).then_some(()))
            .expect("the app's reset of mode 2031 was missed");
    }

    #[test]
    fn snap_glyph_maps_wide_and_spacer_flags() {
        // (char, flags) -> (drawn char, wide)
        let cases = [
            ('a', Flags::empty(), ('a', false)),
            ('你', Flags::WIDE_CHAR, ('你', true)),
            (' ', Flags::WIDE_CHAR_SPACER, ('\0', false)),
            (' ', Flags::LEADING_WIDE_CHAR_SPACER, ('\0', false)),
            ('b', Flags::BOLD, ('b', false)), // unrelated flags don't mark wide
        ];
        for (c, flags, want) in cases {
            assert_eq!(snap_glyph(c, flags), want, "{c:?} {flags:?}");
        }
    }

    #[test]
    fn real_pty_snapshot_carries_the_bold_flag() {
        // SGR 1 marks cells bold in the snapshot (the renderer's real-bold-face switch);
        // the flag is raw - independent of the bold_bright color treatment (off here).
        let got = spawn_and_poll("printf 'p \\033[1mB\\033[0m\\n'; sleep 5", |t| {
            let snap = t.grid_snapshot();
            let p = snap.cells.iter().position(|c| c.c == 'p')?;
            let b = snap.cells.iter().position(|c| c.c == 'B')?;
            Some((snap.cells[p].bold, snap.cells[b].bold))
        })
        .expect("bold output never hit the grid");
        assert_eq!(got, (false, true));
    }

    /// Row-major grid text (skips wide-char spacers) - lets `contains` match strings that
    /// wrap across rows, since wrapped output is contiguous in row-major order.
    fn grid_text(term: &PtyTerm) -> String {
        term.grid_snapshot().cells.iter().map(|c| c.c).filter(|c| *c != '\0').collect()
    }

    #[test]
    fn real_pty_osc_11_query_answers_with_the_theme_bg() {
        // The OSC 11 background query is how gemini/copilot detect light vs dark; unanswered,
        // they assume a dark terminal and render unreadable colors on a light theme. The
        // reply must encode the LIVE theme bg in X-color format. The script goes raw first
        // (like any querying TUI - canonical mode would hold the reply until a newline),
        // then echoes the 24-byte reply back with ESC mapped to 'E' so it lands in the grid.
        let bg = crate::colors::bg();
        let want = format!(
            "E]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}",
            bg.r(),
            bg.g(),
            bg.b()
        );
        let script =
            "stty raw -echo; printf '\\033]11;?\\007'; head -c 24 | tr '\\033' 'E'; sleep 5";
        let got = spawn_and_poll(script, |t| grid_text(t).contains(&want).then_some(()));
        assert!(got.is_some(), "OSC 11 reply must carry the theme bg ({want})");
    }

    #[test]
    fn real_pty_da_and_dsr_queries_are_answered() {
        // DA1 (CSI c) and DSR 6 (cursor position) are probe-and-wait queries; TUIs stall or
        // mis-fall-back when they stay silent (they used to be dropped with every other
        // `Event::PtyWrite`). Echo trick as above: the DA1 reply is exactly 5 bytes; after
        // echoing it the cursor sits at column 6, so the DSR reply is exactly `ESC[1;6R`.
        let script = "stty raw -echo; printf '\\033[c'; head -c 5 | tr '\\033' 'E'; \
                      printf '\\033[6n'; head -c 6 | tr '\\033' 'E'; sleep 5";
        let got = spawn_and_poll(script, |t| {
            let text = grid_text(t);
            (text.contains("E[?6c") && text.contains("E[1;6R")).then_some(())
        });
        assert!(got.is_some(), "DA1 + DSR replies must reach the app");
    }

    #[test]
    fn real_pty_title_stack_pop_restores_the_previous_title() {
        // copilot sets its title via OSC 0 but RESTORES it via the xterm title stack
        // (CSI 22;0t push / 23;0t pop), which only the Term's Title events see - the old
        // OSC-scanner-only path left "GitHub Copilot" stuck on the tab forever.
        let term = e2e_term(
            "printf '\\033]0;before\\007'; sleep 1; \
             printf '\\033[22;0t\\033]0;GitHub Copilot\\007'; sleep 1; \
             printf '\\033[23;0t'; sleep 5",
        );
        poll_term(&term, |t| (t.title_osc().as_deref() == Some("GitHub Copilot")).then_some(()))
            .expect("the app title never applied");
        poll_term(&term, |t| (t.title_osc().as_deref() == Some("before")).then_some(()))
            .expect("title stack pop must restore the pre-app title");
    }

    #[test]
    fn real_pty_hidden_cursor_is_absent_from_the_snapshot() {
        // DECTCEM hide (CSI ?25l) must yield `cursor: None` - the renderer used to paint a
        // cursor over TUIs that hid their own.
        let term = e2e_term("printf '\\033[?25lX'; sleep 5");
        poll_term(&term, |t| {
            let snap = t.grid_snapshot();
            (snap.cells.iter().any(|c| c.c == 'X') && snap.cursor.is_none()).then_some(())
        })
        .expect("hidden cursor must clear the snapshot cursor");
    }

    #[test]
    fn real_pty_faint_text_is_marked_dim_in_the_snapshot() {
        // SGR 2 (faint) is how TUIs draw hint / ghost text (Claude Code's suggestions). The
        // flag used to be dropped on the floor, so faint text rendered at full brightness.
        // `N` is normal, `D` is faint, and SGR 22 must cancel it again (`E` normal).
        let term = e2e_term("printf 'N\\033[2mD\\033[22mE'; sleep 5");
        poll_term(&term, |t| {
            let snap = t.grid_snapshot();
            let at = |ch: char| snap.cells.iter().find(|c| c.c == ch).map(|c| c.dim);
            match (at('N'), at('D'), at('E')) {
                (Some(false), Some(true), Some(false)) => Some(()),
                _ => None,
            }
        })
        .expect("SGR 2 must set dim, SGR 22 must clear it");
    }

    #[test]
    fn real_pty_prompt_mark_heals_a_leaked_alt_screen_and_cursor() {
        // A TUI killed without cleanup leaves the alt screen + a hidden cursor behind and
        // the pane looks frozen. The next prompt mark (OSC 133;A) proves the shell owns the
        // pty again: both leaks must reset and the pane recover.
        let term = e2e_term(
            "printf '\\033[?1049h\\033[?25lFAKEUI'; sleep 1; printf '\\033]133;A\\007'; sleep 5",
        );
        poll_term(&term, |t| {
            (t.is_alt_screen() && t.grid_snapshot().cursor.is_none()).then_some(())
        })
        .expect("the fake TUI never took the alt screen");
        poll_term(&term, |t| (!t.is_alt_screen()).then_some(()))
            .expect("prompt mark must leave the leaked alt screen");
        let snap = term.grid_snapshot();
        assert!(snap.cursor.is_some(), "prompt mark must restore the hidden cursor");
        assert!(
            !grid_text(&term).contains("FAKEUI"),
            "the dead TUI's frame must be gone with the alt screen"
        );
    }

    #[test]
    fn real_pty_prompt_mark_without_leaks_is_a_noop() {
        // The heal fires only on leaked state: a prompt mark on a healthy primary screen
        // leaves the grid alone (no swap, no redraw request).
        let term = e2e_term("printf 'ok\\033]133;A\\007'; sleep 5");
        poll_term(&term, |t| grid_text(t).contains("ok").then_some(()))
            .expect("output never landed");
        assert!(!term.is_alt_screen());
        assert!(term.grid_snapshot().cursor.is_some());
        assert!(grid_text(&term).contains("ok"), "healthy grid must be untouched");
    }

    #[test]
    fn real_pty_vim_enter_exit_leaves_modes_clean() {
        // Real-TUI sanity sweep: vim enters the alt screen and must leave everything clean
        // on a NORMAL exit (no heal involved - its own rmcup/cnorm do the work).
        let term = e2e_term("vim -u NONE +q; printf 'VIMDONE'; sleep 5");
        poll_term(&term, |t| grid_text(t).contains("VIMDONE").then_some(()))
            .expect("vim never ran/exited");
        assert!(!term.is_alt_screen(), "vim must leave the alt screen");
        assert!(term.grid_snapshot().cursor.is_some(), "cursor must be visible after vim");
    }

    #[test]
    fn real_pty_less_enter_exit_leaves_modes_clean() {
        // Same sweep for a pager: full-screen less takes the alt screen; `q` must restore it.
        let mut term = e2e_term("seq 200 | less; printf 'LESSDONE'; sleep 5");
        poll_term(&term, |t| t.is_alt_screen().then_some(()))
            .expect("less never took the alt screen");
        term.send(b"q");
        poll_term(&term, |t| {
            (!t.is_alt_screen() && grid_text(t).contains("LESSDONE")).then_some(())
        })
        .expect("less must exit cleanly on q");
        assert!(term.grid_snapshot().cursor.is_some(), "cursor must be visible after less");
    }

    #[test]
    fn real_pty_snapshot_marks_cjk_and_emoji_wide() {
        // A real shell printing CJK + emoji: the snapshot must mark each wide glyph and blank
        // its spacer cell, so the renderer can span the glyph across two cells without overlap.
        let got = spawn_and_poll("printf '\u{4f60}\u{597d} \u{1f600}\\n'; sleep 5", |t| {
            let snap = t.grid_snapshot();
            let i = snap.cells.iter().position(|c| c.c == '\u{4f60}')?; // 你
            let e = snap.cells.iter().position(|c| c.c == '\u{1f600}')?; // 😀
            Some((
                snap.cells[i].wide,
                snap.cells[i + 1].c, // 你's spacer
                snap.cells[i + 2].c, // 好
                snap.cells[i + 2].wide,
                snap.cells[e].wide,
                snap.cells[e + 1].c, // 😀's spacer
            ))
        })
        .expect("wide glyphs never hit the grid");
        assert_eq!(got, (true, '\0', '\u{597d}', true, true, '\0'));
    }

    #[test]
    fn repaint_coalesce_window_is_imperceptible_but_nonzero() {
        // A zero window reintroduces the per-chunk mid-burst flicker; a large one adds
        // perceptible input->paint lag. Keep it inside one 60Hz frame.
        assert!(!REPAINT_COALESCE_WINDOW.is_zero(), "zero disables burst coalescing");
        assert!(
            REPAINT_COALESCE_WINDOW <= std::time::Duration::from_millis(16),
            "window must stay under a 60Hz frame to be imperceptible"
        );
    }

    #[test]
    fn echo_wait_is_nonzero_and_under_a_120hz_frame() {
        // Zero brings back the pre-echo frame the echo queues behind; a bound past one 120Hz frame
        // would itself cost a vsync on every key that produces no output.
        assert!(!ECHO_WAIT.is_zero(), "zero disables the echo wait");
        assert!(
            ECHO_WAIT < std::time::Duration::from_micros(8333),
            "the bound must stay under a 120Hz frame"
        );
    }

    #[test]
    fn real_pty_the_echo_is_on_the_grid_when_the_wait_returns() {
        // The regression: the frame that sent a key painted the grid from BEFORE the echo, and the
        // echo frame then queued behind it for 1-3 vsyncs. The wait only helps if the grid already
        // holds the echo the moment it reports output - proven here with the tty's own echo.
        let mut term = e2e_term("printf READY; exec cat >/dev/null");
        assert!(
            poll_term(&term, |t| grid_text(t).contains("READY").then_some(())).is_some(),
            "the probe never started"
        );
        let since = term.output_gen();
        term.send(b"Q");
        assert!(
            term.wait_for_output_after(since, std::time::Duration::from_secs(5)),
            "no echo within 5s"
        );
        assert!(
            grid_text(&term).contains("READYQ"),
            "the echo was not on the grid when the wait returned: {:?}",
            grid_text(&term)
        );
    }

    #[test]
    fn real_pty_the_echo_wait_gives_up_at_its_bound_without_output() {
        // Keys that produce nothing (echo off, a TUI that ignores them) must cost at most the bound.
        let mut term = e2e_term("stty -echo; printf READY; exec cat >/dev/null");
        assert!(
            poll_term(&term, |t| grid_text(t).contains("READY").then_some(())).is_some(),
            "the probe never started"
        );
        let since = term.output_gen();
        term.send(b"Q");
        let bound = std::time::Duration::from_millis(50);
        let start = std::time::Instant::now();
        assert!(!term.wait_for_output_after(since, bound), "an unechoed key reported output");
        let took = start.elapsed();
        assert!(
            took >= bound && took < std::time::Duration::from_secs(1),
            "the wait must end at its bound, took {took:?}"
        );
    }

    // --- Pending resume input ------------------------------------------------------------------

    /// Spawn options for a `/bin/sh -c script` pane with `input` queued for its first prompt and
    /// `fallback` as the wait before the fallback may type it (the pane has no integration).
    fn pending_opts(
        script: &str,
        input: Option<&[u8]>,
        fallback: std::time::Duration,
    ) -> SpawnOpts {
        let mut opts = crate::test_support::spawn_opts("/bin/sh", &["-c", script]);
        opts.agent_tracking = Some(crate::agent_pane::AgentSetup {
            pending_input: input.map(<[u8]>::to_vec),
            fallback_delay: fallback,
        });
        opts
    }

    /// Wait until a command, and not the shell, owns the tty foreground. A printed marker does not
    /// say that: the shell prints it before it forks the command.
    fn wait_until_a_command_owns_the_tty(term: &PtyTerm) {
        let busy = |t: &PtyTerm| t.foreground_pgid().is_some_and(|fg| Some(fg) != t.shell_pid());
        poll_term(term, |t| busy(t).then_some(())).expect("no command took the tty");
    }

    fn pending_term(script: &str, input: &[u8], fallback: std::time::Duration) -> PtyTerm {
        let opts = pending_opts(script, Some(input), fallback);
        PtyTerm::spawn(60, 5, egui::Context::default(), &opts)
    }

    fn count_in(text: &str, needle: &str) -> usize {
        text.matches(needle).count()
    }

    #[test]
    fn real_pty_pending_input_waits_for_the_first_prompt_mark_and_types_once() {
        // The shell is "starting up" for 0.8 s, then marks its prompt twice (OSC 133;A). The queued
        // text must not appear before the first mark, and must appear exactly once after both
        // (the tty echoes input while a shell builtin keeps ownership).
        let term = pending_term(
            "sleep 0.8; printf '\\033]133;A\\007READY'; read -t 0.3 line; printf '\\033]133;A\\007AGAIN'; while :; do read line; done",
            b"resume-me",
            std::time::Duration::from_secs(60),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!grid_text(&term).contains("resume-me"), "typed before the first prompt");
        poll_term(&term, |t| grid_text(t).contains("AGAIN").then_some(()))
            .expect("the second prompt mark never arrived");
        assert_eq!(count_in(&grid_text(&term), "resume-me"), 1, "{:?}", grid_text(&term));
    }

    #[test]
    fn real_pty_the_fallback_types_at_an_idle_shell_once_the_wait_is_over() {
        // No integration, so no prompt mark. A builtin `read` keeps the shell itself in the
        // foreground, which is what a shell at its prompt looks like to the tty.
        let mut term = pending_term(
            "printf ARMED; while :; do read line; done",
            b"via-fallback",
            std::time::Duration::from_millis(600),
        );
        poll_term(&term, |t| grid_text(t).contains("ARMED").then_some(())).unwrap();
        term.release_pending_after_wait();
        assert!(!grid_text(&term).contains("via-fallback"), "the fallback fired early");
        std::thread::sleep(std::time::Duration::from_millis(700));
        term.release_pending_after_wait();
        poll_term(&term, |t| grid_text(t).contains("via-fallback").then_some(()))
            .expect("the fallback never typed the input");
        term.release_pending_after_wait(); // a second call must not type again
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(count_in(&grid_text(&term), "via-fallback"), 1);
    }

    #[test]
    fn real_pty_the_fallback_drops_the_input_when_a_command_owns_the_tty() {
        // A foreground `sleep` runs in its own process group, like `vim` or `ssh` that the user
        // started during the wait. Typing there would feed the command to that program.
        let mut term =
            pending_term("while :; do sleep 30; done", b"never-typed", std::time::Duration::ZERO);
        wait_until_a_command_owns_the_tty(&term);
        term.release_pending_after_wait();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!grid_text(&term).contains("never-typed"), "{:?}", grid_text(&term));
        let queue = term.agent.as_ref().unwrap().pending();
        assert!(queue.take().is_none(), "the input is dropped, not kept for later");
    }

    #[test]
    fn real_pty_user_input_drops_the_pending_command() {
        let mut term = pending_term(
            "printf ARMED; while :; do read line; done",
            b"resume-me",
            std::time::Duration::from_millis(200),
        );
        poll_term(&term, |t| grid_text(t).contains("ARMED").then_some(())).unwrap();
        term.send(b"x");
        std::thread::sleep(std::time::Duration::from_millis(400));
        term.release_pending_after_wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let text = grid_text(&term);
        assert!(text.contains('x') && !text.contains("resume-me"), "{text:?}");
    }

    #[test]
    fn real_pty_a_pane_without_pending_input_types_nothing() {
        let opts = pending_opts(
            "printf '\\033]133;A\\007QUIET'; cat",
            None,
            std::time::Duration::from_millis(100),
        );
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        poll_term(&term, |t| grid_text(t).contains("QUIET").then_some(())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        term.release_pending_after_wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            grid_text(&term).trim_end(),
            "QUIET",
            "nothing may be typed into an ordinary pane"
        );
    }

    #[test]
    fn real_pty_a_startup_child_prompt_keeps_resume_queued_for_the_shell() {
        let fixture = crate::test_support::ShellFixture::new("foreign-prompt");
        std::fs::write(
            fixture.home.join("startup.sh"),
            "printf '\\033]133;A\\007CHILD_READY'; read -t 1 line; printf '%s' \"$line\" > \"$HOME/received\"\n",
        ).unwrap();
        std::fs::write(fixture.home.join(".zshrc"), "/bin/sh \"$HOME/startup.sh\"\n").unwrap();
        let mut opts = fixture.zsh_opts();
        opts.agent_tracking = Some(crate::agent_pane::AgentSetup {
            pending_input: Some(b"printf resumed > \"$HOME/resumed\"\r".to_vec()),
            fallback_delay: std::time::Duration::from_secs(60),
        });
        let term = PtyTerm::spawn(120, 5, egui::Context::default(), &opts);
        poll_term(&term, |_| fixture.home.join("received").exists().then_some(()))
            .expect("startup child did not finish");
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("received")).unwrap(),
            "",
            "the startup child received the resume command"
        );
        poll_term(&term, |_| fixture.home.join("resumed").exists().then_some(()))
            .expect("the shell did not receive the queued command");
        assert_eq!(std::fs::read_to_string(fixture.home.join("resumed")).unwrap(), "resumed");
    }

    #[test]
    fn real_pty_zsh_shows_the_pending_command_after_its_first_prompt() {
        // The whole restore path in a real zsh with our integration bridge: the command is queued
        // before the shell starts, the first OSC 133;A releases it, and zsh's line editor shows it.
        // (`prefill`: no Enter, so nothing runs and the text stays on the prompt line.) The
        // fallback delay is huge, so only the prompt mark can have typed it.
        let fixture = crate::test_support::ShellFixture::new("zshpending");
        let mut opts = fixture.zsh_opts();
        opts.agent_tracking = Some(crate::agent_pane::AgentSetup {
            pending_input: Some(b"claude --resume 0c2cbc96-1111-4222-8333-444455556666".to_vec()),
            fallback_delay: std::time::Duration::from_secs(60),
        });
        let term = PtyTerm::spawn(120, 5, egui::Context::default(), &opts);
        let shown = poll_term(&term, |t| {
            grid_text(t).contains("--resume 0c2cbc96-1111-4222-8333-444455556666").then_some(())
        });
        assert!(shown.is_some(), "zsh never showed the pending command: {:?}", grid_text(&term));
    }

    /// A real integrated zsh following an agent record, with its fixture and repeatable scan.
    fn zsh_agent_pane(
        tag: &str,
    ) -> (PtyTerm, crate::test_support::ShellFixture, crate::agents::Seen) {
        use crate::agents::{AgentKind, AgentSession, Seen, SessionId};
        let fixture = crate::test_support::ShellFixture::new(tag);
        let mut opts = fixture.zsh_opts();
        opts.agent_tracking = Some(crate::agent_pane::AgentSetup::default());
        let mut term = PtyTerm::spawn(100, 5, egui::Context::default(), &opts);
        // The first prompt is up once zsh has printed anything after its rc files ran.
        poll_term(&term, |t| (!grid_text(t).trim().is_empty()).then_some(())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        // A restored record, bound by the scan that finds its resume command. Later scans of the
        // agent are silent, as Codex is before its first turn: only a record that is kept can
        // survive them.
        let id = SessionId::parse("0c2cbc96-1111-4222-8333-444455556666").unwrap();
        let session = AgentSession {
            kind: AgentKind::Claude,
            id: id.clone(),
            cwd: "/w".into(),
            transcript: None,
        };
        term.restore_agent(session, false);
        let bind = Seen::Agent {
            kind: AgentKind::Claude,
            pid: 4242,
            session: None,
            argv_session: Some(id),
        };
        term.step_agent(bind, std::time::Instant::now());
        let seen =
            Seen::Agent { kind: AgentKind::Claude, pid: 4242, session: None, argv_session: None };
        (term, fixture, seen)
    }

    /// Wait until the shell has reported a status that no scan took yet.
    fn wait_for_status(term: &PtyTerm) {
        poll_term(term, |t| t.state.lock().unwrap().first_status.is_some().then_some(()))
            .expect("the shell never reported its status");
    }

    /// Run `command` in a real zsh, in a pane whose record follows an agent. Then let the scans
    /// find the agent gone, and say if the record survived. The command stands in for the agent
    /// process.
    fn record_survives_command(command: &str) -> bool {
        let (mut term, _fixture, _) = zsh_agent_pane(&command.len().to_string());
        term.send(format!("{command}\r").as_bytes());
        // The shell reports the status (OSC 133;D) at its next prompt.
        wait_for_status(&term);
        for _ in 0..4 {
            term.step_agent(crate::agents::Seen::NoAgent, std::time::Instant::now());
        }
        term.agent_saved().is_some()
    }

    #[test]
    fn real_pty_zsh_reports_a_kill_status_and_the_record_survives_only_a_crash() {
        // The zsh hook prints `133;D;$?`, and zsh turns a signal death into 128 plus the signal.
        // SIGKILL is 137 (a crash: kept). SIGTERM is 143 and a plain exit is 3 (dropped).
        assert!(record_survives_command("sh -c 'kill -9 $$'"), "SIGKILL must keep the record");
        assert!(!record_survives_command("sh -c 'kill -15 $$;'"), "SIGTERM must drop it");
        assert!(!record_survives_command("sh -c 'exit 3'"), "a plain exit must drop it");
    }

    #[test]
    fn real_pty_zsh_ctrl_z_and_later_commands_do_not_end_a_live_agents_record() {
        // Ctrl+Z stops the foreground job and the shell reports a status for it (146 on macOS).
        // Every later command reports one too. The agent still lives, so a scan after them shows
        // it and the record stays. `sleep` stands in for the agent process.
        let (mut term, _fixture, alive) = zsh_agent_pane("ctrlz");
        term.send(b"sleep 30\r");
        std::thread::sleep(std::time::Duration::from_millis(500));
        term.send(b"\x1a");
        wait_for_status(&term);
        // The scan after the status still sees the agent, so the status was no exit.
        term.step_agent(alive.clone(), std::time::Instant::now());
        assert_eq!(term.agent_saved().map(|a| a.crashed), Some(false), "after Ctrl+Z");
        term.send(b"true\r");
        wait_for_status(&term);
        term.step_agent(alive.clone(), std::time::Instant::now());
        assert_eq!(term.agent_saved().map(|a| a.crashed), Some(false), "after Ctrl+Z and true");
        // `fg` reports nothing until the job ends. Ctrl+C then ends it, and the agent is gone.
        term.send(b"fg\r");
        std::thread::sleep(std::time::Duration::from_millis(500));
        term.step_agent(alive, std::time::Instant::now());
        assert_eq!(term.agent_saved().map(|a| a.crashed), Some(false), "after fg");
        term.send(b"\x03");
        wait_for_status(&term);
        for _ in 0..2 {
            term.step_agent(crate::agents::Seen::NoAgent, std::time::Instant::now());
        }
        assert!(term.agent_saved().is_none(), "the agent ended, so its record ends");
    }

    /// The statuses a real bash with our integration reports for `keys`, typed one at a time.
    /// Waits `settle` after each key so the prompt (and its status mark) has come back, and takes
    /// what the shell reported for that key.
    fn bash_reported_statuses(keys: &[&str]) -> Vec<i32> {
        bash_with_rc_statuses("", keys)
    }

    /// The same, with `user_rc` as the user's `~/.bash_profile` that our bashrc sources.
    fn bash_with_rc_statuses(user_rc: &str, keys: &[&str]) -> Vec<i32> {
        let fixture = crate::test_support::ShellFixture::new("bashexit");
        std::fs::write(fixture.home.join(".bash_profile"), user_rc).unwrap();
        let opts = fixture.bash_opts();
        let mut term = PtyTerm::spawn(100, 5, egui::Context::default(), &opts);
        poll_term(&term, |t| grid_text(t).contains('$').then_some(())).expect("no bash prompt");
        let settle = std::time::Duration::from_millis(400);
        std::thread::sleep(settle);
        let mut got = Vec::new();
        for key in keys {
            term.send(format!("{key}\r").as_bytes());
            std::thread::sleep(settle);
            got.extend(term.take_status().map(|s| s.code));
        }
        got
    }

    #[test]
    fn real_pty_bash_reports_a_status_only_after_a_command_ran() {
        // The first prompt and an empty Enter report nothing, as in zsh. A repeated command that
        // `HISTCONTROL` keeps out of the history still reports when its status differs.
        assert_eq!(
            bash_reported_statuses(&["", "", "true", "", "false", "", "true"]),
            vec![0, 1, 0]
        );
        assert_eq!(bash_reported_statuses(&["", ""]), Vec::<i32>::new());
        assert_eq!(bash_reported_statuses(&["sh -c 'kill -9 $$'", ""]), vec![137]);
    }

    #[test]
    fn real_pty_a_crash_status_no_scan_has_seen_is_applied_before_the_final_save() {
        // An out-of-memory kill (137) a second before Cmd+Q: the snapshot must save the pane's
        // session as crashed, not as a normal record that would auto-run.
        use crate::agents::{AgentKind, AgentSession, Seen, SessionId};
        let opts = pending_opts("cat", None, std::time::Duration::from_secs(60));
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        let session = AgentSession {
            kind: AgentKind::Claude,
            id: SessionId::parse("0c2cbc96-1111-4222-8333-444455556666").unwrap(),
            cwd: "/w".into(),
            transcript: None,
        };
        let seen = Seen::Agent {
            kind: AgentKind::Claude,
            pid: 7,
            session: Some(session),
            argv_session: None,
        };
        term.step_agent(seen, std::time::Instant::now());
        assert_eq!(term.agent_saved().map(|a| a.crashed), Some(false));
        assert!(!term.apply_pending_status(), "nothing reported yet");
        let status = Status { code: 137, at: std::time::Instant::now() };
        term.state.lock().unwrap().first_status = Some(status);
        assert!(term.apply_pending_status(), "a status came in");
        assert_eq!(term.agent_saved().map(|a| a.crashed), Some(true));
        assert!(term.state.lock().unwrap().first_status.is_none(), "the status is used once");
    }

    #[test]
    fn real_pty_only_stdusks_own_mark_makes_a_status_and_the_first_one_is_kept() {
        // Two own marks in a burst (5, then 6), a foreign one (9, no `stdusk` word) between and
        // after them. The pane keeps the first own status. The foreign mark makes none.
        let script = "printf '\\033]133;D;9\\007\\033]133;D;5;stdusk\\007\\033]133;D;9\\007\\033]133;D;6;stdusk\\007READY'; cat";
        let term = PtyTerm::spawn(
            60,
            5,
            egui::Context::default(),
            &pending_opts(script, None, std::time::Duration::from_secs(60)),
        );
        poll_term(&term, |t| grid_text(t).contains("READY").then_some(())).unwrap();
        assert_eq!(term.take_status().map(|s| s.code), Some(5));
        assert_eq!(term.take_status(), None, "one status per take");
        // The tab dot still follows the foreign mark: it is the last `D` the reader saw (6).
        assert_eq!(term.cmd_state(), CmdState::Fail);
    }

    /// A user rc like iTerm2's integration: a `133;D;$?` mark at every prompt, the first included.
    /// (The real script does `printf "\033]133;D;%s\007" "$STATUS"` in its precmd.)
    const FOREIGN_ZSH_D: &str = "iterm2_precmd() { printf '\\033]133;D;%s\\007' \"$?\"; }\n\
        precmd_functions=($precmd_functions iterm2_precmd)\n";
    const FOREIGN_BASH_D: &str = "PROMPT_COMMAND='printf \"\\033]133;D;%s\\007\" \"$?\"'\n";

    #[test]
    fn real_pty_zsh_ignores_a_foreign_prompt_mark_and_keeps_its_own_status() {
        use crate::agents::{AgentKind, AgentSession, SessionId};
        let fixture = crate::test_support::ShellFixture::new("zshforeign");
        std::fs::write(fixture.home.join(".zshrc"), FOREIGN_ZSH_D).unwrap();
        let mut opts = fixture.zsh_opts();
        opts.agent_tracking = Some(crate::agent_pane::AgentSetup::default());
        let mut term = PtyTerm::spawn(100, 5, egui::Context::default(), &opts);
        let id = SessionId::parse("0c2cbc96-1111-4222-8333-444455556666").unwrap();
        let session =
            AgentSession { kind: AgentKind::Codex, id, cwd: "/w".into(), transcript: None };
        term.restore_agent(session, false);
        // The first prompt carries the foreign mark. Give it time, then take what was reported.
        poll_term(&term, |t| (t.cmd_state() != CmdState::Idle).then_some(()))
            .expect("the foreign mark at the first prompt never reached the tab dot");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(term.take_status(), None, "a foreign mark is no status");
        term.step_agent(crate::agents::Seen::NoAgent, std::time::Instant::now());
        assert!(term.agent_saved().is_some(), "the restored record survives the first prompt");
        // An empty Enter makes another foreign mark, and still nothing.
        term.send(b"\r");
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(term.take_status(), None);
        // A command makes our own mark, and only then is a status reported.
        term.send(b"true\r");
        wait_for_status(&term);
        term.step_agent(crate::agents::Seen::NoAgent, std::time::Instant::now());
        assert!(term.agent_saved().is_none(), "the first command ended the waiting record");
    }

    #[test]
    fn real_pty_bash_ignores_a_foreign_prompt_mark_and_keeps_its_own_status() {
        // A user PROMPT_COMMAND that marks every prompt (the first, and each empty Enter) as
        // iTerm2's bash integration does. Only a command that ran makes our own status.
        let got = bash_with_rc_statuses(FOREIGN_BASH_D, &["", "true", "", "false"]);
        assert_eq!(got, vec![0, 1]);
    }

    #[test]
    fn a_pane_notice_ends_on_the_users_input_or_click_and_never_on_stdusks_own_typing() {
        use crate::agent_restore::PaneNotice;
        use crate::agents::AgentKind;
        let notice = PaneNotice::CrashHint { kind: AgentKind::Claude };
        // A builtin `read` keeps the shell itself in the foreground, so the fallback may type.
        let new_term = || {
            let script = "printf ARMED; while :; do read line; done";
            let opts = pending_opts(script, Some(b"cmd"), std::time::Duration::ZERO);
            let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
            term.set_notice(notice);
            poll_term(&term, |t| grid_text(t).contains("ARMED").then_some(())).unwrap();
            term
        };
        assert_eq!(new_term().notice(), Some(notice));
        let mut term = new_term();
        term.send(b"x");
        assert_eq!(term.notice(), None, "a key ends it");
        let mut term = new_term();
        term.dismiss_notice();
        assert_eq!(term.notice(), None, "a click ends it");
        // The fallback types the resume command for the user. That is no reason to end the notice.
        let mut term = new_term();
        term.release_pending_after_wait();
        poll_term(&term, |t| grid_text(t).contains("cmd").then_some(()))
            .expect("the fallback never typed the input");
        assert_eq!(term.notice(), Some(notice));
    }

    #[test]
    fn a_pane_notice_draws_each_frame_until_a_key_reaches_the_pane() {
        // The frames run as the workspace runs them: read the keys, send them to the pane, and
        // draw the pane's notice over the grid.
        use crate::agent_restore::PaneNotice;
        use crate::agents::AgentKind;
        let opts = pending_opts("cat", None, std::time::Duration::from_secs(60));
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        term.set_notice(PaneNotice::CrashHint { kind: AgentKind::Codex });
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 400.0));
        let frame = |term: &mut PtyTerm, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(screen),
                events,
                focused: true,
                ..Default::default()
            };
            let (mut drawn, mut keys) = (false, Vec::new());
            let _ = ctx.run_ui(raw, |ui| {
                keys = crate::ui::collect_input(ui, false, false, false);
                if let Some(notice) = term.notice() {
                    let text = crate::ui::pane_notice_text(notice);
                    let pill = crate::ui::draw_pane_notice(ui, screen, &text);
                    drawn = pill.width() > 0.0 && screen.contains_rect(pill);
                }
            });
            if !keys.is_empty() {
                term.send(&keys);
            }
            (drawn, keys)
        };
        assert_eq!(frame(&mut term, vec![]), (true, vec![]), "drawn on the first frame");
        assert_eq!(frame(&mut term, vec![]), (true, vec![]), "and on every frame after");
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let (drawn, keys) = frame(&mut term, vec![enter]);
        assert_eq!(keys, b"\r", "the key still reaches the pty");
        assert!(drawn, "the frame that sent the key drew before the pane changed");
        assert!(!frame(&mut term, vec![]).0, "gone from the next frame on");
    }

    #[test]
    fn a_theme_report_is_stdusks_own_typing_and_keeps_the_notice_and_the_queued_command() {
        // An app that enabled DEC mode 2031 gets a report when the theme flips. That is no input
        // from the user, so it must not end the restore notice or drop the queued resume command.
        use crate::agent_restore::PaneNotice;
        use crate::agents::AgentKind;
        let notice = PaneNotice::CrashHint { kind: AgentKind::Claude };
        let opts = pending_opts("cat", Some(b"queued"), std::time::Duration::from_secs(60));
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        term.set_notice(notice);
        term.state.lock().unwrap().theme_reports = true;
        term.report_theme(true);
        assert_eq!(term.notice(), Some(notice));
        assert_eq!(
            term.agent.as_ref().unwrap().pending().take().as_deref(),
            Some(&b"queued"[..]),
            "the queued command is still there"
        );
        // A key from the user still does both.
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        term.set_notice(notice);
        term.send(b"x");
        assert_eq!(term.notice(), None);
        assert!(term.agent.as_ref().unwrap().pending().take().is_none());
    }

    #[test]
    fn real_pty_a_dropped_fallback_replaces_the_crash_hint_with_a_shell_busy_notice() {
        // The notice promised a typed command. The fallback found a command on the tty and dropped
        // the input, so the pane must say that and not keep the promise.
        use crate::agent_restore::{PaneNotice, SkipReason};
        use crate::agents::{AgentKind, AgentSession, SessionId};
        // One long `sleep` holds the tty. A short loop of sleeps leaves the shell in the foreground
        // between two of them, and the fallback then sees a shell at its prompt.
        let opts = pending_opts(
            "while :; do sleep 30; done",
            Some(b"never-typed"),
            std::time::Duration::ZERO,
        );
        let mut term = PtyTerm::spawn(60, 5, egui::Context::default(), &opts);
        let id = SessionId::parse("0c2cbc96-1111-4222-8333-444455556666").unwrap();
        let session =
            AgentSession { kind: AgentKind::Codex, id, cwd: "/w".into(), transcript: None };
        term.restore_agent(session, true);
        term.set_notice(PaneNotice::CrashHint { kind: AgentKind::Codex });
        wait_until_a_command_owns_the_tty(&term);
        term.release_pending_after_wait();
        assert_eq!(
            term.notice(),
            Some(PaneNotice::NotResumed { kind: AgentKind::Codex, reason: SkipReason::ShellBusy })
        );
        assert!(!grid_text(&term).contains("never-typed"));
    }

    /// Whether `shell` can run here: a path that exists, or a name found on `PATH`.
    fn shell_path(name: &str) -> Option<String> {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .chain(["/bin".into(), "/usr/bin".into(), "/opt/homebrew/bin".into()])
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn real_pty_a_claude_is_captured_under_any_shell_because_nothing_is_injected() {
        // No wrapper, no hook, no environment: the scan finds the agent under the pane's shell and
        // the registry names the session. So fish and plain sh work like zsh and bash.
        use crate::agents::Seen;
        const ID: &str = "0c2cbc96-1111-4222-8333-444455556666";
        for name in ["sh", "bash", "zsh", "fish"] {
            let Some(shell) = shell_path(name) else {
                eprintln!("skipping {name}: not installed");
                continue;
            };
            let fixture = crate::test_support::ShellFixture::new(&format!("anyshell-{name}"));
            let (bin, claude_home) = (fixture.base.join("bin"), fixture.base.join("claude"));
            for d in [&bin, &claude_home.join("sessions")] {
                std::fs::create_dir_all(d).unwrap();
            }
            std::os::unix::fs::symlink("/bin/sleep", bin.join("claude")).unwrap();
            // `; true` keeps the shell from exec-ing into the agent.
            let mut opts = fixture.opts(&shell, &["-c", "claude 30; true"]);
            opts.profile
                .as_mut()
                .unwrap()
                .env
                .insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
            opts.agent_tracking = Some(crate::agent_pane::AgentSetup::default());
            let mut term = PtyTerm::spawn(80, 24, egui::Context::default(), &opts);
            let root = term.shell_pid().expect("the shell has a pid");
            let mut sys = sysinfo::System::new();
            let found = (0..200).find_map(|_| {
                let scan = crate::procwatch::scan_now(&mut sys, &claude_home, None);
                let agent = crate::procwatch::nearest_agent(&scan.procs, root, None)
                    .map(|(_, p)| (p.pid, p.start_time));
                if agent.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                agent
            });
            let (pid, start) = found.unwrap_or_else(|| panic!("{name}: no agent under the shell"));
            // What Claude writes for a live interactive session.
            let registry = format!(
                r#"{{"pid":{pid},"sessionId":"{ID}","cwd":"{}","startedAt":{},"kind":"interactive"}}"#,
                fixture.home.display(),
                start * 1000 + 700
            );
            std::fs::write(claude_home.join(format!("sessions/{pid}.json")), registry).unwrap();
            let scan = crate::procwatch::scan_now(&mut sys, &claude_home, None);
            let seen = crate::procwatch::pane_seen(&scan, root, None);
            term.kill();
            let Seen::Agent { pid: seen_pid, session: Some(session), .. } = seen else {
                panic!("{name}: the registry named no session");
            };
            assert_eq!((seen_pid, session.id.as_str()), (pid, ID), "{name}");
        }
    }
}
