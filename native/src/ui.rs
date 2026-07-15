//! View layer: the egui drawing widgets (tab, icon button, grid, toast) plus the pure,
//! unit-testable helpers extracted from the render loop (geometry, text, input mapping).
//! Keeping the math here - free of `Ui`/`Context` - is what makes it testable; `main.rs`'s
//! `eframe::App` loop stays a thin caller.
use eframe::egui;

use crate::colors;
use crate::progress::Progress;
use crate::terminal::{GridSnap, PtyTerm};

/// Phosphor icon codepoints (font vendored in assets/Phosphor.ttf, MIT).
pub(crate) mod icons {
    pub(crate) const PLUS: &str = "\u{E3D4}";
    pub(crate) const X: &str = "\u{E4F6}";
    pub(crate) const GEAR: &str = "\u{E270}";
    pub(crate) const APP_WINDOW: &str = "\u{E5DA}";
}

// ---- pure helpers (no egui state; unit-tested below) ----

/// Last path segment of a cwd, for the auto tab title. Trailing slashes ignored; `/` for root.
pub(crate) fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".into();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// Truncate to `max` chars with an ellipsis; returns (shown, was_truncated).
pub(crate) fn ellipsize(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    (format!("{head}…"), true)
}

/// Map a pointer offset from the grid origin to a grid point: (buffer line, column, right-half).
/// `top_line` is the buffer line of viewport row 0 (negative while scrolled into history).
pub(crate) fn pos_to_cell(
    rel_x: f32,
    rel_y: f32,
    cw: f32,
    ch: f32,
    cols: usize,
    rows: usize,
    top_line: i32,
) -> (i32, usize, bool) {
    let relx = (rel_x / cw).max(0.0);
    let rely = (rel_y / ch).max(0.0);
    let col = (relx.floor() as usize).min(cols.saturating_sub(1));
    let row = (rely.floor() as usize).min(rows.saturating_sub(1));
    let right = relx.fract() > 0.5;
    (top_line + row as i32, col, right)
}

/// Bytes a key press sends to the pty, or `None` when the key is unmapped (plain text is
/// handled separately via `Event::Text`). `Ctrl+letter` wins over everything; then macOS
/// natural-editing: `Option+←/→` word, `Cmd+←/→` line ends, `Option/Cmd+Backspace` deletes.
pub(crate) fn key_to_bytes(key: egui::Key, mods: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key;
    if mods.ctrl {
        return ctrl_letter(key).map(|b| vec![b]);
    }
    let bytes: Vec<u8> = match key {
        Key::Enter => vec![b'\r'],
        Key::Backspace if mods.alt => b"\x1b\x7f".to_vec(), // delete previous word
        Key::Backspace if mods.command => vec![0x15],       // Ctrl-U: delete to line start
        Key::Backspace => vec![0x7f],
        Key::Tab => vec![b'\t'],
        Key::Escape => vec![0x1b],
        Key::ArrowUp => b"\x1b[A".to_vec(),
        Key::ArrowDown => b"\x1b[B".to_vec(),
        Key::ArrowRight if mods.alt => b"\x1bf".to_vec(), // forward word (readline)
        Key::ArrowRight if mods.command => vec![0x05],    // Ctrl-E: end of line
        Key::ArrowRight => b"\x1b[C".to_vec(),
        Key::ArrowLeft if mods.alt => b"\x1bb".to_vec(), // backward word (readline)
        Key::ArrowLeft if mods.command => vec![0x01],    // Ctrl-A: start of line
        Key::ArrowLeft => b"\x1b[D".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// Control code for `Ctrl+<letter>` (Ctrl-A = 1 .. Ctrl-Z = 26), or `None` for non-letters.
pub(crate) fn ctrl_letter(key: egui::Key) -> Option<u8> {
    use egui::Key;
    let n = match key {
        Key::A => 1,
        Key::B => 2,
        Key::C => 3,
        Key::D => 4,
        Key::E => 5,
        Key::F => 6,
        Key::G => 7,
        Key::H => 8,
        Key::I => 9,
        Key::J => 10,
        Key::K => 11,
        Key::L => 12,
        Key::M => 13,
        Key::N => 14,
        Key::O => 15,
        Key::P => 16,
        Key::Q => 17,
        Key::R => 18,
        Key::S => 19,
        Key::T => 20,
        Key::U => 21,
        Key::V => 22,
        Key::W => 23,
        Key::X => 24,
        Key::Y => 25,
        Key::Z => 26,
        _ => return None,
    };
    Some(n)
}

/// Fill fraction (0..=1) for a tab progress bar, or `None` to hide it. Determinate states
/// fill by percentage; error/indeterminate fill fully (color carries the meaning).
pub(crate) fn progress_fraction(p: Progress) -> Option<f32> {
    match p {
        Progress::None => None,
        Progress::Normal(v) | Progress::Paused(v) => Some(f32::from(v) / 100.0),
        Progress::Error(_) | Progress::Indeterminate => Some(1.0),
    }
}

/// Toast opacity (0..=1): full until `remaining_s` drops below `fade_window_s`, then linear out.
pub(crate) fn toast_alpha(remaining_s: f64, fade_window_s: f64) -> f32 {
    (remaining_s / fade_window_s).clamp(0.0, 1.0) as f32
}

// ---- egui drawing widgets (thin; not unit-tested) ----

/// Apply window opacity to a fill color (straight alpha).
pub(crate) fn tint(c: egui::Color32, opacity: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (opacity.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

pub(crate) fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = colors::bg();
    v.window_fill = colors::bg();
    v.override_text_color = Some(colors::fg());
    ctx.set_visuals(v);
}

/// A fixed-size Phosphor-icon button with hover feedback. Returns the Response (so callers
/// can anchor a popup or read `.clicked()`).
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 30.0), egui::Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 6.0, colors::hover());
    }
    let color = if hovered { colors::fg() } else { colors::dim() };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(17.0),
        color,
    );
    resp.on_hover_text(tip)
}

/// Paint a small pill-shaped status toast centered near the bottom edge. `fade` in 0..1
/// scales opacity so the message dissolves as it expires.
pub(crate) fn draw_toast(ui: &egui::Ui, msg: &str, fade: f32) {
    let a = |c: egui::Color32, base: u8| {
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (f32::from(base) * fade) as u8)
    };
    let area = ui.max_rect();
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(msg.to_owned(), font.clone(), colors::fg());
    let pad = egui::vec2(14.0, 8.0);
    let size = galley.size() + pad * 2.0;
    let center = egui::pos2(area.center().x, area.bottom() - 34.0);
    let rect = egui::Rect::from_center_size(center, size);
    let p = ui.painter();
    p.rect_filled(rect, 8.0, a(colors::elevated(), 235));
    p.rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, a(colors::border(), 255)),
        egui::StrokeKind::Inside,
    );
    p.text(center, egui::Align2::CENTER_CENTER, msg, font, a(colors::fg(), 255));
}

/// `(fraction, color)` for the tab progress bar, or `None` to hide it.
fn progress_bar(p: Progress) -> Option<(f32, egui::Color32)> {
    let frac = progress_fraction(p)?;
    let color = match p {
        Progress::Normal(_) => colors::green(),
        Progress::Paused(_) => colors::yellow(),
        Progress::Error(_) => colors::red(),
        Progress::Indeterminate => colors::accent(),
        Progress::None => return None,
    };
    Some((frac, color))
}

/// Flat Tabby-style tab: dark bg (elevated when active), optional per-tab colored underline,
/// and progress rendered as a thin bar on the TOP edge. Returns (click response, close-clicked).
pub(crate) fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
    title: &str,
    active: bool,
    color: Option<egui::Color32>,
    progress: Progress,
) -> (egui::Response, bool) {
    let (shown, truncated) = ellipsize(title, 14);
    let fg = if active { colors::fg() } else { colors::dim() };
    // trailing spaces reserve room for the close x
    let mut rt = egui::RichText::new(format!("{idx}  {shown}   ")).color(fg).monospace();
    if active {
        rt = rt.strong();
    }
    let fill = if active { colors::elevated() } else { egui::Color32::TRANSPARENT };
    let inner = egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }) // top-rounded tab shape
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let lbl = ui.add(egui::Label::new(rt).selectable(false));
            if truncated {
                lbl.on_hover_text(title);
            }
        });
    let rect = inner.response.rect;
    // A foreground-layer painter: the row layout's clip cuts off the tab's top/bottom edges,
    // so edge strokes (underline, progress) must be drawn on an unclipped layer.
    let dp = ui
        .ctx()
        .layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tab_deco")));
    // Per-tab color underline (bottom edge) - only when the user set a color.
    if let Some(color) = color {
        dp.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() - 3.0),
                egui::vec2(rect.width(), 3.0),
            ),
            0.0,
            color,
        );
    }
    // Progress bar (top edge).
    if let Some((frac, pcolor)) = progress_bar(progress) {
        dp.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top()),
                egui::vec2(rect.width() * frac, 2.0),
            ),
            0.0,
            pcolor,
        );
    }
    // Close x, revealed on the active or hovered tab, with its own hover feedback.
    let mut close = false;
    if active || inner.response.hovered() {
        let x_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 22.0, rect.center().y - 9.0),
            egui::vec2(18.0, 18.0),
        );
        let xr = ui.interact(x_rect, ui.id().with(("close", idx)), egui::Sense::click());
        let xh = xr.hovered();
        if xh {
            ui.painter().rect_filled(x_rect, 5.0, colors::hover());
        }
        ui.painter().text(
            x_rect.center(),
            egui::Align2::CENTER_CENTER,
            icons::X,
            egui::FontId::proportional(13.0),
            if xh { colors::fg() } else { colors::dim() },
        );
        if xr.clicked() {
            close = true;
        }
    }
    (inner.response.interact(egui::Sense::click()), close)
}

/// Translate this frame's key/text events into bytes for the pty.
pub(crate) fn collect_input(ui: &egui::Ui) -> Vec<u8> {
    let mut out = Vec::new();
    ui.input(|i| {
        for event in &i.events {
            match event {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(bytes) = key_to_bytes(*key, *modifiers) {
                        out.extend_from_slice(&bytes);
                    }
                }
                _ => {}
            }
        }
    });
    out
}

/// Paint the terminal grid (per-cell bg + selection overlay + fg glyph + beam cursor) and
/// drive mouse text selection: drag to select, double/triple-click to select word/line,
/// click to clear.
pub(crate) fn render_grid(
    ui: &mut egui::Ui,
    term: &PtyTerm,
    snap: &GridSnap,
    cw: f32,
    ch: f32,
    font: &egui::FontId,
) {
    let size = egui::vec2(cw * snap.cols as f32, ch * snap.rows as f32);
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let origin = resp.rect.min;
    let hit = |p: egui::Pos2| {
        pos_to_cell(p.x - origin.x, p.y - origin.y, cw, ch, snap.cols, snap.rows, snap.top_line)
    };
    if resp.triple_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (line, col, _) = hit(p);
            term.select_line(line, col);
        }
    } else if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (line, col, _) = hit(p);
            term.select_word(line, col);
        }
    } else if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (line, col, right) = hit(p);
            term.start_selection(line, col, right);
        }
    } else if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (line, col, right) = hit(p);
            term.update_selection(line, col, right);
        }
    } else if resp.clicked() {
        term.clear_selection();
    }

    for r in 0..snap.rows {
        for c in 0..snap.cols {
            let cell = &snap.cells[r * snap.cols + c];
            let pos = origin + egui::vec2(c as f32 * cw, r as f32 * ch);
            let rect = egui::Rect::from_min_size(pos, egui::vec2(cw, ch));
            if let Some(bg) = cell.bg {
                painter.rect_filled(rect, 0.0, bg);
            }
            if cell.selected {
                painter.rect_filled(rect, 0.0, colors::selection());
            }
            if cell.c != ' ' && cell.c != '\0' {
                painter.text(pos, egui::Align2::LEFT_TOP, cell.c, font.clone(), cell.fg);
            }
        }
    }

    // Beam cursor (block/underline styles land in M9); hidden while scrolled into history.
    if let Some((cr, cc)) = snap.cursor {
        let cpos = origin + egui::vec2(cc as f32 * cw, cr as f32 * ch);
        painter.rect_filled(
            egui::Rect::from_min_size(cpos, egui::vec2(2.0, ch)),
            0.0,
            colors::cursor(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    fn mods(ctrl: bool, alt: bool, command: bool) -> Modifiers {
        Modifiers { alt, ctrl, shift: false, mac_cmd: command, command }
    }

    #[test]
    fn basename_cases() {
        assert_eq!(basename("/tmp/foo"), "foo");
        assert_eq!(basename("/tmp/foo/"), "foo"); // trailing slash ignored
        assert_eq!(basename("foo"), "foo");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "/");
    }

    #[test]
    fn ellipsize_marks_only_when_truncated() {
        assert_eq!(ellipsize("short", 10), ("short".into(), false));
        assert_eq!(ellipsize("exactly-ten", 11), ("exactly-ten".into(), false));
        let (shown, trunc) = ellipsize("a-very-long-tab-title", 6);
        assert!(trunc);
        assert_eq!(shown.chars().count(), 6); // 5 chars + ellipsis
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn ellipsize_counts_chars_not_bytes() {
        // Multi-byte chars: 5 emoji is under a max of 6, so untouched.
        let (shown, trunc) = ellipsize("🐱🐱🐱🐱🐱", 6);
        assert!(!trunc);
        assert_eq!(shown, "🐱🐱🐱🐱🐱");
    }

    #[test]
    fn pos_to_cell_basic_and_right_half() {
        // 8x16 cells, click near left of col 2, row 1.
        let (line, col, right) = pos_to_cell(2.0 * 8.0 + 1.0, 16.0 + 2.0, 8.0, 16.0, 80, 24, 0);
        assert_eq!((line, col, right), (1, 2, false));
        // Past the midpoint of col 3 -> right half.
        let (_, col, right) = pos_to_cell(3.0 * 8.0 + 6.0, 0.0, 8.0, 16.0, 80, 24, 0);
        assert_eq!((col, right), (3, true));
    }

    #[test]
    fn pos_to_cell_clamps_and_offsets() {
        // Way past the right/bottom edge clamps to the last cell.
        let (line, col, _) = pos_to_cell(9999.0, 9999.0, 8.0, 16.0, 80, 24, 0);
        assert_eq!((line, col), (23, 79));
        // Negative offset clamps to origin.
        let (line, col, _) = pos_to_cell(-50.0, -50.0, 8.0, 16.0, 80, 24, 0);
        assert_eq!((line, col), (0, 0));
        // top_line offset (scrolled 5 into history) shifts the buffer line.
        let (line, _, _) = pos_to_cell(0.0, 3.0 * 16.0, 8.0, 16.0, 80, 24, -5);
        assert_eq!(line, -2);
    }

    #[test]
    fn key_to_bytes_plain_and_ctrl() {
        assert_eq!(key_to_bytes(Key::Enter, mods(false, false, false)), Some(vec![b'\r']));
        assert_eq!(key_to_bytes(Key::Backspace, mods(false, false, false)), Some(vec![0x7f]));
        assert_eq!(key_to_bytes(Key::C, mods(true, false, false)), Some(vec![3])); // Ctrl-C SIGINT
        assert_eq!(key_to_bytes(Key::Enter, mods(true, false, false)), None); // Ctrl+non-letter
        assert_eq!(key_to_bytes(Key::F5, mods(false, false, false)), None); // unmapped
    }

    #[test]
    fn key_to_bytes_natural_editing() {
        // Option (alt) + arrows -> word motion; Cmd + arrows -> line ends.
        assert_eq!(key_to_bytes(Key::ArrowLeft, mods(false, true, false)), Some(b"\x1bb".to_vec()));
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, true, false)),
            Some(b"\x1bf".to_vec())
        );
        assert_eq!(key_to_bytes(Key::ArrowLeft, mods(false, false, true)), Some(vec![0x01]));
        assert_eq!(key_to_bytes(Key::ArrowRight, mods(false, false, true)), Some(vec![0x05]));
        // Plain arrows keep the CSI sequences.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, false, false)),
            Some(b"\x1b[D".to_vec())
        );
        // Backspace variants.
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, true, false)),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(key_to_bytes(Key::Backspace, mods(false, false, true)), Some(vec![0x15]));
    }

    #[test]
    fn ctrl_letter_bounds() {
        assert_eq!(ctrl_letter(Key::A), Some(1));
        assert_eq!(ctrl_letter(Key::Z), Some(26));
        assert_eq!(ctrl_letter(Key::Num1), None);
    }

    #[test]
    fn progress_fraction_mapping() {
        assert_eq!(progress_fraction(Progress::None), None);
        assert_eq!(progress_fraction(Progress::Normal(50)), Some(0.5));
        assert_eq!(progress_fraction(Progress::Normal(100)), Some(1.0));
        assert_eq!(progress_fraction(Progress::Paused(20)), Some(0.2));
        assert_eq!(progress_fraction(Progress::Error(0)), Some(1.0));
        assert_eq!(progress_fraction(Progress::Indeterminate), Some(1.0));
    }

    #[test]
    fn toast_alpha_fades() {
        assert_eq!(toast_alpha(1.0, 0.35), 1.0); // still full
        assert_eq!(toast_alpha(0.35, 0.35), 1.0); // at the edge
        assert!((toast_alpha(0.175, 0.35) - 0.5).abs() < 1e-6);
        assert_eq!(toast_alpha(0.0, 0.35), 0.0);
        assert_eq!(toast_alpha(-1.0, 0.35), 0.0); // clamped
    }
}
