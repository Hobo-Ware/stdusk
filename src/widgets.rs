//! Design-system UI primitives: the reusable egui widgets (surfaces, inputs, buttons, toggles,
//! chips, menu rows, icon buttons) that the chrome (settings, finder, tab bar, palette) builds
//! on so every control reads identically. Pure egui - no terminal-grid drawing (that stays in
//! `ui.rs`). Use these; don't hand-roll surfaces/inputs/buttons.
use eframe::egui;

use crate::colors;
use crate::ui::icons;

/// A filled-circle color swatch for the tab Color menu. Draws a bright ring when `selected`, a
/// dim ring on hover; returns the Response.
pub(crate) fn color_swatch(
    ui: &mut egui::Ui,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 24.0), egui::Sense::click());
    let center = rect.center();
    // Keyboard focus wins the ring (accent), then the selected/hover treatments. Memory
    // focus, not Response::has_focus - same rationale as `focus_ring`.
    let ring = if ui.ctx().memory(|m| m.has_focus(resp.id)) {
        Some((colors::accent(), 2.0))
    } else if selected {
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

/// Clamped integer parse for `int_field`: digits only, empty -> None, clamped into range.
pub(crate) fn parse_int_clamped(s: &str, min: usize, max: usize) -> Option<usize> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    digits.parse::<usize>().ok().map(|v| v.clamp(min, max))
}

/// One `num_field` stepper press: `up` adds `step`, else subtracts; saturating and clamped.
pub(crate) fn step_int(v: usize, step: usize, up: bool, min: usize, max: usize) -> usize {
    if up { v.saturating_add(step).min(max) } else { v.saturating_sub(step).max(min) }
}

/// A small square +/- stepper button matching the `text_field` look (field bg, hairline
/// border, hover fill, painted glyph). Sized to the field's height so the group reads as one.
fn stepper_button(ui: &mut egui::Ui, icon: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 31.0), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 7.0, if resp.hovered() { colors::hover_elevated() } else { colors::bg() });
    p.rect_stroke(rect, 7.0, egui::Stroke::new(1.0, colors::border()), egui::StrokeKind::Inside);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(13.0),
        if resp.hovered() { colors::fg() } else { colors::dim() },
    );
    focus_ring(ui, &resp, 7.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The standard numeric input: a `text_field` bound to a `usize` (digits only, clamped into
/// `range` as you type; the shown text snaps to the real value on blur) plus small +/-
/// steppers that nudge by `step`. Matches the rest of the inputs visually - never use a raw
/// `DragValue` in settings rows.
pub(crate) fn num_field(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut usize,
    range: std::ops::RangeInclusive<usize>,
    step: usize,
    width: f32,
) -> egui::Response {
    let id = ui.make_persistent_id(("num_field", id_salt));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut buf =
            ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| value.to_string());
        let r = text_field(ui, &mut buf, "", width, colors::fg());
        if r.changed()
            && let Some(v) = parse_int_clamped(&buf, *range.start(), *range.end())
        {
            *value = v;
        }
        let mut stepped = false;
        for (icon, up) in [(icons::MINUS, false), (icons::PLUS, true)] {
            if stepper_button(ui, icon).clicked() {
                *value = step_int(*value, step, up, *range.start(), *range.end());
                stepped = true;
            }
        }
        // Keyboard: Up/Down step while the field has focus (Shift = 10x). The TextEdit's own
        // event filter claims vertical arrows, so egui won't move focus - the press is ours.
        if r.has_focus() {
            let (up, down, big) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.modifiers.shift,
                )
            });
            if up != down {
                let by = if big { step.saturating_mul(10) } else { step };
                *value = step_int(*value, by, up, *range.start(), *range.end());
                stepped = true;
            }
        }
        if stepped || !r.has_focus() {
            // Not being edited (or just stepped): track the real value (covers Revert/Discard,
            // out-of-range text, and stepper presses while the field holds stale text).
            buf = value.to_string();
        }
        ui.data_mut(|d| d.insert_temp(id, buf));
        r
    })
    .inner
}

/// A small read-only value readout for sliders (field bg, hairline border, centered text).
/// Fixed min width so the chip doesn't jitter as the value's digit count changes.
fn value_chip(ui: &mut egui::Ui, text: &str) {
    let font = egui::FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, colors::fg());
    let w = (galley.size().x + 16.0).max(52.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 7.0, colors::bg());
    p.rect_stroke(rect, 7.0, egui::Stroke::new(1.0, colors::border()), egui::StrokeKind::Inside);
    p.galley(rect.center() - galley.size() / 2.0, galley, colors::fg());
}

/// The design-system slider: accent trailing fill and the live value in a styled chip
/// instead of egui's raw `DragValue`. `fmt` renders the chip text ("85%", "13 pt", ...).
pub(crate) fn slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    fmt: impl Fn(f32) -> String,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.visuals_mut().selection.bg_fill = colors::accent(); // trailing fill reads accent
        let resp = ui.add(egui::Slider::new(value, range).show_value(false).trailing_fill(true));
        // Left/Right arrows nudge natively while focused (egui Slider); the ring shows it.
        focus_ring(ui, &resp, 4.0);
        value_chip(ui, &fmt(*value));
        resp
    })
    .inner
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
    let resp = ui.add(btn);
    focus_ring(ui, &resp, 7.0);
    resp
}

/// Accent ring around a widget holding keyboard focus - the shared focus indicator for every
/// hand-painted primitive (egui outlines only its own `TextEdit`). `Sense::click` widgets are
/// Tab/arrow-focusable and Space/Enter-activatable for free; without this ring that focus is
/// invisible. `radius` matches the widget's corner radius. Reads memory focus directly (not
/// `Response::has_focus`, which is also gated on VIEWPORT focus): macOS keeps a control's
/// focus ring visible in inactive windows, and the screenshot harness window is never focused.
pub(crate) fn focus_ring(ui: &egui::Ui, resp: &egui::Response, radius: f32) {
    if ui.ctx().memory(|m| m.has_focus(resp.id)) {
        ui.painter().rect_stroke(
            resp.rect.expand(1.5),
            radius,
            egui::Stroke::new(1.5, colors::accent()),
            egui::StrokeKind::Outside,
        );
    }
}

/// Give a context menu / popup room to breathe (wider, roomier rows). Call at the top of every
/// menu closure AND its submenus so they stay consistent.
pub(crate) fn style_menu(ui: &mut egui::Ui) {
    ui.set_min_width(210.0);
    let s = ui.spacing_mut();
    s.button_padding = egui::vec2(12.0, 7.0);
    s.item_spacing.y = 3.0;
}

/// The standard context-menu row: label left, dim keyboard shortcut right (pass "" for none),
/// stretched to the menu width so shortcuts align. Use for every plain menu row (submenu
/// triggers stay `ui.menu_button`).
pub(crate) fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> egui::Response {
    let mut btn = egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 0.0));
    if !shortcut.is_empty() {
        btn = btn.shortcut_text(egui::RichText::new(shortcut).size(11.5).color(colors::dim()));
    }
    ui.add(btn)
}

/// A Tabby-style on/off switch: sliding knob on an accent-filled pill while on. The standard
/// boolean control for settings rows (reads better right-aligned than a checkbox).
pub(crate) fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(38.0, 22.0), egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let mix = |a: egui::Color32, b: egui::Color32| {
        let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
        egui::Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
    };
    let p = ui.painter();
    p.rect_filled(rect, 11.0, mix(colors::elevated(), colors::accent()));
    p.rect_stroke(
        rect,
        11.0,
        egui::Stroke::new(1.0, mix(colors::border(), colors::accent())),
        egui::StrokeKind::Inside,
    );
    let knob_x = rect.left() + 11.0 + t * (rect.width() - 22.0);
    p.circle_filled(egui::pos2(knob_x, rect.center().y), 7.5, mix(colors::dim(), colors::bg()));
    focus_ring(ui, &resp, 12.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small selectable chip (segmented option, e.g. cursor style): accent tint + ring while
/// selected, hover fill otherwise. Returns the Response.
pub(crate) fn chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let color = if selected { colors::accent() } else { colors::fg() };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let (rect, resp) =
        ui.allocate_exact_size(galley.size() + egui::vec2(24.0, 12.0), egui::Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 8.0, colors::selection());
        p.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, colors::accent()),
            egui::StrokeKind::Inside,
        );
    } else {
        if resp.hovered() {
            p.rect_filled(rect, 8.0, colors::hover());
        }
        p.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, colors::border()),
            egui::StrokeKind::Inside,
        );
    }
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    focus_ring(ui, &resp, 8.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tab-bar icon-button size (shared so the tab bar can right-align the gear by a spacer).
pub(crate) const ICON_BTN_W: f32 = 32.0;
const ICON_BTN_H: f32 = 30.0;
/// `icon_toggle` size (shared so the tab bar can right-align the gear by a spacer).
pub(crate) const ICON_TOGGLE_W: f32 = 28.0;

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
    focus_ring(ui, &resp, 6.0);
    resp.on_hover_text(tip)
}

/// A toggle variant of `icon_button`: tinted accent fill + accent glyph while `active`.
pub(crate) fn icon_toggle(
    ui: &mut egui::Ui,
    icon: &str,
    active: bool,
    tip: &str,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ICON_TOGGLE_W, 26.0), egui::Sense::click());
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
    focus_ring(ui, &resp, 6.0);
    resp.on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui::{Key, Modifiers};

    use super::{chip, num_field, parse_int_clamped, slider, step_int, text_field, toggle_switch};

    #[test]
    fn int_parse_clamps_and_rejects_junk() {
        assert_eq!(parse_int_clamped("25000", 100, 1_000_000), Some(25000));
        assert_eq!(parse_int_clamped("5", 100, 1_000_000), Some(100)); // below min
        assert_eq!(parse_int_clamped("99999999", 100, 1_000_000), Some(1_000_000)); // above max
        assert_eq!(parse_int_clamped("12a34", 100, 1_000_000), Some(1234)); // digits only
        assert_eq!(parse_int_clamped("", 100, 1_000_000), None);
        assert_eq!(parse_int_clamped("abc", 100, 1_000_000), None);
    }

    #[test]
    fn step_int_clamps_and_saturates() {
        assert_eq!(step_int(25_000, 1000, true, 100, 1_000_000), 26_000);
        assert_eq!(step_int(25_000, 1000, false, 100, 1_000_000), 24_000);
        assert_eq!(step_int(999_500, 1000, true, 100, 1_000_000), 1_000_000); // clamped high
        assert_eq!(step_int(500, 1000, false, 100, 1_000_000), 100); // clamped low
        assert_eq!(step_int(usize::MAX, 1000, true, 100, usize::MAX), usize::MAX); // saturates
        assert_eq!(step_int(50, 1000, false, 100, 1_000_000), 100); // below-min input snaps up
    }

    // ---- headless keyboard-a11y frames: the settings primitives must be operable without a
    // pointer (egui gives Tab/arrow focus + Space/Enter clicks; these pin that contract plus
    // our own arrow handling) ----

    /// One central-panel frame for widget-level keyboard tests. `modifiers` mirrors the raw
    /// modifier state (needed for Shift-stepping; key events alone don't set `i.modifiers`).
    fn widget_frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        modifiers: Modifiers,
        add: &mut dyn FnMut(&mut egui::Ui),
    ) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            events,
            modifiers,
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| add(ui));
        });
    }

    fn keyev(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn space_and_enter_toggle_a_focused_switch() {
        let ctx = egui::Context::default();
        let none = Modifiers::default();
        let mut on = false;
        widget_frame(&ctx, vec![], none, &mut |ui| {
            toggle_switch(ui, &mut on).request_focus();
        });
        widget_frame(&ctx, vec![keyev(Key::Space, none)], none, &mut |ui| {
            toggle_switch(ui, &mut on);
        });
        assert!(on, "Space on a focused switch must toggle it on");
        widget_frame(&ctx, vec![keyev(Key::Enter, none)], none, &mut |ui| {
            toggle_switch(ui, &mut on);
        });
        assert!(!on, "Enter must toggle it back off");
    }

    #[test]
    fn tab_moves_focus_from_a_text_field_to_the_next_primitive() {
        // The settings traversal contract: hand-painted primitives are real Tab stops after
        // a text field (their `Sense::click` is focusable), and traversal alone never acts.
        let ctx = egui::Context::default();
        let none = Modifiers::default();
        let mut q = String::new();
        let mut on = false;
        widget_frame(&ctx, vec![], none, &mut |ui| {
            text_field(ui, &mut q, "search", 120.0, crate::colors::fg()).request_focus();
            toggle_switch(ui, &mut on);
        });
        widget_frame(&ctx, vec![keyev(Key::Tab, none)], none, &mut |ui| {
            text_field(ui, &mut q, "search", 120.0, crate::colors::fg());
            toggle_switch(ui, &mut on);
        });
        let mut focused = false;
        widget_frame(&ctx, vec![], none, &mut |ui| {
            text_field(ui, &mut q, "search", 120.0, crate::colors::fg());
            focused = toggle_switch(ui, &mut on).has_focus();
        });
        assert!(focused, "Tab must move focus from the field to the switch");
        assert!(!on, "traversal alone must not toggle");
    }

    #[test]
    fn arrow_keys_move_focus_along_a_chip_row() {
        // Chip groups: egui's directional focus movement walks the row; Space/Enter then
        // selects (keyboard click). Pin it so a chip regression can't ship silently.
        let ctx = egui::Context::default();
        let none = Modifiers::default();
        let chips = |ui: &mut egui::Ui, out: &mut [bool; 2]| {
            ui.horizontal(|ui| {
                out[0] = chip(ui, "Fixed", true).has_focus();
                out[1] = chip(ui, "Dynamic", false).has_focus();
            });
        };
        let mut focus = [false; 2];
        widget_frame(&ctx, vec![], none, &mut |ui| {
            ui.horizontal(|ui| {
                chip(ui, "Fixed", true).request_focus();
                chip(ui, "Dynamic", false);
            });
        });
        widget_frame(&ctx, vec![keyev(Key::ArrowRight, none)], none, &mut |ui| {
            chips(ui, &mut focus);
        });
        widget_frame(&ctx, vec![], none, &mut |ui| chips(ui, &mut focus));
        assert_eq!(focus, [false, true], "ArrowRight must move focus to the next chip");
    }

    #[test]
    fn arrows_step_a_focused_num_field() {
        let ctx = egui::Context::default();
        let none = Modifiers::default();
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let mut v = 500_usize;
        widget_frame(&ctx, vec![], none, &mut |ui| {
            num_field(ui, "kb", &mut v, 0..=10_000, 100, 90.0).request_focus();
        });
        // Settle: the field installs its focus-lock filter on its first focused render.
        widget_frame(&ctx, vec![], none, &mut |ui| {
            num_field(ui, "kb", &mut v, 0..=10_000, 100, 90.0);
        });
        widget_frame(&ctx, vec![keyev(Key::ArrowUp, none)], none, &mut |ui| {
            num_field(ui, "kb", &mut v, 0..=10_000, 100, 90.0);
        });
        assert_eq!(v, 600, "ArrowUp steps by the field's step");
        widget_frame(&ctx, vec![keyev(Key::ArrowDown, shift)], shift, &mut |ui| {
            num_field(ui, "kb", &mut v, 0..=10_000, 100, 90.0);
        });
        assert_eq!(v, 0, "Shift+ArrowDown steps 10x (600 - 1000 clamps to the floor)");
    }

    #[test]
    fn arrow_keys_nudge_a_focused_slider() {
        // egui-native slider keyboard support - pinned so the design-system wrapper can't
        // accidentally drop it.
        let ctx = egui::Context::default();
        let none = Modifiers::default();
        let mut v = 0.5_f32;
        widget_frame(&ctx, vec![], none, &mut |ui| {
            slider(ui, &mut v, 0.0..=1.0, |x| format!("{x:.2}")).request_focus();
        });
        widget_frame(&ctx, vec![keyev(Key::ArrowRight, none)], none, &mut |ui| {
            slider(ui, &mut v, 0.0..=1.0, |x| format!("{x:.2}"));
        });
        assert!(v > 0.5, "ArrowRight must nudge the focused slider up");
    }
}
