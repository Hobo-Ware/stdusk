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

/// Last-command state from OSC 133 shell integration, shown as the tab's state dot.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CmdState {
    #[default]
    Idle, // no command run yet (no dot)
    Running,
    Ok,
    Fail,
}

/// One styled cell for the renderer. `bg == None` means the terminal default (transparent).
pub(crate) struct CellSnap {
    pub(crate) c: char,
    pub(crate) fg: Color32,
    pub(crate) bg: Option<Color32>,
    pub(crate) selected: bool,
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

/// No-op event sink. We drive repaints from the reader thread directly.
#[derive(Clone)]
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {}
}

pub(crate) struct PtyTerm {
    term: Arc<FairMutex<Term<EventProxy>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>, // kept for resize
    state: Arc<Mutex<TabState>>,
    cols: usize,
    rows: usize,
}

impl PtyTerm {
    pub(crate) fn spawn(
        cols: usize,
        rows: usize,
        ctx: egui::Context,
        detect_progress: bool,
        cwd: Option<String>,
    ) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(shell);
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.cwd(dir);
        }
        let _child = pair.slave.spawn_command(cmd).expect("spawn shell");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &Dims { cols, rows },
            EventProxy,
        )));

        let state = Arc::new(Mutex::new(TabState::default()));

        let term_reader = term.clone();
        let state_reader = state.clone();
        thread::spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut prog = ProgressScanner::new(detect_progress);
            let mut osc = OscScanner::new();
            let mut buf = [0u8; 8192];
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
                        for ev in osc_events {
                            match ev {
                                OscEvent::Progress(p) => progress = p, // OSC 9;4 wins over %-scrape
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
                                    }
                                    ShellEvent::CommandEnd(code) => {
                                        cmd_update = Some(if code.unwrap_or(0) == 0 {
                                            CmdState::Ok
                                        } else {
                                            CmdState::Fail
                                        });
                                    }
                                    // PromptStart: keep the last result visible at the prompt.
                                    ShellEvent::PromptStart => {}
                                },
                            }
                        }
                        {
                            let mut s = state_reader.lock().unwrap();
                            s.progress = progress;
                            if let Some(c) = cwd_update {
                                s.cwd = Some(c);
                            }
                            if let Some(c) = clip_update {
                                s.clipboard = Some(c);
                            }
                            if let Some(c) = cmd_update {
                                s.cmd = c;
                            }
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });

        Self { term, writer, master: pair.master, state, cols, rows }
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
        if cols == self.cols && rows == self.rows || cols == 0 || rows == 0 {
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

    pub(crate) fn cwd(&self) -> Option<String> {
        self.state.lock().unwrap().cwd.clone()
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
        let mut cells = Vec::with_capacity(self.rows * self.cols);
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
            cells.push(CellSnap { c: cell.c, fg: colors::to_color32(fg_c), bg, selected });
        }
        // Cursor only shown when the viewport is at the bottom (not scrolled into history).
        let cursor = if grid.display_offset() == 0 {
            let cp = grid.cursor.point;
            Some((
                (cp.line.0.max(0) as usize).min(self.rows.saturating_sub(1)),
                cp.column.0.min(self.cols.saturating_sub(1)),
            ))
        } else {
            None
        };
        GridSnap { cols: self.cols, rows: self.rows, cells, cursor, top_line }
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

    /// Selected text (for Cmd+C), or None when there's no non-empty selection.
    pub(crate) fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    /// Whole buffer (scrollback + screen) as `(alacritty Line, trailing-trimmed text)` pairs,
    /// top-to-bottom - the input to scrollback search.
    pub(crate) fn buffer_lines(&self) -> Vec<(i32, String)> {
        let term = self.term.lock();
        let grid = term.grid();
        let (top, bot) = (grid.topmost_line().0, grid.bottommost_line().0);
        let mut out = Vec::with_capacity((bot - top + 1).max(0) as usize);
        for l in top..=bot {
            let row = &grid[Line(l)];
            let mut s = String::with_capacity(self.cols);
            for c in 0..self.cols {
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
