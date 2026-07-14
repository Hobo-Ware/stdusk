//! stdusk - a quake terminal with a real GUI tab bar.
//! M0: chrome. M1: live shell per tab.
use eframe::egui;

mod osc;
mod progress;
mod terminal;
use progress::Progress;
use terminal::PtyTerm;

const COLS: usize = 80;
const ROWS: usize = 24;

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

    /// Default per-tab underline colors, cycled by tab index (Tabby-style rainbow).
    pub const TAB_COLORS: [Color32; 6] = [RED, ACCENT, YELLOW, PURPLE, GREEN, CYAN];
    pub fn tab_color(i: usize) -> Color32 {
        TAB_COLORS[i % TAB_COLORS.len()]
    }
}

struct Tab {
    title: String,
    color: egui::Color32,
    term: PtyTerm,
}

fn spawn_tab(ctx: &egui::Context, color: egui::Color32) -> Tab {
    Tab {
        title: "zsh".into(),
        color,
        term: PtyTerm::spawn(COLS, ROWS, ctx.clone()),
    }
}

struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
}

impl Stdusk {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        Self {
            tabs: vec![spawn_tab(&cc.egui_ctx, palette::tab_color(0))],
            active: 0,
        }
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
    color: egui::Color32,
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
    // Per-tab color underline (bottom edge) - the color-coding affordance.
    p.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            egui::vec2(rect.width(), 2.0),
        ),
        0.0,
        color,
    );
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
                        let color = palette::tab_color(self.tabs.len());
                        self.tabs.push(spawn_tab(&ctx, color));
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
                let text = self.tabs[self.active].term.snapshot().join("\n");
                ui.add(
                    egui::Label::new(egui::RichText::new(text).monospace().color(palette::FG))
                        .selectable(false),
                );
            });
    }
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
