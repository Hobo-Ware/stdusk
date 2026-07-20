//! Real terminal. portable-pty spawns the shell; a reader thread feeds bytes through the vte
//! ANSI parser into a shared alacritty_terminal `Term`. The egui thread reads the grid to
//! render, resizes, scrolls, and writes keystrokes/paste back to the pty.
use std::io::{Read, Write};
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

use crate::colors;
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
/// registered; independent of the `bold_bright` color treatment.
pub(crate) struct CellSnap {
    pub(crate) c: char,
    pub(crate) fg: Color32,
    pub(crate) bg: Option<Color32>,
    pub(crate) selected: bool,
    pub(crate) wide: bool,
    pub(crate) bold: bool,
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

/// The mouse-reporting an app switched on, snapshotted from the alacritty `Term` modes. `stdusk`
/// sends NO reports unless the app asked (DECSET 1000/1002/1003 + SGR 1006); before this, TUIs
/// that enabled tracking (Claude Code's fullscreen UI, `git` list pickers) never received
/// wheel/click events and so couldn't scroll or repaint efficiently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent DECSET flags, not a state machine
pub(crate) struct MouseReporting {
    pub(crate) report_click: bool,     // ?1000 - button press + release
    pub(crate) drag: bool, // ?1002 - button-event tracking (motion while a button is down)
    pub(crate) motion: bool, // ?1003 - any-motion tracking (every move)
    pub(crate) sgr: bool,  // ?1006 - SGR extended coordinates
    pub(crate) alternate_scroll: bool, // ?1007 - wheel emits arrow keys on the alt screen
}

impl MouseReporting {
    /// A button/motion tracking mode is on, so pointer events belong to the app (not to local
    /// scroll/selection). Encoding still gates on `sgr` - we only speak SGR 1006.
    pub(crate) fn reports_buttons(self) -> bool {
        self.report_click || self.drag || self.motion
    }
}

/// SGR mouse button code for a wheel tick: 64 = up, 65 = down; `None` for a zero delta.
pub(crate) fn wheel_button(delta_lines: i32) -> Option<u8> {
    match delta_lines.signum() {
        1 => Some(64),
        -1 => Some(65),
        _ => None,
    }
}

/// Encode one pointer event in SGR 1006 form: `ESC [ < button ; col ; row (M|m)`. `col`/`row`
/// are 0-based grid cells; the wire format is 1-based, so they're bumped here. `pressed = false`
/// emits the release terminator `m`; presses, wheel ticks and motion use `M`.
pub(crate) fn sgr_mouse(button: u8, col: usize, row: usize, pressed: bool) -> Vec<u8> {
    let terminator = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{button};{};{}{terminator}", col + 1, row + 1).into_bytes()
}

/// SGR 1006 reports for a wheel scroll of `lines` at cell `(col, row)`: one report per line, as a
/// physical mouse sends one event per notch. Positive = wheel up (64), negative = down (65);
/// empty for a zero delta. Wheel events are always the `M` (pressed) form.
pub(crate) fn wheel_sgr(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let Some(button) = wheel_button(lines) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for _ in 0..lines.unsigned_abs() {
        out.extend_from_slice(&sgr_mouse(button, col, row, true));
    }
    out
}

/// Clamp a frame's wheel delta to a physical-wheel-sized burst of MOUSE-REPORT ticks. `lines` is
/// sized for LOCAL scrollback, where a big accelerated / high-res-trackpad flick is harmless -
/// alacritty caps the scroll to the available history. An app that requested mouse reporting has
/// no such backstop: every wheel report we forward scrolls its TUI another notch, so a frame of
/// tens of lines flings its view (the alt-screen over-acceleration users hit in Claude Code's
/// fullscreen UI). Cap the per-frame report count; a normal one-line notch passes through.
pub(crate) fn wheel_report_lines(lines: i32) -> i32 {
    const MAX_REPORTS: i32 = 3; // one deliberate wheel burst - never a whole accelerated flick
    lines.clamp(-MAX_REPORTS, MAX_REPORTS)
}

/// Lines to auto-scroll the viewport per frame while a selection drag is held past a pane edge,
/// so the selection can extend beyond what was visible when the drag began (standard terminal
/// behavior). Sign matches `PtyTerm::scroll`: + past the TOP edge (reveal older history), - past
/// the BOTTOM. Ramps 1..=MAX with how far past the edge the pointer sits; 0 inside the viewport.
pub(crate) fn drag_autoscroll_lines(pointer_y: f32, top: f32, bottom: f32, cell_h: f32) -> i32 {
    const MAX: i32 = 4;
    let step = |past: f32| (1 + (past / cell_h.max(1.0)) as i32).min(MAX);
    if pointer_y < top {
        step(top - pointer_y)
    } else if pointer_y > bottom {
        -step(pointer_y - bottom)
    } else {
        0
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
#[derive(Default)]
pub(crate) struct TabState {
    pub(crate) progress: Progress,
    pub(crate) cwd: Option<String>,
    pub(crate) clipboard: Option<String>, // OSC 52 copy request, consumed by the UI thread
    pub(crate) cmd: CmdState,             // OSC 133 last-command state (tab dot)
    pub(crate) bell: bool,                // BEL rung since last consumed (drives the visual flash)
    pub(crate) activity: bool,            // output since last consumed (notify-on-activity)
    pub(crate) done_notify: Option<i32>, // a long command just finished (exit code); UI consumes it
    pub(crate) exited: Option<ExitInfo>, // the shell exited (pty EOF + reaped); UI applies on_exit
    pub(crate) title_osc: Option<String>, // OSC 0/2 window title (None = unset / reset)
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
#[derive(Clone)]
pub(crate) struct SpawnOpts {
    pub(crate) detect_progress: bool,
    pub(crate) shell_integration: bool,
    pub(crate) scrollback_lines: usize,
    pub(crate) word_separators: String,
    pub(crate) bold_bright: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) profile: Option<crate::config::Profile>, // launch profile overrides (shell/args/cwd/env)
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
    master: Box<dyn MasterPty + Send>,         // kept for resize
    state: Arc<Mutex<TabState>>,
    cols: usize,
    rows: usize,
    shell_pid: Option<u32>, // for CLI-awareness process scanning (procwatch)
    bold_bright: bool,      // draw bold text in bright ANSI colors
    rapid_exits: u32,       // consecutive <RAPID_EXIT_SECS deaths, carried across respawns
}

impl PtyTerm {
    pub(crate) fn spawn(cols: usize, rows: usize, ctx: egui::Context, opts: &SpawnOpts) -> Self {
        let SpawnOpts { detect_progress, shell_integration, cwd, profile, .. } = opts.clone();
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
        crate::shell::configure(&mut cmd, &shell, shell_integration);
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
        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let shell_pid = child.process_id();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reader");
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

        let term_reader = term.clone();
        let state_reader = state.clone();
        let writer_reader = writer.clone();
        thread::spawn(move || {
            let spawned = std::time::Instant::now(); // for ExitInfo.uptime_secs
            let mut parser: Processor = Processor::new();
            let mut prog = ProgressScanner::new(detect_progress);
            let mut osc = OscScanner::new();
            let mut buf = [0u8; 8192];
            let mut cmd_started: Option<std::time::Instant> = None; // for notify-when-done
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let osc_events = osc.feed(chunk);
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
                                    ShellEvent::CommandEnd(code) => {
                                        cmd_update = Some(cmd_from_exit(code));
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
                        // Defer (don't paint per read): coalesce the burst so a clear+redraw
                        // lands atomically before the UI snapshots. See REPAINT_COALESCE_WINDOW.
                        ctx.request_repaint_after(REPAINT_COALESCE_WINDOW);
                    }
                }
            }
            // EOF/err: the shell's side of the pty closed, so the shell is gone and wait()
            // returns promptly. Reap the real exit code and flag the pane as exited.
            let code = child.wait().map_or(-1, |st| st.exit_code() as i32);
            state_reader.lock().unwrap().exited =
                Some(ExitInfo { code, uptime_secs: spawned.elapsed().as_secs_f32() });
            ctx.request_repaint();
        });

        Self {
            term,
            writer,
            master: pair.master,
            state,
            cols,
            rows,
            shell_pid,
            bold_bright: opts.bold_bright,
            rapid_exits: 0,
        }
    }

    /// PID of the tab's shell process - the root for CLI-awareness descendant scans.
    pub(crate) fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
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
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.term.lock().resize(Dims { cols, rows });
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
    /// pane is respawned or closed - the UI reads it every frame to apply `on_exit`.
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
            let (c, wide) = snap_glyph(cell.c, cell.flags);
            cells.push(CellSnap { c, fg: colors::cell_fg(fg_c, bright), bg, selected, wide, bold });
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

    /// Whether the terminal is on the alternate screen (vim/less/...), e.g. to suppress the
    /// multiline-paste warning like Tabby does.
    pub(crate) fn is_alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
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

#[cfg(test)]
mod tests {
    use super::{
        CmdState, ExitAction, Flags, OnExit, PtyTerm, REPAINT_COALESCE_WINDOW, SpawnOpts,
        cmd_from_exit, drag_autoscroll_lines, exit_action, on_exit_mode, resolve_cwd,
        resolve_shell, sgr_mouse, snap_glyph, wheel_button, wheel_report_lines, wheel_sgr,
    };
    use crate::config::Profile;

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
        let opts = SpawnOpts {
            detect_progress: false,
            shell_integration: false,
            scrollback_lines: 500,
            word_separators: " ".into(),
            bold_bright: false,
            cwd: None,
            profile: Some(Profile {
                name: "e2e".into(),
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
                color: None,
            }),
        };
        PtyTerm::spawn(20, 5, egui::Context::default(), &opts)
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
    fn wheel_button_maps_direction() {
        assert_eq!(wheel_button(1), Some(64)); // wheel up
        assert_eq!(wheel_button(5), Some(64)); // magnitude ignored, sign only
        assert_eq!(wheel_button(-1), Some(65)); // wheel down
        assert_eq!(wheel_button(-3), Some(65));
        assert_eq!(wheel_button(0), None);
    }

    #[test]
    fn sgr_mouse_encodes_1based_cells_and_terminator() {
        // 0-based (col, row) -> 1-based on the wire; press = `M`, release = `m`.
        assert_eq!(sgr_mouse(0, 0, 0, true), b"\x1b[<0;1;1M".to_vec());
        assert_eq!(sgr_mouse(2, 4, 9, false), b"\x1b[<2;5;10m".to_vec()); // right-button release
        assert_eq!(sgr_mouse(64, 2, 4, true), b"\x1b[<64;3;5M".to_vec()); // wheel up
    }

    #[test]
    fn wheel_report_lines_clamps_bursts_but_keeps_a_normal_notch() {
        // A normal notch passes through unchanged; an accelerated / high-res flick clamps to a
        // small burst so a mouse-reporting TUI (alt-screen) doesn't over-scroll. Sign preserved.
        assert_eq!(wheel_report_lines(1), 1); // one-line notch untouched
        assert_eq!(wheel_report_lines(-1), -1);
        assert_eq!(wheel_report_lines(3), 3); // right at the cap
        assert_eq!(wheel_report_lines(40), 3); // big accelerated delta -> small burst
        assert_eq!(wheel_report_lines(-40), -3);
        assert_eq!(wheel_report_lines(0), 0); // no delta -> no report
    }

    #[test]
    fn wheel_sgr_emits_one_report_per_line() {
        assert_eq!(wheel_sgr(1, 2, 4), b"\x1b[<64;3;5M".to_vec());
        assert_eq!(wheel_sgr(-1, 0, 0), b"\x1b[<65;1;1M".to_vec());
        assert_eq!(wheel_sgr(2, 0, 0), b"\x1b[<64;1;1M\x1b[<64;1;1M".to_vec());
        assert!(wheel_sgr(0, 3, 3).is_empty());
    }

    #[test]
    fn drag_autoscroll_ramps_and_signs() {
        // Inside the viewport: no scroll.
        assert_eq!(drag_autoscroll_lines(50.0, 10.0, 100.0, 10.0), 0);
        assert_eq!(drag_autoscroll_lines(10.0, 10.0, 100.0, 10.0), 0); // exactly at the top edge
        // Past the TOP edge -> positive (reveal older history), ramps with distance, capped at 4.
        assert_eq!(drag_autoscroll_lines(5.0, 10.0, 100.0, 10.0), 1); // <1 cell over
        assert_eq!(drag_autoscroll_lines(-100.0, 10.0, 100.0, 10.0), 4); // far over -> cap
        // Past the BOTTOM edge -> negative (reveal newer content).
        assert_eq!(drag_autoscroll_lines(105.0, 10.0, 100.0, 10.0), -1);
        assert_eq!(drag_autoscroll_lines(1000.0, 10.0, 100.0, 10.0), -4);
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
}
