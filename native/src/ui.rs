//! View layer: the egui drawing widgets (tab, icon button, grid, toast) plus the pure,
//! unit-testable helpers extracted from the render loop (geometry, text, input mapping).
//! Keeping the math here - free of `Ui`/`Context` - is what makes it testable; `main.rs`'s
//! `eframe::App` loop stays a thin caller.
use eframe::egui;

use crate::colors;
use crate::progress::Progress;
use crate::terminal::{CmdState, GridSnap, PtyTerm};

/// Phosphor icon codepoints (font vendored in assets/Phosphor.ttf, MIT).
pub(crate) mod icons {
    pub(crate) const PLUS: &str = "\u{E3D4}";
    pub(crate) const X: &str = "\u{E4F6}";
    pub(crate) const GEAR: &str = "\u{E270}";
    pub(crate) const APP_WINDOW: &str = "\u{E5DA}";
    pub(crate) const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub(crate) const CARET_UP: &str = "\u{E13C}";
    pub(crate) const CARET_DOWN: &str = "\u{E136}";
    pub(crate) const TEXT_AA: &str = "\u{E6EE}"; // case sensitivity
    pub(crate) const ASTERISK: &str = "\u{E0AA}"; // regex
    pub(crate) const BRACKETS_SQUARE: &str = "\u{E85E}"; // whole word
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
pub(crate) fn key_to_bytes(
    key: egui::Key,
    mods: egui::Modifiers,
    alt_is_meta: bool,
) -> Option<Vec<u8>> {
    use egui::Key;
    if mods.ctrl {
        return ctrl_letter(key).map(|b| vec![b]);
    }
    // altIsMeta: Option+letter/digit sends ESC-prefixed keys (like xterm macOptionIsMeta) instead
    // of macOS composed characters. Arrows/backspace keep their word-motion mappings below.
    if alt_is_meta
        && mods.alt
        && !mods.command
        && let Some(n) = ctrl_letter(key)
    {
        return Some(vec![0x1b, b'a' + n - 1]);
    }
    let bytes: Vec<u8> = match key {
        // Cmd+Alt+{arrows,Enter} are app pane bindings (nav / maximize) - don't forward to the pty.
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Enter
            if mods.command && mods.alt =>
        {
            return None;
        }
        // Cmd+Shift+arrows are app tab bindings (move tab) - don't forward either.
        Key::ArrowLeft | Key::ArrowRight if mods.command && mods.shift => return None,
        Key::Enter => vec![b'\r'],
        Key::Backspace if mods.alt => b"\x1b\x7f".to_vec(), // delete previous word
        Key::Backspace if mods.command => vec![0x15],       // Ctrl-U: delete to line start
        Key::Backspace => vec![0x7f],
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(), // back-tab (CSI Z) - apps cycle on this
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

/// Cursor blink phase: on for the first half of each ~1.06s cycle (xterm-ish cadence).
pub(crate) fn blink_on(time: f64) -> bool {
    time.rem_euclid(1.06) < 0.53
}

/// Normalize pasted text the way Tabby does (`baseTerminalTab.paste()`): CRLF/LF -> CR (shells
/// expect CR), then optionally collapse newline runs to single spaces. Returns the normalized
/// text; multiline-ness afterwards is simply `contains('\r')`.
pub(crate) fn normalize_paste(input: &str, newlines_to_spaces: bool) -> String {
    let s = input.replace("\r\n", "\r").replace('\n', "\r");
    if newlines_to_spaces {
        // Collapse runs of CRs into one space (Tabby: /[\r\n]+/g -> ' ').
        let mut out = String::with_capacity(s.len());
        let mut in_run = false;
        for ch in s.chars() {
            if ch == '\r' {
                if !in_run {
                    out.push(' ');
                    in_run = true;
                }
            } else {
                in_run = false;
                out.push(ch);
            }
        }
        out
    } else {
        s
    }
}

/// Tabby's trim rule for the NON-warned paste path: trim the end always; trim the start only when
/// the (already-trimmed) paste is single-line. The multiline-warning modal path skips this.
pub(crate) fn trim_paste(s: &str, trim: bool) -> String {
    if !trim {
        return s.to_owned();
    }
    let t = s.trim_end();
    if t.contains('\r') { t.to_owned() } else { t.trim_start().to_owned() }
}

/// Terminal cursor shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorStyle {
    Block,
    Underline,
    Beam,
}

/// Parse the config `cursor` string; unknown values fall back to `Block` (the default).
pub(crate) fn cursor_style(s: &str) -> CursorStyle {
    match s.to_ascii_lowercase().as_str() {
        "underline" | "under" => CursorStyle::Underline,
        "beam" | "bar" | "ibeam" => CursorStyle::Beam,
        _ => CursorStyle::Block,
    }
}

/// Is the configured link-activation modifier satisfied? `"none"` (or unknown) means links react
/// on plain hover/click (Tabby default); otherwise the named modifier must be held.
pub(crate) fn link_modifier_held(mods: egui::Modifiers, setting: &str) -> bool {
    match setting.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "meta" => mods.command,
        "ctrl" | "control" => mods.ctrl,
        "alt" | "option" | "opt" => mods.alt,
        "shift" => mods.shift,
        _ => true, // "none" / unrecognized: no modifier required
    }
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

/// A filled-circle color swatch for the tab Color menu. Draws a bright ring when `selected`, a
/// dim ring on hover; returns the Response.
pub(crate) fn color_swatch(
    ui: &mut egui::Ui,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 24.0), egui::Sense::click());
    let center = rect.center();
    let ring = if selected {
        Some((colors::fg(), 2.0))
    } else if resp.hovered() {
        Some((colors::dim(), 1.5))
    } else {
        None
    };
    if let Some((c, w)) = ring {
        ui.painter().circle_stroke(center, 10.0, egui::Stroke::new(w, c));
    }
    ui.painter().circle_filled(center, 8.0, color);
    resp
}

// ---- design-system primitives (use these; don't hand-roll surfaces/inputs/buttons) ----

/// The standard floating-overlay surface: elevated fill, hairline border, rounded corners, soft
/// shadow. Use for every popover/dialog (find bar, rename) so they read identically.
pub(crate) fn overlay_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::elevated())
        .stroke(egui::Stroke::new(1.0, colors::border()))
        .corner_radius(12)
        .shadow(egui::epaint::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: egui::Color32::from_black_alpha(100),
        })
        .inner_margin(egui::Margin::symmetric(12, 8))
}

/// The standard single-line text input: uniform 15pt font, theme-colored field bg, consistent
/// inner padding. `color` tints the typed text (e.g. red for no-results). Returns the Response.
pub(crate) fn text_field(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    color: egui::Color32,
) -> egui::Response {
    ui.style_mut().override_font_id = Some(egui::FontId::proportional(15.0));
    ui.visuals_mut().extreme_bg_color = colors::bg();
    ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(width)
            .margin(egui::Margin::symmetric(8, 6))
            .text_color(color)
            .hint_text(hint),
    )
}

/// The standard action button (dialog OK/Cancel etc.): consistent padding; `primary` fills with
/// the accent so the default action stands out. Returns the Response.
pub(crate) fn action_button(ui: &mut egui::Ui, label: &str, primary: bool) -> egui::Response {
    ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
    let text = egui::RichText::new(label).color(if primary { colors::bg() } else { colors::fg() });
    let mut btn = egui::Button::new(text).corner_radius(7);
    if primary {
        btn = btn.fill(colors::accent());
    }
    ui.add(btn)
}

/// Give a context menu / popup room to breathe (wider, roomier rows). Call at the top of every
/// menu closure AND its submenus so they stay consistent.
pub(crate) fn style_menu(ui: &mut egui::Ui) {
    ui.set_min_width(210.0);
    let s = ui.spacing_mut();
    s.button_padding = egui::vec2(12.0, 7.0);
    s.item_spacing.y = 3.0;
}

/// Tab-bar icon-button size (shared so the tab bar can right-align the gear by a spacer).
pub(crate) const ICON_BTN_W: f32 = 32.0;
const ICON_BTN_H: f32 = 30.0;

/// A fixed-size Phosphor-icon button with hover feedback. Returns the Response (so callers
/// can anchor a popup or read `.clicked()`).
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ICON_BTN_W, ICON_BTN_H), egui::Sense::click());
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

/// A toggle variant of `icon_button`: tinted accent fill + accent glyph while `active`.
pub(crate) fn icon_toggle(
    ui: &mut egui::Ui,
    icon: &str,
    active: bool,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), egui::Sense::click());
    let hovered = resp.hovered();
    if active {
        ui.painter().rect_filled(rect, 6.0, colors::selection());
    } else if hovered {
        ui.painter().rect_filled(rect, 6.0, colors::hover());
    }
    let color = if active {
        colors::accent()
    } else if hovered {
        colors::fg()
    } else {
        colors::dim()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(15.0),
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

/// A tiny glyph on the tab that previews the pane split layout (nested rectangles). `rects` are
/// the leaf rects of `Pane::miniature()` in a unit square; drawn only when there's >1 pane.
fn draw_mini_layout(ui: &mut egui::Ui, rects: &[egui::Rect], active: bool) {
    let (box_rect, _) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::hover());
    let inner = box_rect.shrink(1.0);
    let col = if active { colors::fg() } else { colors::dim() };
    let painter = ui.painter();
    for r in rects {
        let cell = egui::Rect::from_min_max(
            inner.min + egui::vec2(r.min.x * inner.width(), r.min.y * inner.height()),
            inner.min + egui::vec2(r.max.x * inner.width(), r.max.y * inner.height()),
        );
        painter.rect_filled(cell, 1.0, col);
    }
}

/// A small brand-colored pill naming a known AI CLI running in the tab (Claude / Gemini / ...).
fn draw_cli_badge(ui: &mut egui::Ui, cli: crate::procwatch::Cli) {
    let col = cli.color();
    let label = cli.label();
    let font = egui::FontId::proportional(10.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), col);
    let (rect, resp) =
        ui.allocate_exact_size(galley.size() + egui::vec2(10.0, 4.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, col.gamma_multiply(0.18)); // tinted fill
    p.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, col.gamma_multiply(0.55)),
        egui::StrokeKind::Inside,
    );
    p.text(rect.center(), egui::Align2::CENTER_CENTER, label, font, col);
    resp.on_hover_text(format!("{label} is running in this tab"));
}

/// Flat Tabby-style tab: dark bg (elevated when active), optional per-tab colored underline, a
/// split-layout preview glyph, and progress as a thin bar on the TOP edge. Returns (click
/// response, close-clicked). `layout` = `Pane::miniature()` leaf rects (glyph shown when >1).
pub(crate) fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
    title: &str,
    active: bool,
    color: Option<egui::Color32>,
    progress: Progress,
    cmd: CmdState,
    layout: &[egui::Rect],
    cli: Option<crate::procwatch::Cli>,
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
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                if layout.len() > 1 {
                    draw_mini_layout(ui, layout, active);
                }
                let lbl = ui.add(egui::Label::new(rt).selectable(false));
                if truncated {
                    lbl.on_hover_text(title);
                }
                if let Some(cli) = cli {
                    draw_cli_badge(ui, cli);
                }
            });
        });
    let rect = inner.response.rect;
    // A foreground-layer painter: the row layout's clip cuts off the tab's top/bottom edges,
    // so edge strokes (underline, progress) must be drawn on an unclipped layer.
    let dp =
        ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("tab_deco")));
    // OSC 133 exit state on the tab's left edge: only a failed command gets a signal, and a very
    // subtle one. Progress reporting (the top-edge %-bar) is the primary "something is happening"
    // cue; a per-command running/success indicator was pure noise (it lit up for any idle REPL
    // like an open Claude CLI), so idle / running / ok all draw nothing.
    if cmd == CmdState::Fail {
        let h = 12.0_f32.min(rect.height() - 10.0);
        let track = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 1.0, rect.center().y - h / 2.0),
            egui::vec2(2.0, h),
        );
        dp.rect_filled(track, 1.0, colors::red().gamma_multiply(0.8));
    }
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
    // Interact the whole tab FIRST so the close-x (registered after, hence on top) wins its own
    // clicks. Previously the tab's click was registered last and stole the x's click, so clicking
    // the x just focused the tab instead of closing it.
    let tab_resp = inner.response.interact(egui::Sense::click());
    // Close x: shown on the active tab, or whenever the pointer is anywhere over the tab. Use
    // `contains_pointer` (true across the whole tab rect, incl. the x) rather than `hovered` so
    // moving onto the x doesn't drop the tab's hover state and make the x flicker.
    let mut close = false;
    if active || tab_resp.contains_pointer() {
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
    (tab_resp, close)
}

/// Translate this frame's key/text events into bytes for the pty.
pub(crate) fn collect_input(ui: &egui::Ui, alt_is_meta: bool, intercept_ctrl_c: bool) -> Vec<u8> {
    let mut out = Vec::new();
    ui.input(|i| {
        let alt_held = i.modifiers.alt;
        for event in &i.events {
            match event {
                // With altIsMeta on, Option+key already produced ESC-prefixed bytes via the Key
                // event - drop the macOS composed character so it isn't sent twice.
                egui::Event::Text(_) if alt_is_meta && alt_held => {}
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    // Intelligent Ctrl-C (Tabby): when a selection exists the caller copies it
                    // instead of us sending SIGINT.
                    if intercept_ctrl_c && *key == egui::Key::C && modifiers.ctrl {
                        continue;
                    }
                    if let Some(bytes) = key_to_bytes(*key, *modifiers, alt_is_meta) {
                        out.extend_from_slice(&bytes);
                    }
                }
                _ => {}
            }
        }
    });
    out
}

/// Paint one terminal pane inside `rect`: per-cell bg + selection overlay + glyph + cursor +
/// a scrollbar, and drive mouse selection (drag / double / triple click) + wheel scroll. When
/// `dimmed` fades the pane toward the background (used for unfocused panes in a split). Returns
/// the pane's `Response` (so the caller can focus on click/drag + attach a context menu).
/// `id_src` must be unique per pane (its path).
pub(crate) fn render_grid(
    ui: &mut egui::Ui,
    id_src: &[crate::pane::Side],
    rect: egui::Rect,
    term: &PtyTerm,
    snap: &GridSnap,
    cw: f32,
    ch: f32,
    font: &egui::FontId,
    cursor: CursorStyle,
    dimmed: bool,
    link_active: bool,
    blink: bool,
) -> egui::Response {
    let resp = ui.interact(rect, egui::Id::new(id_src), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let origin = rect.min;
    let hit = |p: egui::Pos2| {
        pos_to_cell(p.x - origin.x, p.y - origin.y, cw, ch, snap.cols, snap.rows, snap.top_line)
    };

    // Clickable links: when active (enabled + configured modifier held, or no modifier), underline
    // the link under the pointer and open it on click. Kept off the selection path so plain drags
    // still select text.
    let mut link_underline: Option<(f32, f32, f32)> = None; // (x0, x1, y)
    if link_active
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && rect.contains(p)
    {
        let row = (((p.y - origin.y) / ch) as usize).min(snap.rows.saturating_sub(1));
        let row_text: String = (0..snap.cols).map(|c| snap.cells[row * snap.cols + c].c).collect();
        let col = (((p.x - origin.x) / cw) as usize).min(snap.cols.saturating_sub(1));
        if let Some(link) = crate::links::find_in_row(&row_text)
            .into_iter()
            .find(|l| col >= l.start && col < l.start + l.len)
        {
            link_underline = Some((
                origin.x + link.start as f32 * cw,
                origin.x + (link.start + link.len) as f32 * cw,
                origin.y + (row as f32 + 1.0) * ch - 1.5,
            ));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                let text: String = row_text.chars().skip(link.start).take(link.len).collect();
                crate::links::open(&text, link.kind, term.cwd().as_deref());
            }
        }
    }
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

    // Unfocused panes fade their CONTENT (Tabby-style) by scaling its alpha down, leaving the
    // blank cells transparent at the window's global opacity - so the glass stays uniform and
    // only the content recedes (no opaque scrim that would change see-through).
    let fade = |c: egui::Color32| if dimmed { c.gamma_multiply(0.5) } else { c };
    for r in 0..snap.rows {
        for c in 0..snap.cols {
            let cell = &snap.cells[r * snap.cols + c];
            let pos = origin + egui::vec2(c as f32 * cw, r as f32 * ch);
            let cell_rect = egui::Rect::from_min_size(pos, egui::vec2(cw, ch));
            if let Some(bg) = cell.bg {
                painter.rect_filled(cell_rect, 0.0, fade(bg));
            }
            if cell.selected {
                painter.rect_filled(cell_rect, 0.0, fade(colors::selection()));
            }
            if cell.c != ' ' && cell.c != '\0' {
                painter.text(pos, egui::Align2::LEFT_TOP, cell.c, font.clone(), fade(cell.fg));
            }
        }
    }

    // Underline the hovered (command-held) link.
    if let Some((x0, x1, y)) = link_underline {
        painter.hline(x0..=x1, y, egui::Stroke::new(1.0, fade(colors::accent())));
    }

    // Cursor blink (focused pane only, like xterm): skip the draw during the off phase and
    // schedule a repaint at the next phase flip so the cadence keeps ticking.
    let blink_hidden = if blink && !dimmed {
        let time = ui.input(|i| i.time);
        let next_flip = 0.53 - time.rem_euclid(0.53);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(next_flip.max(0.01)));
        !blink_on(time)
    } else {
        false
    };

    // Cursor (block/underline/beam); hidden while scrolled into history.
    if let Some((cr, cc)) = snap.cursor
        && !blink_hidden
    {
        let cpos = origin + egui::vec2(cc as f32 * cw, cr as f32 * ch);
        let cur = fade(colors::cursor());
        match cursor {
            CursorStyle::Beam => {
                painter.rect_filled(egui::Rect::from_min_size(cpos, egui::vec2(2.0, ch)), 0.0, cur);
            }
            CursorStyle::Underline => {
                let u = egui::pos2(cpos.x, cpos.y + ch - 2.0);
                painter.rect_filled(egui::Rect::from_min_size(u, egui::vec2(cw, 2.0)), 0.0, cur);
            }
            CursorStyle::Block => {
                painter.rect_filled(egui::Rect::from_min_size(cpos, egui::vec2(cw, ch)), 0.0, cur);
                // Redraw the glyph under the block in the background color so it stays legible.
                let glyph = snap.cells[cr * snap.cols + cc].c;
                if glyph != ' ' && glyph != '\0' {
                    painter.text(
                        cpos,
                        egui::Align2::LEFT_TOP,
                        glyph,
                        font.clone(),
                        fade(colors::bg()),
                    );
                }
            }
        }
    }

    pane_scrollbar(ui, id_src, rect, term, snap.rows);
    resp
}

/// Draggable scrollback thumb on the right edge of a pane's `rect`.
fn pane_scrollbar(
    ui: &mut egui::Ui,
    id_src: &[crate::pane::Side],
    rect: egui::Rect,
    term: &PtyTerm,
    rows: usize,
) {
    let (offset, history) = term.scroll_state();
    if history == 0 {
        return;
    }
    let track = rect.height();
    let total = (history + rows) as f32;
    let thumb_h = (rows as f32 / total * track).max(24.0);
    let top_frac = (history - offset) as f32 / total;
    let thumb_y = rect.top() + top_frac * track;
    let bar_x = rect.right() - 6.0;

    let track_rect = egui::Rect::from_min_max(
        egui::pos2(bar_x - 2.0, rect.top()),
        egui::pos2(rect.right(), rect.bottom()),
    );
    let resp =
        ui.interact(track_rect, egui::Id::new((id_src, "sb")), egui::Sense::click_and_drag());
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let frac = ((p.y - rect.top()) / track).clamp(0.0, 1.0);
        let target = ((1.0 - frac) * history as f32).round() as usize;
        term.scroll_to_offset(target.min(history));
    }
    let alpha = if resp.hovered() || resp.dragged() { 180 } else { 90 };
    let d = colors::dim();
    let col = egui::Color32::from_rgba_unmultiplied(d.r(), d.g(), d.b(), alpha);
    ui.painter_at(rect).rect_filled(
        egui::Rect::from_min_size(egui::pos2(bar_x, thumb_y), egui::vec2(4.0, thumb_h)),
        2.0,
        col,
    );
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
        assert_eq!(key_to_bytes(Key::Enter, mods(false, false, false), false), Some(vec![b'\r']));
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, false, false), false),
            Some(vec![0x7f])
        );
        assert_eq!(key_to_bytes(Key::C, mods(true, false, false), false), Some(vec![3])); // Ctrl-C SIGINT
        assert_eq!(key_to_bytes(Key::Enter, mods(true, false, false), false), None); // Ctrl+non-letter
        assert_eq!(key_to_bytes(Key::F5, mods(false, false, false), false), None); // unmapped
    }

    #[test]
    fn key_to_bytes_natural_editing() {
        // Option (alt) + arrows -> word motion; Cmd + arrows -> line ends.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, true, false), false),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, true, false), false),
            Some(b"\x1bf".to_vec())
        );
        assert_eq!(key_to_bytes(Key::ArrowLeft, mods(false, false, true), false), Some(vec![0x01]));
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, false, true), false),
            Some(vec![0x05])
        );
        // Plain arrows keep the CSI sequences.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, false, false), false),
            Some(b"\x1b[D".to_vec())
        );
        // Backspace variants.
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, true, false), false),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(key_to_bytes(Key::Backspace, mods(false, false, true), false), Some(vec![0x15]));
    }

    #[test]
    fn paste_normalization_matches_tabby() {
        // CRLF and LF both become CR.
        assert_eq!(normalize_paste("a\r\nb\nc", false), "a\rb\rc");
        // newlines->spaces collapses runs into ONE space.
        assert_eq!(normalize_paste("a\n\n\nb", true), "a b");
        assert_eq!(normalize_paste("ls -la\n", true), "ls -la ");
    }

    #[test]
    fn paste_trim_rules() {
        // Single-line: both ends trimmed.
        assert_eq!(trim_paste("  ls -la \r", true), "ls -la");
        // Multiline (still contains \r after end-trim): only the end is trimmed.
        assert_eq!(trim_paste("  a\rb  \r", true), "  a\rb");
        // Disabled: untouched.
        assert_eq!(trim_paste("  x  ", false), "  x  ");
    }

    #[test]
    fn blink_phase_toggles() {
        assert!(blink_on(0.0));
        assert!(blink_on(0.52));
        assert!(!blink_on(0.54));
        assert!(blink_on(1.07)); // next cycle
    }

    #[test]
    fn alt_is_meta_sends_esc_prefixed_letters() {
        let alt = mods(false, true, false);
        assert_eq!(key_to_bytes(Key::B, alt, true), Some(vec![0x1b, b'b']));
        // Off: unmapped (macOS composes a Text event instead).
        assert_eq!(key_to_bytes(Key::B, alt, false), None);
        // Word-motion arrows unchanged even with altIsMeta.
        assert_eq!(key_to_bytes(Key::ArrowLeft, alt, true), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn cmd_shift_arrows_are_reserved_for_move_tab() {
        let m = egui::Modifiers { shift: true, command: true, mac_cmd: true, ..Default::default() };
        assert_eq!(key_to_bytes(Key::ArrowLeft, m, false), None);
        assert_eq!(key_to_bytes(Key::ArrowRight, m, false), None);
    }

    #[test]
    fn link_modifier_matching() {
        let none = Modifiers::default();
        let cmd = mods(false, false, true);
        assert!(link_modifier_held(none, "none")); // plain hover default
        assert!(link_modifier_held(none, "")); // unknown -> no modifier needed
        assert!(!link_modifier_held(none, "cmd")); // cmd required but not held
        assert!(link_modifier_held(cmd, "cmd"));
        assert!(link_modifier_held(mods(true, false, false), "ctrl"));
        assert!(!link_modifier_held(cmd, "alt"));
    }

    #[test]
    fn cmd_alt_combos_are_reserved_for_panes() {
        // Cmd+Alt+arrows/Enter are app pane bindings; they must not reach the pty.
        assert_eq!(key_to_bytes(Key::ArrowLeft, mods(false, true, true), false), None);
        assert_eq!(key_to_bytes(Key::ArrowRight, mods(false, true, true), false), None);
        assert_eq!(key_to_bytes(Key::ArrowUp, mods(false, true, true), false), None);
        assert_eq!(key_to_bytes(Key::ArrowDown, mods(false, true, true), false), None);
        assert_eq!(key_to_bytes(Key::Enter, mods(false, true, true), false), None);
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::Tab, shift, false), Some(b"\x1b[Z".to_vec())); // apps cycle on this
        assert_eq!(key_to_bytes(Key::Tab, Modifiers::default(), false), Some(vec![b'\t'])); // plain tab
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
    fn cursor_style_parse() {
        assert_eq!(cursor_style("block"), CursorStyle::Block);
        assert_eq!(cursor_style("Underline"), CursorStyle::Underline);
        assert_eq!(cursor_style("beam"), CursorStyle::Beam);
        assert_eq!(cursor_style("bar"), CursorStyle::Beam);
        assert_eq!(cursor_style("nonsense"), CursorStyle::Block); // fallback
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
