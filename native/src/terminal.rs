//! M1: real terminal. portable-pty spawns the shell; a reader thread feeds bytes
//! through the vte ANSI parser into a shared alacritty_terminal `Term`. The egui
//! thread reads the grid to render and writes keystrokes back to the pty.
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::osc::{OscEvent, OscScanner};
use crate::progress::{Progress, ProgressScanner};

/// Per-tab observable state, written by the reader thread, read by the UI.
#[derive(Default)]
pub struct TabState {
    pub progress: Progress,
    pub cwd: Option<String>,
}

/// Minimal grid sizing (no scrollback for M1).
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
    state: Arc<Mutex<TabState>>,
    cols: usize,
    rows: usize,
}

impl PtyTerm {
    pub fn spawn(cols: usize, rows: usize, ctx: egui::Context) -> Self {
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
        let cmd = CommandBuilder::new(shell);
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
            let mut prog = ProgressScanner::new(true);
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
                        for ev in osc_events {
                            match ev {
                                OscEvent::Progress(p) => progress = p, // OSC 9;4 wins over %-scrape
                                OscEvent::Cwd(c) => cwd_update = Some(c),
                                OscEvent::Clipboard(_) => {} // wired in M6
                            }
                        }
                        {
                            let mut s = state_reader.lock().unwrap();
                            s.progress = progress;
                            if let Some(c) = cwd_update {
                                s.cwd = Some(c);
                            }
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });

        Self { term, writer, state, cols, rows }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn progress(&self) -> Progress {
        self.state.lock().unwrap().progress
    }

    pub fn cwd(&self) -> Option<String> {
        self.state.lock().unwrap().cwd.clone()
    }

    /// Snapshot the visible grid as plain lines (colors come in M1.5).
    pub fn snapshot(&self) -> Vec<String> {
        let term = self.term.lock();
        let grid = term.grid();
        let mut lines = Vec::with_capacity(self.rows);
        for line in 0..self.rows {
            let mut s = String::with_capacity(self.cols);
            for col in 0..self.cols {
                s.push(grid[Line(line as i32)][Column(col)].c);
            }
            lines.push(s.trim_end().to_string());
        }
        lines
    }
}
