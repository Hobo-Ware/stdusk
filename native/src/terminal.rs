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
use alacritty_terminal::vte::ansi::Processor;
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
pub(crate) struct CellSnap {
    pub(crate) c: char,
    pub(crate) fg: Color32,
    pub(crate) bg: Option<Color32>,
    pub(crate) selected: bool,
    pub(crate) wide: bool,
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

/// Event sink for the alacritty `Term`. We drive repaints from the reader thread, so the only
/// event we care about is the bell -> flag it on the shared state for the UI to flash.
#[derive(Clone)]
struct EventProxy {
    state: Arc<Mutex<TabState>>,
}
impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Bell)
            && let Ok(mut s) = self.state.lock()
        {
            s.bell = true;
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
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>, // kept for resize
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
        let writer = pair.master.take_writer().expect("writer");

        let state = Arc::new(Mutex::new(TabState::default()));
        let term_config = Config {
            scrolling_history: opts.scrollback_lines,
            semantic_escape_chars: opts.word_separators.clone(),
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &Dims { cols, rows },
            EventProxy { state: state.clone() },
        )));

        let term_reader = term.clone();
        let state_reader = state.clone();
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
                        // Advance the terminal, then read whether we're now in the alt-screen.
                        let alt = {
                            let mut term = term_reader.lock();
                            parser.advance(&mut *term, chunk);
                            term.mode().contains(TermMode::ALT_SCREEN)
                        };
                        let text = String::from_utf8_lossy(chunk);
                        let mut progress = prog.feed(&text, alt);
                        let mut cwd_update = None;
                        let mut clip_update = None;
                        let mut cmd_update = None;
                        let mut title_update = None; // Some(title); empty resets
                        let mut notify = None; // Some(exit) when a long command just finished
                        for ev in osc_events {
                            match ev {
                                OscEvent::Progress(p) => progress = p, // OSC 9;4 wins over %-scrape
                                OscEvent::Title(t) => title_update = Some(t),
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
                            if let Some(t) = title_update {
                                s.title_osc = (!t.is_empty()).then_some(t);
                            }
                            if let Some(code) = notify {
                                s.done_notify = Some(code);
                            }
                        }
                        ctx.request_repaint();
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
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
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
    /// which would undo the wipe.
    pub(crate) fn clear_all(&self) {
        let mut t = self.term.lock();
        let g = t.grid_mut();
        g.reset_region(..);
        g.clear_history();
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
            let bold = self.bold_bright && cell.flags.contains(Flags::BOLD);
            let (c, wide) = snap_glyph(cell.c, cell.flags);
            cells.push(CellSnap { c, fg: colors::cell_fg(fg_c, bold), bg, selected, wide });
        }
        // Cursor only shown when the viewport is at the bottom (not scrolled into history).
        let cursor = if grid.display_offset() == 0 {
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
        CmdState, ExitAction, Flags, OnExit, PtyTerm, SpawnOpts, cmd_from_exit, exit_action,
        on_exit_mode, resolve_cwd, resolve_shell, snap_glyph,
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
            env: std::collections::HashMap::new(),
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
                env: std::collections::HashMap::new(),
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
        term.clear_all();
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
    fn real_pty_output_sets_activity() {
        // Any output flags activity; take_activity consumes it (one-shot until more output).
        let term = e2e_term("printf 'hello'; sleep 5");
        poll_term(&term, |t| t.take_activity().then_some(())).expect("activity never flagged");
        assert!(!term.take_activity(), "take_activity must consume the flag");
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
}
