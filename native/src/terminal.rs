//! Real terminal. portable-pty spawns the shell; a reader thread feeds bytes through the vte
//! ANSI parser into a shared alacritty_terminal `Term`. The egui thread reads the grid to
//! render, resizes, scrolls, and writes keystrokes/paste back to the pty.
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use base64::Engine;
use eframe::egui::Color32;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::colors;
use crate::osc::{OscEvent, OscScanner};
use crate::progress::{Progress, ProgressScanner};

/// One styled cell for the renderer. `bg == None` means the terminal default (transparent).
pub struct CellSnap {
    pub c: char,
    pub fg: Color32,
    pub bg: Option<Color32>,
}

/// A frame's worth of visible grid, ready to paint.
pub struct GridSnap {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<CellSnap>,   // row-major, rows*cols
    pub cursor: Option<(usize, usize)>, // (row, col); None while scrolled into history
}

/// Per-tab observable state, written by the reader thread, read by the UI.
#[derive(Default)]
pub struct TabState {
    pub progress: Progress,
    pub cwd: Option<String>,
    pub clipboard: Option<String>, // OSC 52 copy request, consumed by the UI thread
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

pub struct PtyTerm {
    term: Arc<FairMutex<Term<EventProxy>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>, // kept for resize
    state: Arc<Mutex<TabState>>,
    cols: usize,
    rows: usize,
}

impl PtyTerm {
    pub fn spawn(
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
                        for ev in osc_events {
                            match ev {
                                OscEvent::Progress(p) => progress = p, // OSC 9;4 wins over %-scrape
                                OscEvent::Cwd(c) => cwd_update = Some(c),
                                OscEvent::Clipboard(b64) => {
                                    if let Ok(bytes) =
                                        base64::engine::general_purpose::STANDARD.decode(b64)
                                    {
                                        if let Ok(s) = String::from_utf8(bytes) {
                                            clip_update = Some(s);
                                        }
                                    }
                                }
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
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });

        Self { term, writer, master: pair.master, state, cols, rows }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Paste text, wrapped in bracketed-paste markers when the app enabled that mode.
    pub fn paste(&mut self, text: &str) {
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
    pub fn resize(&mut self, cols: usize, rows: usize) {
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

    pub fn scroll(&self, delta_lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(delta_lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// (display_offset, history_size) - for drawing the scrollbar.
    pub fn scroll_state(&self) -> (usize, usize) {
        let t = self.term.lock();
        let g = t.grid();
        (g.display_offset(), g.history_size())
    }

    /// Jump the viewport to an absolute history offset (0 = bottom/live).
    pub fn scroll_to_offset(&self, target: usize) {
        let cur = self.term.lock().grid().display_offset();
        let delta = target as i32 - cur as i32;
        if delta != 0 {
            self.scroll(delta);
        }
    }

    pub fn progress(&self) -> Progress {
        self.state.lock().unwrap().progress
    }

    pub fn cwd(&self) -> Option<String> {
        self.state.lock().unwrap().cwd.clone()
    }

    /// Take a pending OSC 52 clipboard payload (set by the shell), if any.
    pub fn take_clipboard(&self) -> Option<String> {
        self.state.lock().unwrap().clipboard.take()
    }

    /// Snapshot the visible viewport (honoring scrollback offset) with colors + cursor.
    pub fn grid_snapshot(&self) -> GridSnap {
        let term = self.term.lock();
        let grid = term.grid();
        let mut cells = Vec::with_capacity(self.rows * self.cols);
        // display_iter walks the visible region row-major, accounting for scroll offset.
        for indexed in grid.display_iter() {
            let cell = indexed.cell;
            let inverse = cell.flags.contains(Flags::INVERSE);
            let (fg_c, bg_c) = if inverse {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            let bg = if !inverse && colors::is_default_bg(&cell.bg) {
                None
            } else {
                Some(colors::to_color32(bg_c))
            };
            cells.push(CellSnap {
                c: cell.c,
                fg: colors::to_color32(fg_c),
                bg,
            });
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
        GridSnap {
            cols: self.cols,
            rows: self.rows,
            cells,
            cursor,
        }
    }
}
