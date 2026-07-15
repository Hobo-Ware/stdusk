//! stdusk - a quake terminal with a real GUI tab bar.
//! M0: chrome. M1: live shell per tab.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

mod colors;
mod osc;
mod progress;
mod terminal;
use progress::Progress;
use terminal::{GridSnap, PtyTerm};

const COLS: usize = 80;
const ROWS: usize = 24;
const FONT_SIZE: f32 = 13.0;

mod palette {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x28, 0x2c, 0x34);
    pub const PANEL: Color32 = Color32::from_rgb(0x21, 0x25, 0x2b);
    pub const FG: Color32 = Color32::from_rgb(0xdc, 0xdf, 0xe4);
    pub const DIM: Color32 = Color32::from_rgb(0x5c, 0x63, 0x70);
    pub const ELEVATED: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3a); // active-tab bg
    pub const ACCENT: Color32 = Color32::from_rgb(0x61, 0xaf, 0xef);
    pub const GREEN: Color32 = Color32::from_rgb(0x98, 0xc3, 0x79);
    pub const YELLOW: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x7b);
    pub const RED: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
    pub const PURPLE: Color32 = Color32::from_rgb(0xc6, 0x78, 0xdd);
    pub const CYAN: Color32 = Color32::from_rgb(0x56, 0xb6, 0xc2);

    /// Palette offered by the M5 right-click Color menu (tabs are colorless by default).
    #[allow(dead_code)] // consumed by the M5 color picker
    pub const TAB_COLORS: [Color32; 6] = [RED, ACCENT, YELLOW, PURPLE, GREEN, CYAN];
}

struct Tab {
    title: String,
    color: Option<egui::Color32>, // None = no underline (Tabby default); set via M5 menu
    term: PtyTerm,
}

fn spawn_tab(ctx: &egui::Context) -> Tab {
    Tab {
        title: "zsh".into(),
        color: None,
        term: PtyTerm::spawn(COLS, ROWS, ctx.clone()),
    }
}

struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
    _hotkey: GlobalHotKeyManager, // kept alive so the registration persists
    toggle: Arc<AtomicBool>,      // set by the hotkey thread, consumed in ui()
    visible: bool,
    was_focused: bool, // gained focus since last show (so blur can hide)
    sized: bool,       // applied quake sizing once the monitor size was known
}

impl Stdusk {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);

        // Global quake hotkey (default Ctrl+`; config-driven in M4). Carbon API on
        // macOS - no Accessibility grant needed.
        let mgr = GlobalHotKeyManager::new().expect("hotkey manager");
        let _ = mgr.register(HotKey::new(Some(Modifiers::CONTROL), Code::Backquote));

        // A thread wakes the UI (even while hidden) when the hotkey fires.
        let toggle = Arc::new(AtomicBool::new(false));
        let toggle_thread = toggle.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if ev.state == HotKeyState::Pressed {
                    toggle_thread.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                }
            }
        });

        Self {
            tabs: vec![spawn_tab(&cc.egui_ctx)],
            active: 0,
            _hotkey: mgr,
            toggle,
            visible: true,
            was_focused: false, // arm hide-on-blur only after the first focus gain
            sized: false,
        }
    }
}

/// Show (drop to the top edge, focused) or "hide" the quake window.
///
/// We do NOT use `Visible(false)` or move fully off-screen: on macOS that lets the OS
/// occlude the window and App-Nap the process, which throttles the run loop so the global
/// hotkey handler never fires again (it can't be brought back). Instead we park the window
/// mostly below the screen, leaving a ~2px sliver on-screen so it stays un-occluded and the
/// run loop keeps delivering the hotkey. Combined with a repaint tick while hidden. A proper
/// native hide (NSPanel orderOut) is a polish item.
fn apply_visibility(ctx: &egui::Context, visible: bool) {
    let mon = ctx.input(|i| i.viewport().monitor_size);
    let height = mon.map(|m| (m.y * 0.5).round()).unwrap_or(500.0);
    if visible {
        if let Some(m) = mon {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(m.x, height)));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    } else {
        // Park just off the bottom edge, leaving a 2px sliver on-screen.
        let y = mon.map(|m| m.y - 2.0).unwrap_or(2000.0);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, y)));
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = palette::BG;
    v.window_fill = palette::BG;
    v.override_text_color = Some(palette::FG);
    ctx.set_visuals(v);
}

/// Flat Tabby-style tab: dark bg (elevated when active), a thin per-tab colored underline,
/// and progress rendered as a thin bar on the TOP edge.
fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
    title: &str,
    active: bool,
    color: Option<egui::Color32>,
    progress: Progress,
) -> egui::Response {
    let fg = if active { palette::FG } else { palette::DIM };
    let mut rt = egui::RichText::new(format!("{idx}  {title}")).color(fg).monospace();
    if active {
        rt = rt.strong();
    }
    let fill = if active {
        palette::ELEVATED
    } else {
        egui::Color32::TRANSPARENT
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.add(egui::Label::new(rt).selectable(false));
        });
    let rect = inner.response.rect;
    let p = ui.painter();
    // Per-tab color underline (bottom edge) - only when the user has set a color (M5 menu).
    if let Some(color) = color {
        p.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() - 2.0),
                egui::vec2(rect.width(), 2.0),
            ),
            0.0,
            color,
        );
    }
    // Progress bar (top edge), width = fraction, colored by state.
    if let Some((frac, pcolor)) = progress_bar(progress) {
        p.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top()),
                egui::vec2(rect.width() * frac, 2.0),
            ),
            0.0,
            pcolor,
        );
    }
    inner.response.interact(egui::Sense::click())
}

/// (fill fraction 0..1, color) for the tab progress bar, or None to hide it.
fn progress_bar(p: Progress) -> Option<(f32, egui::Color32)> {
    match p {
        Progress::None => None,
        Progress::Normal(v) => Some((v as f32 / 100.0, palette::GREEN)),
        Progress::Paused(v) => Some((v as f32 / 100.0, palette::YELLOW)),
        Progress::Error(_) => Some((1.0, palette::RED)),
        Progress::Indeterminate => Some((1.0, palette::ACCENT)),
    }
}

/// Translate this frame's key/text events into bytes for the pty.
fn collect_input(ui: &egui::Ui) -> Vec<u8> {
    let mut out = Vec::new();
    ui.input(|i| {
        for event in &i.events {
            match event {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    use egui::Key;
                    if modifiers.ctrl {
                        // Ctrl+A..Z -> control bytes 0x01..0x1a
                        if let Some(off) = ctrl_letter(*key) {
                            out.push(off);
                        }
                        return;
                    }
                    match key {
                        Key::Enter => out.push(b'\r'),
                        Key::Backspace => out.push(0x7f),
                        Key::Tab => out.push(b'\t'),
                        Key::Escape => out.push(0x1b),
                        Key::ArrowUp => out.extend_from_slice(b"\x1b[A"),
                        Key::ArrowDown => out.extend_from_slice(b"\x1b[B"),
                        Key::ArrowRight => out.extend_from_slice(b"\x1b[C"),
                        Key::ArrowLeft => out.extend_from_slice(b"\x1b[D"),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    });
    out
}

fn ctrl_letter(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    let n = match key {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8, I => 9,
        J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16, Q => 17,
        R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24, Y => 25, Z => 26,
        _ => return None,
    };
    Some(n)
}

impl eframe::App for Stdusk {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.157, 0.173, 0.204, 0.85] // translucent OneHalfDark bg
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // First run: apply full quake sizing once the monitor size is known (it's not
        // populated on frame 0, so retry until it is).
        if !self.sized {
            if ctx.input(|i| i.viewport().monitor_size).is_some() {
                apply_visibility(&ctx, true);
                self.sized = true;
            } else {
                ctx.request_repaint();
            }
        }

        // Quake toggle (from the global-hotkey thread).
        if self.toggle.swap(false, Ordering::SeqCst) {
            self.visible = !self.visible;
            apply_visibility(&ctx, self.visible);
            if self.visible {
                self.was_focused = false; // wait to regain focus before blur can hide us
            }
        }
        // Hide when focus is lost (after we've actually gained it since showing).
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if self.visible {
            if focused {
                self.was_focused = true;
            } else if self.was_focused {
                self.visible = false;
                apply_visibility(&ctx, false);
            }
        } else {
            // Keep the loop alive while hidden so the hotkey toggle is polled.
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        egui::Panel::top("tabbar")
            .frame(
                egui::Frame::new()
                    .fill(palette::PANEL)
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut clicked: Option<usize> = None;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let active = i == self.active;
                        if draw_tab(ui, i + 1, &tab.title, active, tab.color, tab.term.progress())
                            .clicked()
                        {
                            clicked = Some(i);
                        }
                    }
                    if let Some(i) = clicked {
                        self.active = i;
                    }
                    if ui.button("  +  ").clicked() {
                        let ctx = ui.ctx().clone();
                        self.tabs.push(spawn_tab(&ctx));
                        self.active = self.tabs.len() - 1;
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette::BG)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let input = collect_input(ui);
                if !input.is_empty() {
                    self.tabs[self.active].term.send(&input);
                }
                render_grid(ui, &self.tabs[self.active].term.grid_snapshot());
            });
    }
}

/// Paint the terminal grid: per-cell bg rects + fg glyphs + a beam cursor.
fn render_grid(ui: &mut egui::Ui, snap: &GridSnap) {
    let font = egui::FontId::monospace(FONT_SIZE);
    // Cell metrics from a monospace glyph (advance width + line height).
    let m = ui
        .painter()
        .layout_no_wrap("M".to_owned(), font.clone(), colors::FG);
    let cw = m.size().x;
    let ch = m.size().y;
    let size = egui::vec2(cw * snap.cols as f32, ch * snap.rows as f32);
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let origin = resp.rect.min;

    for r in 0..snap.rows {
        for c in 0..snap.cols {
            let cell = &snap.cells[r * snap.cols + c];
            let pos = origin + egui::vec2(c as f32 * cw, r as f32 * ch);
            if let Some(bg) = cell.bg {
                painter.rect_filled(egui::Rect::from_min_size(pos, egui::vec2(cw, ch)), 0.0, bg);
            }
            if cell.c != ' ' && cell.c != '\0' {
                painter.text(pos, egui::Align2::LEFT_TOP, cell.c, font.clone(), cell.fg);
            }
        }
    }

    // Beam cursor (block/underline styles land in M9).
    let (cr, cc) = snap.cursor;
    let cpos = origin + egui::vec2(cc as f32 * cw, cr as f32 * ch);
    painter.rect_filled(
        egui::Rect::from_min_size(cpos, egui::vec2(2.0, ch)),
        0.0,
        colors::FG,
    );
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_transparent(true)
            .with_inner_size([1200.0, 500.0])
            .with_position([0.0, 0.0]),
        ..Default::default()
    };
    eframe::run_native(
        "stdusk",
        options,
        Box::new(|cc| Ok(Box::new(Stdusk::new(cc)))),
    )
}
