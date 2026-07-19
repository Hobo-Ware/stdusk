//! Settings: a full-height, Tabby-style preferences view swapped into the central area while
//! `settings_open` (a real panel, not an `egui::Window` - windows don't render in the
//! screenshot harness). Left: section nav. Right: a roomy content pane; the Color scheme
//! section browses every embedded scheme with live palette previews and a fake-shell preview
//! card. Controls edit `self.cfg` in place (live-apply); nothing persists until Save writes
//! the TOML back to the config file.

use eframe::egui;

use crate::colors::{self, Theme};
use crate::ui::{self, icons};
use crate::{Stdusk, config, sync, terminal, themes};

/// Left nav width (outer, incl. margins).
const NAV_W: f32 = 184.0;
/// Content column cap - keeps rows readable on wide windows.
const CONTENT_MAX_W: f32 = 640.0;
/// Label column width inside setting grids, so controls line up across rows.
const LABEL_W: f32 = 330.0;
/// Scheme-list row height incl. spacing (uniform, for `show_rows` virtualization).
const SCHEME_ROW_H: f32 = 44.0;

/// Settings sections, in nav order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Appearance,
    ColorScheme,
    Terminal,
    Quake,
    Session,
    About,
}

impl Section {
    const ALL: [Self; 6] = [
        Self::Appearance,
        Self::ColorScheme,
        Self::Terminal,
        Self::Quake,
        Self::Session,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::ColorScheme => "Color scheme",
            Self::Terminal => "Terminal",
            Self::Quake => "Quake",
            Self::Session => "Session",
            Self::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Appearance => icons::PALETTE,
            Self::ColorScheme => icons::SWATCHES,
            Self::Terminal => icons::TERMINAL_WINDOW,
            Self::Quake => icons::LIGHTNING,
            Self::Session => icons::CLOCK_COUNTER_CLOCKWISE,
            Self::About => icons::INFO,
        }
    }
}

/// Settings-view state that outlives a frame (selected section, scheme search) plus the
/// one-frame hover handoff for the scheme preview card.
pub(crate) struct SettingsState {
    pub(crate) section: Section,
    filter: String,
    hover_preview: Option<Theme>, // scheme row hovered last frame; preview card follows it
    scroll_to_active: bool,       // scroll the scheme list to the active row once, on entry
    baseline: Option<config::Config>, // config as of open/save - the unsaved-changes reference
    confirm_close: bool,          // the "Unsaved changes" modal is showing
    dropdown_open: Option<egui::Id>, // which searchable theme dropdown is open, if any
    dropdown_filter: String,      // the open dropdown's search query
    dropdown_focus: bool,         // focus the dropdown filter once, on open
}

impl SettingsState {
    pub(crate) fn new() -> Self {
        Self {
            section: Section::Appearance,
            filter: String::new(),
            hover_preview: None,
            scroll_to_active: false,
            baseline: None,
            confirm_close: false,
            dropdown_open: None,
            dropdown_filter: String::new(),
            dropdown_focus: false,
        }
    }

    /// Switch section; entering the scheme browser jumps its list to the active scheme.
    pub(crate) fn open_section(&mut self, section: Section) {
        self.section = section;
        self.scroll_to_active = section == Section::ColorScheme;
        self.dropdown_open = None;
    }
}

// ---- pure helpers (unit-tested below) ----

/// Color role of a preview segment, resolved against a `Theme` by `ink_color`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ink {
    Fg,
    Dim,
    Ansi(usize),
}

/// One colored run of the fake-shell preview.
pub(crate) struct Seg {
    pub(crate) text: &'static str,
    pub(crate) ink: Ink,
    pub(crate) bg: Option<Ink>,
}

const fn seg(text: &'static str, ink: Ink) -> Seg {
    Seg { text, ink, bg: None }
}

/// Fake shell output for the scheme preview card: a prompt line plus ls-style rows that
/// exercise red/green/yellow/blue/magenta/cyan and the default fg on the scheme's bg.
pub(crate) const PREVIEW_LINES: &[&[Seg]] = &[
    &[
        seg("user", Ink::Ansi(2)),
        seg("@", Ink::Ansi(6)),
        seg("stdusk", Ink::Ansi(4)),
        seg(" ~", Ink::Ansi(5)),
        seg(" $", Ink::Ansi(1)),
        seg(" ls -la", Ink::Fg),
    ],
    &[seg("drwxr-xr-x  1 user", Ink::Dim), seg("  Documents", Ink::Ansi(3))],
    &[
        seg("drwxr-xr-x  1 user", Ink::Dim),
        seg("  ", Ink::Fg),
        Seg { text: " Downloads ", ink: Ink::Ansi(0), bg: Some(Ink::Ansi(2)) },
    ],
    &[seg("drwxr-xr-x  1 user", Ink::Dim), seg("  Music", Ink::Ansi(5))],
    &[
        seg("lrwxr-xr-x  1 user", Ink::Dim),
        seg("  sym", Ink::Ansi(6)),
        seg(" -> ", Ink::Fg),
        seg("link", Ink::Ansi(1)),
    ],
];

/// Resolve an `Ink` role against a theme.
pub(crate) fn ink_color(t: &Theme, ink: Ink) -> egui::Color32 {
    match ink {
        Ink::Fg => t.fg,
        Ink::Dim => t.ansi[8],
        Ink::Ansi(i) => t.ansi[i.min(15)],
    }
}

/// Normalize a theme name the way the scheme pack does (lowercase, spaces/underscores -> '-').
pub(crate) fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase().replace([' ', '_'], "-")
}

/// Indices of schemes whose name contains `query` (case-insensitive); all when empty.
pub(crate) fn filter_schemes(all: &[(String, Theme)], query: &str) -> Vec<usize> {
    let q = query.trim().to_ascii_lowercase();
    all.iter()
        .enumerate()
        .filter(|(_, (name, _))| q.is_empty() || name.contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// Indices of names containing `query` (case-insensitive); all when empty. Unlike scheme
/// names, font family names are mixed-case, so both sides are lowercased.
pub(crate) fn filter_names(all: &[String], query: &str) -> Vec<usize> {
    let q = query.trim().to_ascii_lowercase();
    all.iter()
        .enumerate()
        .filter(|(_, name)| q.is_empty() || name.to_ascii_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// Which config field a picked scheme lands in. With follow-system on, the pick applies to
/// the theme slot of the CURRENT OS appearance (the fixed `theme` is ignored in that mode).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SchemeSlot {
    Fixed,
    Light,
    Dark,
}

pub(crate) fn scheme_slot(follow_system: bool, system_light: bool) -> SchemeSlot {
    if !follow_system {
        SchemeSlot::Fixed
    } else if system_light {
        SchemeSlot::Light
    } else {
        SchemeSlot::Dark
    }
}

/// The theme name that resolves as active for this appearance config (mirrors the per-frame
/// reconcile in main.rs).
pub(crate) fn resolved_theme_name(a: &config::Appearance, system_light: bool) -> String {
    match scheme_slot(a.follow_system, system_light) {
        SchemeSlot::Fixed => a.theme.clone(),
        SchemeSlot::Light => a.theme_light.clone(),
        SchemeSlot::Dark => a.theme_dark.clone(),
    }
}

// ---- content-pane building blocks ----

/// Section heading.
fn title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(text).size(20.0).strong().color(colors::fg()));
    ui.add_space(14.0);
}

/// Dim uppercase group label (Behavior / Paste / ...).
fn subheading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(18.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(11.0).strong().color(colors::dim()));
    ui.add_space(6.0);
}

/// A group of aligned two-column rows. NOT an `egui::Grid`: Grid requests a sizing discard on
/// its first pass, which lands eframe's pass-2 screenshot capture on a never-painted buffer
/// (blank PNG); a fixed label column gives the same alignment without the discard.
fn rows(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 12.0;
        add(ui);
    });
}

/// One settings row: title (+ optional dim description + optional italic hint) left, control
/// right. Call inside `rows`.
fn row_full(
    ui: &mut egui::Ui,
    name: &str,
    desc: &str,
    hint: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LABEL_W);
            ui.label(egui::RichText::new(name).size(14.0).color(colors::fg()));
            if !desc.is_empty() {
                ui.label(egui::RichText::new(desc).size(11.0).color(colors::dim()));
            }
            if !hint.is_empty() {
                ui.label(egui::RichText::new(hint).size(10.5).italics().color(colors::dim()));
            }
        });
        ui.add_space(16.0);
        control(ui);
    });
}

/// One settings row: title (+ optional dim description) left, control right. Call inside `rows`.
fn row(ui: &mut egui::Ui, name: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    row_full(ui, name, desc, "", control);
}

/// A `row` whose setting only takes effect for terminals spawned after the change.
fn row_new_tabs(ui: &mut egui::Ui, name: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    row_full(ui, name, desc, "Applies to new tabs", control);
}

/// A fraction slider displayed as a live percentage chip ("85%"). Returns true while changed.
fn pct_slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    ui::slider(ui, value, range, |v| format!("{:.0}%", v * 100.0)).changed()
}

/// A link-style row (About section): icon + title + dim subtitle, whole row clickable.
fn link_row(ui: &mut egui::Ui, icon: &str, name: &str, desc: &str) -> egui::Response {
    let w = ui.available_width().min(CONTENT_MAX_W);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 46.0), egui::Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 9.0, colors::hover());
    }
    p.rect_stroke(rect, 9.0, egui::Stroke::new(1.0, colors::border()), egui::StrokeKind::Inside);
    p.text(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(16.0),
        colors::accent(),
    );
    p.text(
        egui::pos2(rect.left() + 44.0, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.5),
        colors::fg(),
    );
    p.text(
        egui::pos2(rect.left() + 44.0, rect.center().y + 8.0),
        egui::Align2::LEFT_CENTER,
        desc,
        egui::FontId::proportional(11.0),
        colors::dim(),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One nav-sidebar row: Phosphor icon + label; selection fill + accent text when active.
fn nav_row(ui: &mut egui::Ui, section: Section, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 8.0, colors::selection());
    } else if resp.hovered() {
        p.rect_filled(rect, 8.0, colors::hover());
    }
    let icon_col = if selected { colors::accent() } else { colors::dim() };
    let text_col = if selected {
        colors::accent()
    } else if resp.hovered() {
        colors::fg()
    } else {
        colors::fg().gamma_multiply(0.82)
    };
    p.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        section.icon(),
        egui::FontId::proportional(15.0),
        icon_col,
    );
    p.text(
        egui::pos2(rect.left() + 40.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        section.label(),
        egui::FontId::proportional(14.0),
        text_col,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---- the fake-shell preview card (shared by Appearance and the scheme browser) ----

/// What the preview card reflects beyond the theme - built from the live config so every
/// appearance-affecting setting shows up in ONE place.
pub(crate) struct PreviewOpts {
    pub(crate) font_size: f32,
    pub(crate) cursor: ui::CursorStyle,
    pub(crate) blink: bool,
    pub(crate) ligatures: bool,
}

/// Preview options straight from the live config.
fn preview_opts(cfg: &config::Config) -> PreviewOpts {
    PreviewOpts {
        font_size: cfg.appearance.font_size,
        cursor: ui::cursor_style(&cfg.terminal.cursor),
        blink: cfg.terminal.cursor_blink,
        ligatures: cfg.terminal.ligatures,
    }
}

/// The fake-shell preview card: `PREVIEW_LINES` painted on the scheme's own bg at the
/// configured font size, with the configured cursor (blinking on egui's clock when enabled)
/// after the prompt and symbol ligatures applied when on.
fn preview_card(ui: &mut egui::Ui, theme: &Theme, opts: &PreviewOpts) {
    let width = ui.available_width().min(CONTENT_MAX_W);
    let font = egui::FontId::monospace(opts.font_size);
    let line_h = (opts.font_size * 1.45).round();
    let pad = egui::vec2(16.0, 13.0);
    let height = PREVIEW_LINES.len() as f32 * line_h + pad.y * 2.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 10.0, theme.bg);
    painter.rect_stroke(
        rect,
        10.0,
        egui::Stroke::new(1.0, colors::border()),
        egui::StrokeKind::Inside,
    );
    for (li, line) in PREVIEW_LINES.iter().enumerate() {
        let mut x = rect.left() + pad.x;
        let y = rect.top() + pad.y + li as f32 * line_h;
        for run in *line {
            let color = ink_color(theme, run.ink);
            let text =
                if opts.ligatures { ui::apply_ligatures(run.text) } else { run.text.to_owned() };
            let galley = painter.layout_no_wrap(text, font.clone(), color);
            if let Some(bg) = run.bg {
                let bg_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y - 1.0),
                    galley.size() + egui::vec2(0.0, 2.0),
                );
                painter.rect_filled(bg_rect, 3.0, ink_color(theme, bg));
            }
            let run_w = galley.size().x;
            painter.galley(egui::pos2(x, y), galley, color);
            x += run_w;
        }
        if li == 0 {
            // The configured cursor right after the typed command. Blink rides egui's clock
            // (never std::time); keep repainting while visible so it ticks.
            if opts.blink {
                let time = ui.input(|i| i.time);
                let next_flip = 0.53 - time.rem_euclid(0.53);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(next_flip.max(0.01)));
                if !ui::blink_on(time) {
                    continue;
                }
            }
            let (cw, ch) = (opts.font_size * 0.55, opts.font_size * 1.15);
            let cpos = egui::pos2(x + 2.0, y);
            match opts.cursor {
                ui::CursorStyle::Beam => {
                    painter.rect_filled(
                        egui::Rect::from_min_size(cpos, egui::vec2(2.0, ch)),
                        0.0,
                        theme.cursor,
                    );
                }
                ui::CursorStyle::Underline => {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(cpos.x, cpos.y + ch - 2.0),
                            egui::vec2(cw, 2.0),
                        ),
                        0.0,
                        theme.cursor,
                    );
                }
                ui::CursorStyle::Block => {
                    painter.rect_filled(
                        egui::Rect::from_min_size(cpos, egui::vec2(cw, ch)),
                        1.0,
                        theme.cursor,
                    );
                }
            }
        }
    }
}

// ---- color-scheme section drawing ----

/// One scheme row: name + a strip of the 16 ANSI swatches, drawn on the scheme's own bg so
/// every row is its own live preview. Accent border marks the active scheme.
fn scheme_row(ui: &mut egui::Ui, name: &str, t: &Theme, active: bool) -> egui::Response {
    let w = ui.available_width().min(CONTENT_MAX_W);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(w, SCHEME_ROW_H - 6.0), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 9.0, t.bg);
    let stroke = if active {
        egui::Stroke::new(2.0, colors::accent())
    } else if resp.hovered() {
        egui::Stroke::new(1.0, colors::dim())
    } else {
        egui::Stroke::new(1.0, colors::border())
    };
    p.rect_stroke(rect, 9.0, stroke, egui::StrokeKind::Inside);
    let mut x = rect.left() + 14.0;
    if active {
        p.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icons::CHECK,
            egui::FontId::proportional(13.0),
            colors::accent(),
        );
        x += 20.0;
    }
    p.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        t.fg,
    );
    // 16 ANSI swatches, right-aligned.
    let (sw, gap) = (11.0, 3.0);
    let total = 16.0 * sw + 15.0 * gap;
    let mut sx = rect.right() - 14.0 - total;
    for c in t.ansi {
        p.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(sx + sw / 2.0, rect.center().y),
                egui::vec2(sw, sw),
            ),
            3.0,
            c,
        );
        sx += sw + gap;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---- searchable dropdowns (theme slots + font family) ----

/// Combo-style button + searchable-overlay scaffold shared by the scheme and font dropdowns:
/// paints the closed button (current `label` + caret), owns the open/filter/focus-once state
/// (`st.dropdown_*`, one open at a time), and dismisses on pick, Esc, or an outside press.
/// `list` draws the filtered rows inside the popup and returns true when an item was picked.
fn searchable_dropdown(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    id_salt: &str,
    label: &str,
    hint: &str,
    list: impl FnOnce(&mut egui::Ui, &mut SettingsState) -> bool,
) -> bool {
    let id = ui.make_persistent_id(id_salt);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(200.0, 28.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = st.dropdown_open == Some(id);
    let p = ui.painter();
    p.rect_filled(rect, 7.0, colors::bg());
    let stroke_col = if open || resp.hovered() { colors::accent() } else { colors::border() };
    p.rect_stroke(rect, 7.0, egui::Stroke::new(1.0, stroke_col), egui::StrokeKind::Inside);
    p.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        colors::fg(),
    );
    p.text(
        egui::pos2(rect.right() - 12.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        if open { ui::icons::CARET_UP } else { ui::icons::CARET_DOWN },
        egui::FontId::proportional(12.0),
        colors::dim(),
    );
    if resp.clicked() {
        st.dropdown_open = if open { None } else { Some(id) };
        st.dropdown_filter.clear();
        st.dropdown_focus = true;
    }
    if st.dropdown_open != Some(id) {
        return false;
    }

    let mut picked = false;
    let area = egui::Area::new(id.with("popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_bottom() + egui::vec2(0.0, 4.0))
        .show(ui.ctx(), |ui| {
            ui::overlay_frame().show(ui, |ui| {
                ui.set_width(280.0);
                let r = ui::text_field(ui, &mut st.dropdown_filter, hint, 264.0, colors::fg());
                if st.dropdown_focus {
                    r.request_focus();
                    st.dropdown_focus = false;
                }
                ui.add_space(6.0);
                picked = list(ui, st);
            });
        });
    // Close on pick, Esc, or a press outside both the popup and its button.
    let dismissed = ui.ctx().input(|i| {
        i.key_pressed(egui::Key::Escape)
            || (i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p) && !rect.contains(p)))
    });
    if picked || dismissed {
        st.dropdown_open = None;
    }
    picked
}

/// One row of a searchable-dropdown popup: selection fill + accent text when active, hover
/// fill otherwise.
fn dropdown_row(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 6.0, colors::selection());
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, colors::hover());
    }
    p.text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(13.0),
        if active { colors::accent() } else { colors::fg() },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A searchable scheme picker (all three theme slots). Hovering a row hands the scheme to
/// `st.hover_preview` so the section's preview card follows it. Returns true when a scheme
/// was picked into `value`.
fn scheme_dropdown(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    id_salt: &str,
    value: &mut String,
) -> bool {
    let all = themes::all_schemes();
    let label = value.clone();
    let hint = format!("Search {} schemes", all.len());
    searchable_dropdown(ui, st, id_salt, &label, &hint, |ui, st| {
        let shown = filter_schemes(all, &st.dropdown_filter);
        if shown.is_empty() {
            ui.label(egui::RichText::new("No schemes match.").color(colors::dim()));
            return false;
        }
        let mut picked = false;
        egui::ScrollArea::vertical().max_height(230.0).show_rows(
            ui,
            24.0,
            shown.len(),
            |ui, range| {
                for i in range {
                    let (name, theme) = &all[shown[i]];
                    let name = name.as_str();
                    let resp = dropdown_row(ui, name, name == normalize_name(value));
                    if resp.hovered() {
                        // Live-preview the hovered scheme in the section's preview card
                        // (same hover handoff as the scheme browser rows).
                        st.hover_preview = Some(*theme);
                    }
                    if resp.clicked() {
                        name.clone_into(value);
                        picked = true;
                    }
                }
            },
        );
        picked
    })
}

/// A searchable picker over the installed font families, with a leading "Default (bundled)"
/// reset row while unfiltered. Returns true when a family was picked into `value` ("" =
/// bundled default).
fn font_dropdown(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    id_salt: &str,
    value: &mut String,
) -> bool {
    const DEFAULT_LABEL: &str = "Default (bundled)";
    let all = crate::installed_families();
    let label = if value.is_empty() { DEFAULT_LABEL.to_owned() } else { value.clone() };
    let hint = format!("Search {} font families", all.len());
    searchable_dropdown(ui, st, id_salt, &label, &hint, |ui, st| {
        let shown = filter_names(all, &st.dropdown_filter);
        let with_default = st.dropdown_filter.trim().is_empty();
        if shown.is_empty() && !with_default {
            ui.label(egui::RichText::new("No fonts match.").color(colors::dim()));
            return false;
        }
        let mut picked = false;
        let total = shown.len() + usize::from(with_default);
        egui::ScrollArea::vertical().max_height(230.0).show_rows(ui, 24.0, total, |ui, range| {
            for i in range {
                let name = if with_default && i == 0 {
                    ""
                } else {
                    all[shown[i - usize::from(with_default)]].as_str()
                };
                let text = if name.is_empty() { DEFAULT_LABEL } else { name };
                if dropdown_row(ui, text, name == value).clicked() {
                    name.clone_into(value);
                    picked = true;
                }
            }
        });
        picked
    })
}

// ---- plain sections (pure config edits) ----

/// Side effects the Appearance section needs applied by the caller (which has `&mut Stdusk`).
struct AppearanceFx {
    font_commit: bool, // the font field committed / a family was picked - re-apply fonts live
}

/// Appearance aggregates EVERYTHING that changes how the terminal looks (theme, opacity,
/// font, cursor, ligatures) so the single live preview card at the top reflects each control
/// below it. The card also follows a scheme row hovered in an open theme dropdown.
fn appearance_section(
    ui: &mut egui::Ui,
    cfg: &mut config::Config,
    st: &mut SettingsState,
) -> AppearanceFx {
    title(ui, "Appearance");
    let mut fx = AppearanceFx { font_commit: false };

    // One live preview for the whole section: resolved theme + font size + cursor + ligatures,
    // overridden by the dropdown row hovered last frame (same handoff as the scheme browser).
    let system_light = matches!(ui.ctx().input(|i| i.raw.system_theme), Some(egui::Theme::Light));
    let resolved = resolved_theme_name(&cfg.appearance, system_light);
    let theme = st.hover_preview.unwrap_or_else(|| colors::by_name(&resolved));
    preview_card(ui, &theme, &preview_opts(cfg));
    st.hover_preview = None;
    ui.add_space(14.0);

    subheading(ui, "Theme");
    let a = &mut cfg.appearance;
    rows(ui, |ui| {
        row(ui, "Follow system appearance", "Switch themes with the macOS light/dark mode", |ui| {
            ui::toggle_switch(ui, &mut a.follow_system);
        });
        if a.follow_system {
            row(ui, "Light theme", "Applied while macOS is light", |ui| {
                scheme_dropdown(ui, st, "theme_light", &mut a.theme_light);
            });
            row(ui, "Dark theme", "Applied while macOS is dark", |ui| {
                scheme_dropdown(ui, st, "theme_dark", &mut a.theme_dark);
            });
        } else {
            row(ui, "Theme", "Also browsable in the Color scheme section", |ui| {
                scheme_dropdown(ui, st, "theme_fixed", &mut a.theme);
            });
        }
    });

    subheading(ui, "Window");
    let q = &mut cfg.quake;
    rows(ui, |ui| {
        row(ui, "Opacity", "Window background transparency", |ui| {
            pct_slider(ui, &mut a.opacity, 0.4..=1.0);
        });
        row_full(
            ui,
            "Unfocused opacity",
            "Dim the window while another app has focus",
            "No effect while Hide on focus loss is on",
            |ui| {
                pct_slider(ui, &mut q.unfocused_opacity, 0.2..=1.0);
            },
        );
        row(ui, "Tab width", "Fixed keeps every tab the same size", |ui| {
            ui.horizontal(|ui| {
                for (label, value) in [("Fixed", "fixed"), ("Dynamic", "dynamic")] {
                    let selected = ui::tab_width_mode(&a.tab_width) == ui::tab_width_mode(value);
                    if ui::chip(ui, label, selected).clicked() {
                        a.tab_width = value.into();
                    }
                }
            });
        });
    });

    subheading(ui, "Text");
    let t = &mut cfg.terminal;
    rows(ui, |ui| {
        row_full(
            ui,
            "Font",
            "Terminal font family - Nerd Fonts supported",
            "Empty uses the bundled default; applies when the field loses focus",
            |ui| {
                let r = ui::text_field(
                    ui,
                    &mut a.font,
                    "e.g. JetBrainsMono Nerd Font",
                    200.0,
                    colors::fg(),
                );
                if r.lost_focus() {
                    fx.font_commit = true;
                }
            },
        );
        row(ui, "Installed fonts", "Browse the system's font families", |ui| {
            if font_dropdown(ui, st, "font_family", &mut a.font) {
                fx.font_commit = true;
            }
        });
        row(ui, "Font size", "", |ui| {
            ui::slider(ui, &mut a.font_size, 9.0..=24.0, |v| format!("{v:.0} pt"));
        });
        row(ui, "Line padding", "Extra pixels added to each line's height", |ui| {
            ui::slider(ui, &mut a.line_padding, 0.0..=8.0, |v| format!("{v:.0} px"));
        });
        row_full(
            ui,
            "Minimum contrast",
            "Nudge text toward black/white until it meets a WCAG contrast ratio",
            "1 = off (the default); Tabby ships 4; 4.5 = WCAG AA",
            |ui| {
                ui::slider(ui, &mut t.minimum_contrast, 1.0..=21.0, |v| format!("{v:.1}"));
            },
        );
        row(ui, "Ligatures", "Draw common code sequences as single glyphs", |ui| {
            ui::toggle_switch(ui, &mut t.ligatures);
            ui.add_space(10.0);
            lig_preview(ui, t.ligatures);
        });
        row_new_tabs(
            ui,
            "Bold in bright colors",
            "Draw bold text with the bright ANSI palette",
            |ui| {
                ui::toggle_switch(ui, &mut t.bold_bright);
            },
        );
    });

    subheading(ui, "Cursor");
    rows(ui, |ui| {
        row(ui, "Cursor style", "Previewed live in the card above", |ui| {
            ui.horizontal(|ui| {
                for (label, value) in
                    [("Block", "block"), ("Underline", "underline"), ("Beam", "beam")]
                {
                    let selected = ui::cursor_style(&t.cursor) == ui::cursor_style(value);
                    if ui::chip(ui, label, selected).clicked() {
                        t.cursor = value.into();
                    }
                }
            });
        });
        row(ui, "Blink the cursor", "", |ui| {
            ui::toggle_switch(ui, &mut t.cursor_blink);
        });
    });
    fx
}

/// Tiny live before/after for the Ligatures row: the raw sequences while off, the single
/// glyphs while on - flips with the toggle next to it.
fn lig_preview(ui: &mut egui::Ui, on: bool) {
    const SAMPLE: &str = "-> => != >= <=";
    let (text, color) = if on {
        (ui::apply_ligatures(SAMPLE), colors::accent())
    } else {
        (SAMPLE.into(), colors::dim())
    };
    ui.label(egui::RichText::new(text).monospace().size(13.0).color(color));
}

fn terminal_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    title(ui, "Terminal");
    let t = &mut cfg.terminal;

    subheading(ui, "Behavior");
    rows(ui, |ui| {
        row_new_tabs(ui, "Scrollback lines", "History kept per pane", |ui| {
            ui::num_field(ui, "scrollback", &mut t.scrollback_lines, 100..=1_000_000, 1000, 110.0);
        });
        row_new_tabs(
            ui,
            "Shell integration",
            "OSC 133 command marks for done/failed state",
            |ui| {
                ui::toggle_switch(ui, &mut t.shell_integration);
            },
        );
        row_new_tabs(ui, "Detect progress", "Track % output as a tab progress bar", |ui| {
            ui::toggle_switch(ui, &mut t.detect_progress);
        });
        row_new_tabs(ui, "Word separators", "Characters that end a double-click selection", |ui| {
            ui::text_field(ui, &mut t.word_separators, "separators", 180.0, colors::fg());
        });
        row(ui, "On shell exit", "Close the pane, keep it with a banner, or respawn", |ui| {
            ui.horizontal(|ui| {
                for (label, value) in [("Close", "close"), ("Keep", "keep"), ("Restart", "restart")]
                {
                    let selected =
                        terminal::on_exit_mode(&t.on_exit) == terminal::on_exit_mode(value);
                    if ui::chip(ui, label, selected).clicked() {
                        t.on_exit = value.into();
                    }
                }
            });
        });
        row(ui, "Dynamic tab title", "Follow the title the shell sets (OSC 0/2)", |ui| {
            ui::toggle_switch(ui, &mut t.dynamic_title);
        });
        row(ui, "AI CLI badges", "Badge tabs running a known AI CLI", |ui| {
            ui::toggle_switch(ui, &mut t.detect_clis);
        });
        row(ui, "Notify when done", "Long command finishes while hidden", |ui| {
            ui::toggle_switch(ui, &mut t.notify_on_done);
        });
        row(ui, "Option sends Meta", "ESC-prefixed keys instead of composed chars", |ui| {
            ui::toggle_switch(ui, &mut t.alt_is_meta);
        });
        row(
            ui,
            "Confirm closing busy tabs",
            "Ask before closing a tab with a running process",
            |ui| {
                ui::toggle_switch(ui, &mut t.warn_on_close_running);
            },
        );
    });

    subheading(ui, "Paste");
    rows(ui, |ui| {
        row(ui, "Warn on multiline paste", "Confirm before pasting multiple lines", |ui| {
            ui::toggle_switch(ui, &mut t.warn_on_multiline_paste);
        });
        row(ui, "Trim whitespace", "Strip leading/trailing whitespace from pastes", |ui| {
            ui::toggle_switch(ui, &mut t.trim_whitespace_on_paste);
        });
        row(ui, "Newlines to spaces", "Collapse pasted newlines into single spaces", |ui| {
            ui::toggle_switch(ui, &mut t.replace_newlines_on_paste);
        });
    });

    subheading(ui, "Mouse");
    rows(ui, |ui| {
        row(ui, "Clickable links", "Open URLs and file paths on click", |ui| {
            ui::toggle_switch(ui, &mut t.clickable_links);
        });
        row(ui, "Copy on select", "Clipboard updates when a selection finishes", |ui| {
            ui::toggle_switch(ui, &mut t.copy_on_select);
        });
        row(ui, "Paste on middle click", "", |ui| {
            ui::toggle_switch(ui, &mut t.paste_on_middle_click);
        });
    });

    // (Cursor / ligatures / bold-bright / opacity live in Appearance - one preview there
    // covers them all. Terminal is behavior only.)
}

/// Side effects the Quake section needs applied by the caller (which has `&mut Stdusk`).
struct QuakeFx {
    hotkey_commit: bool,  // the hotkey field lost focus - re-register if it changed
    height_changed: bool, // the height slider moved - re-apply the window size live
}

fn quake_section(ui: &mut egui::Ui, cfg: &mut config::Config) -> QuakeFx {
    title(ui, "Quake");
    let q = &mut cfg.quake;
    let mut fx = QuakeFx { hotkey_commit: false, height_changed: false };

    subheading(ui, "Window");
    rows(ui, |ui| {
        row(ui, "Global hotkey", "Applies when the field loses focus", |ui| {
            let r = ui::text_field(ui, &mut q.hotkey, "e.g. Ctrl+Grave, F13", 180.0, colors::fg());
            if r.lost_focus() {
                fx.hotkey_commit = true;
            }
        });
        row(ui, "Window height", "Fraction of the screen the window drops down", |ui| {
            fx.height_changed = pct_slider(ui, &mut q.height_pct, 0.2..=0.9);
        });
    });

    subheading(ui, "Focus & Dock");
    rows(ui, |ui| {
        row(ui, "Hide on focus loss", "Hide when another app takes focus", |ui| {
            ui::toggle_switch(ui, &mut q.hide_on_focus_loss);
        });
        row(ui, "Hide from Dock", "Run as an accessory app (no Dock icon)", |ui| {
            ui::toggle_switch(ui, &mut q.hide_from_dock);
        });
        row(ui, "Menu-bar icon", "Status item with Show/Hide and Quit", |ui| {
            ui::toggle_switch(ui, &mut q.menu_bar_icon);
        });
        row(ui, "Dock icon while visible", "Show the Dock icon only while shown", |ui| {
            ui::toggle_switch(ui, &mut q.dock_when_visible);
        });
    });
    fx
}

/// Session + settings-sync. Returns the sync operation to start, if a button was clicked.
fn session_section(ui: &mut egui::Ui, cfg: &mut config::Config, busy: bool) -> Option<sync::Op> {
    title(ui, "Session");
    rows(ui, |ui| {
        row(
            ui,
            "Restore session",
            "Reopen last session's tabs (cwd, title, color) on launch",
            |ui| {
                ui::toggle_switch(ui, &mut cfg.session.restore);
            },
        );
    });

    subheading(ui, "Sync");
    let mut op = None;
    rows(ui, |ui| {
        row_full(
            ui,
            "Sync repo",
            "Git remote for config.toml + custom schemes",
            "Keep it private - e.g. gh repo create stdusk-settings --private",
            |ui| {
                ui::text_field(
                    ui,
                    &mut cfg.sync.repo,
                    "git@github.com:you/stdusk-settings.git",
                    300.0,
                    colors::fg(),
                );
            },
        );
        row(ui, "Sync now", "Uses your own git credentials (SSH key / helper)", |ui| {
            ui.add_enabled_ui(!busy && !cfg.sync.repo.trim().is_empty(), |ui| {
                ui.horizontal(|ui| {
                    if ui::action_button(ui, "Push", true).clicked() {
                        op = Some(sync::Op::Push);
                    }
                    if ui::action_button(ui, "Pull", false)
                        .on_hover_text("Overwrites local settings with the repo's")
                        .clicked()
                    {
                        op = Some(sync::Op::Pull);
                    }
                    if busy {
                        ui.label(egui::RichText::new("syncing…").color(colors::dim()));
                    }
                });
            });
        });
    });
    op
}

fn about_section(ui: &mut egui::Ui) {
    title(ui, "About");
    ui.label(egui::RichText::new("stdusk").size(26.0).strong().color(colors::fg()));
    ui.label(
        egui::RichText::new(concat!("version ", env!("CARGO_PKG_VERSION")))
            .size(12.0)
            .color(colors::dim()),
    );
    ui.add_space(4.0);
    ui.label(egui::RichText::new("the machine speaks back").italics().color(colors::dim()));
    ui.add_space(20.0);
    if link_row(ui, icons::ARROW_SQUARE_OUT, "Open config file", "~/.config/stdusk/config.toml")
        .clicked()
        && let Some(p) = config::ensure_and_path()
    {
        let _ = std::process::Command::new("open").arg(p).spawn();
    }
    ui.add_space(8.0);
    if link_row(ui, icons::FOLDER, "Open config folder", "Custom schemes live in schemes/")
        .clicked()
        && let Some(p) = config::ensure_and_path()
        && let Some(dir) = p.parent()
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
}

// ---- the view ----

impl Stdusk {
    /// Open the settings view: snapshot the config as the unsaved-changes baseline and dismiss
    /// the find bar (the workspace it searches is being swapped out).
    pub(crate) fn open_settings(&mut self) {
        if self.search.take().is_some() {
            self.tabs[self.active].focused_term().clear_selection();
        }
        self.settings.baseline = Some(self.cfg.clone());
        self.settings.confirm_close = false;
        self.settings_open = true;
    }

    /// Close the settings view - or, with unsaved changes, show the confirm modal instead.
    pub(crate) fn request_close_settings(&mut self) {
        let dirty =
            self.settings.baseline.as_ref().is_some_and(|b| config::config_dirty(b, &self.cfg));
        if dirty {
            self.settings.confirm_close = true;
        } else {
            self.settings_open = false;
        }
    }

    /// Gear / Cmd+, toggle.
    pub(crate) fn toggle_settings(&mut self) {
        if self.settings_open {
            self.request_close_settings();
        } else {
            self.open_settings();
        }
    }

    /// Apply a scheme picked in the browser: recolor live and record it in the config slot
    /// the current mode resolves from (so the per-frame reconcile doesn't fight the pick).
    fn apply_scheme(&mut self, ctx: &egui::Context, name: &str, theme: Theme) {
        colors::set(theme);
        ui::apply_theme(ctx);
        let system_light = matches!(ctx.input(|i| i.raw.system_theme), Some(egui::Theme::Light));
        let a = &mut self.cfg.appearance;
        match scheme_slot(a.follow_system, system_light) {
            SchemeSlot::Fixed => name.clone_into(&mut a.theme),
            SchemeSlot::Light => name.clone_into(&mut a.theme_light),
            SchemeSlot::Dark => name.clone_into(&mut a.theme_dark),
        }
        name.clone_into(&mut self.theme_name);
        let now = ctx.input(|i| i.time);
        self.toast = Some((format!("Theme: {name}"), now + 1.4));
    }

    /// The crown-jewel section: search + fake-shell preview card + the full scheme list with
    /// per-row 16-color palettes. Hovering a row previews it; clicking applies it live.
    fn scheme_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        title(ui, "Color scheme");
        let all = themes::all_schemes();
        let system_light = matches!(ctx.input(|i| i.raw.system_theme), Some(egui::Theme::Light));
        if self.cfg.appearance.follow_system {
            let slot = if system_light { "light" } else { "dark" };
            ui.label(
                egui::RichText::new(format!(
                    "Following the system appearance - picking a scheme sets your {slot} theme."
                ))
                .size(11.5)
                .color(colors::dim()),
            );
            ui.add_space(8.0);
        }

        // Preview card follows the row hovered last frame, else the active scheme.
        let active = normalize_name(&self.theme_name);
        let active_theme = all
            .iter()
            .find(|(n, _)| *n == active)
            .map_or_else(|| colors::by_name(&self.theme_name), |(_, t)| *t);
        preview_card(
            ui,
            &self.settings.hover_preview.unwrap_or(active_theme),
            &preview_opts(&self.cfg),
        );
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(icons::MAGNIFYING_GLASS).size(15.0).color(colors::dim()));
            ui::text_field(
                ui,
                &mut self.settings.filter,
                &format!("Search {} color schemes", all.len()),
                260.0,
                colors::fg(),
            );
        });
        ui.add_space(10.0);

        // Uniform-height rows through show_rows so 195 palette strips scroll smoothly.
        self.settings.hover_preview = None;
        let shown = filter_schemes(all, &self.settings.filter);
        if shown.is_empty() {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("No schemes match your search.").color(colors::dim()));
            return;
        }
        let mut picked: Option<(String, Theme)> = None;
        ui.spacing_mut().item_spacing.y = 6.0;
        let mut list = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.settings.scroll_to_active {
            self.settings.scroll_to_active = false;
            if let Some(pos) = shown.iter().position(|&i| all[i].0 == active) {
                // Row-aligned offset (2 rows of context above). A mid-row offset leaves the
                // clipped row above painting artifacts at the viewport's top edge.
                list = list.vertical_scroll_offset(pos.saturating_sub(2) as f32 * SCHEME_ROW_H);
            }
        }
        list.show_rows(ui, SCHEME_ROW_H - 6.0, shown.len(), |ui, range| {
            for i in range {
                let (name, theme) = &all[shown[i]];
                let resp = scheme_row(ui, name, theme, *name == active);
                if resp.hovered() {
                    self.settings.hover_preview = Some(*theme);
                }
                if resp.clicked() {
                    picked = Some((name.clone(), *theme));
                }
            }
        });
        if let Some((name, theme)) = picked {
            self.apply_scheme(ctx, &name, theme);
        }
    }

    /// Write the config file, re-baseline the unsaved-changes guard, and re-register the
    /// hotkey if it changed. Shared by the footer Save and the unsaved-changes modal.
    fn save_settings(&mut self, ctx: &egui::Context) {
        if let Some(p) = config::ensure_and_path()
            && std::fs::write(p, config::config_to_toml(&self.cfg)).is_ok()
        {
            self.settings.baseline = Some(self.cfg.clone());
            self.reregister_hotkey();
            self.reapply_font(ctx);
            let now = ctx.input(|i| i.time);
            self.toast = Some(("Saved".into(), now + 1.4));
        }
    }

    /// Re-resolve + re-apply the active theme from `self.cfg` (after Revert / Discard / a
    /// settings-sync pull).
    pub(crate) fn reapply_appearance(&mut self, ctx: &egui::Context) {
        let system_light = matches!(ctx.input(|i| i.raw.system_theme), Some(egui::Theme::Light));
        let want = resolved_theme_name(&self.cfg.appearance, system_light);
        colors::set(colors::by_name(&want));
        ui::apply_theme(ctx);
        self.theme_name = want;
    }

    /// Reset the unsaved-changes baseline to the current config (after an external reload).
    pub(crate) fn rebaseline_settings(&mut self) {
        self.settings.baseline = Some(self.cfg.clone());
    }

    /// Sticky footer: Save / Close / Revert, right-aligned. Returns true when Close was hit.
    fn settings_footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut close = false;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui::action_button(ui, "Save", true).clicked() {
                self.save_settings(ctx);
            }
            if ui::action_button(ui, "Close", false).clicked() {
                close = true;
            }
            if ui::action_button(ui, "Revert", false).clicked() {
                self.cfg = config::Config::load();
                self.settings.baseline = Some(self.cfg.clone());
                self.reapply_appearance(ctx);
                self.reregister_hotkey();
                self.reapply_font(ctx);
                let now = ctx.input(|i| i.time);
                self.toast = Some(("Reverted".into(), now + 1.4));
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("~/.config/stdusk/config.toml").size(11.0).color(colors::dim()),
            );
        });
        close
    }

    /// The full settings view, swapped into the central area while `settings_open` (the tab
    /// bar stays above). Nav sidebar left, footer bottom, scrollable content pane center.
    pub(crate) fn settings_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let opacity = self.fx_opacity;
        let mut close = false;
        // Sampled BEFORE the panels run: a focused text field (scheme search, hotkey) consumes
        // Esc to drop focus; only the NEXT Esc should close the view. Same for an open theme
        // dropdown - its own Esc closes the popup, not the view.
        let field_focused = ctx.memory(|m| m.focused().is_some());
        let dropdown_was_open = self.settings.dropdown_open.is_some();
        let mut appearance_fx: Option<AppearanceFx> = None;
        let mut quake_fx: Option<QuakeFx> = None;
        let mut sync_op: Option<sync::Op> = None;

        let nav = egui::Panel::left("settings_nav")
            .exact_size(NAV_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(ui::tint(colors::titlebar(), opacity))
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 10, se: 0 })
                    .inner_margin(egui::Margin::symmetric(10, 14)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(egui::RichText::new("SETTINGS").size(10.5).strong().color(colors::dim()));
                ui.add_space(6.0);
                for s in Section::ALL {
                    if nav_row(ui, s, self.settings.section == s).clicked() {
                        self.settings.open_section(s);
                    }
                }
                // Version pinned to the sidebar's bottom edge.
                let r = ui.max_rect();
                ui.painter().text(
                    egui::pos2(r.left() + 14.0, r.bottom() - 10.0),
                    egui::Align2::LEFT_CENTER,
                    concat!("stdusk ", env!("CARGO_PKG_VERSION")),
                    egui::FontId::proportional(11.0),
                    colors::dim(),
                );
            });
        // Hairline between nav and content (matches the tab-bar divider).
        let nr = nav.response.rect;
        ui.painter().vline(
            nr.right() + 0.5,
            nr.y_range(),
            egui::Stroke::new(1.0, colors::border()),
        );

        let footer = egui::Panel::bottom("settings_footer")
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(ui::tint(colors::titlebar(), opacity))
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 0, se: 10 })
                    .inner_margin(egui::Margin::symmetric(20, 10)),
            )
            .show(ui, |ui| self.settings_footer(ui, ctx));
        close |= footer.inner;
        let fr = footer.response.rect;
        ui.painter().hline(fr.x_range(), fr.top() - 0.5, egui::Stroke::new(1.0, colors::border()));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT).inner_margin(egui::Margin {
                left: 28,
                right: 20,
                top: 20,
                bottom: 10,
            }))
            .show(ui, |ui| {
                // Thin always-visible scrollbars that ALLOCATE their lane (6px), so the bar
                // can never overlay the content cards. (`solid()` reserves a lane too but its
                // bar rect collapses under this panel's multi-pass sizing - thin() doesn't.)
                ui.spacing_mut().scroll = egui::style::ScrollStyle::thin();
                ui.spacing_mut().slider_width = 190.0;
                // Cap the column width and ease it off the sidebar on wide windows (a hard-left
                // column next to a quake-wide dead zone reads unbalanced).
                let col = |ui: &egui::Ui| {
                    let col_w = ui.available_width().min(CONTENT_MAX_W);
                    let pad = ((ui.available_width() - col_w) / 2.0).clamp(0.0, 120.0);
                    (col_w, pad)
                };
                match self.settings.section {
                    // The scheme browser manages its own scrolling (fixed head, scrolling list).
                    Section::ColorScheme => {
                        let (col_w, pad) = col(ui);
                        let full = ui.available_rect_before_wrap();
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(full.left() + pad, full.top()),
                            egui::vec2(col_w, full.height()),
                        );
                        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        self.scheme_section(&mut ui, ctx);
                    }
                    section => {
                        // The ScrollArea spans the FULL pane so its bar pins to the far right
                        // edge; the readable column is laid out inside it.
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            let (col_w, pad) = col(ui);
                            ui.horizontal(|ui| {
                                ui.add_space(pad);
                                ui.vertical(|ui| {
                                    ui.set_max_width(col_w);
                                    match section {
                                        Section::Appearance => {
                                            appearance_fx = Some(appearance_section(
                                                ui,
                                                &mut self.cfg,
                                                &mut self.settings,
                                            ));
                                        }
                                        Section::Terminal => terminal_section(ui, &mut self.cfg),
                                        Section::Quake => {
                                            quake_fx = Some(quake_section(ui, &mut self.cfg));
                                        }
                                        Section::Session => {
                                            sync_op =
                                                session_section(ui, &mut self.cfg, self.sync_busy);
                                        }
                                        Section::About => about_section(ui),
                                        Section::ColorScheme => unreachable!(),
                                    }
                                    ui.add_space(16.0);
                                });
                            });
                        });
                    }
                }
            });

        // Appearance-section side effects: a committed font family rebuilds the egui fonts
        // (no-op when unchanged; "Font not found" toast when unresolvable).
        if appearance_fx.is_some_and(|fx| fx.font_commit) {
            self.reapply_font(ctx);
        }

        // Quake-section side effects that need &mut self / the viewport.
        if let Some(fx) = quake_fx {
            if fx.hotkey_commit && self.reregister_hotkey() {
                let now = ctx.input(|i| i.time);
                self.toast = Some((format!("Hotkey: {}", self.cfg.quake.hotkey), now + 1.4));
            }
            if fx.height_changed && self.screenshot.is_none() && self.visible {
                crate::apply_visibility(ctx, true, self.cfg.quake.height_pct);
            }
        }

        // Kick off a settings push/pull; a push saves first so the repo gets what you see.
        if let Some(op) = sync_op {
            if op == sync::Op::Push {
                self.save_settings(ctx);
            }
            self.sync_busy = true;
            sync::spawn(op, self.cfg.sync.repo.trim().to_owned(), &self.sync_slot, ctx.clone());
        }

        // Esc closes - but not while a hard modal (rename/paste/close/palette) or the find bar
        // owns it, not on the press that just unfocused a text field or closed a dropdown, and
        // not while the unsaved-changes modal is showing (it owns Esc = keep editing).
        let modal_owns_esc = self.renaming.is_some()
            || !self.pending_pastes.is_empty()
            || self.pending_close.is_some()
            || self.palette.is_some()
            || self.search.is_some()
            || self.settings.confirm_close;
        if !modal_owns_esc
            && !field_focused
            && !dropdown_was_open
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            close = true;
        }
        if close {
            self.request_close_settings();
        }
        if self.settings.confirm_close {
            self.unsaved_changes_modal(ctx);
        }
    }

    /// "Unsaved changes" confirm, shown when closing settings with edits that differ from the
    /// baseline: Save / Discard (restore the baseline incl. re-applying looks) / Keep editing.
    fn unsaved_changes_modal(&mut self, ctx: &egui::Context) {
        let mut save = false;
        let mut discard = false;
        let mut keep = false;
        egui::Window::new("Unsaved changes")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(ui::overlay_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Unsaved changes").strong().color(colors::fg()));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Your settings edits haven't been saved.")
                        .color(colors::dim()),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui::action_button(ui, "Save", true).clicked() {
                        save = true;
                    }
                    if ui::action_button(ui, "Discard", false).clicked() {
                        discard = true;
                    }
                    if ui::action_button(ui, "Keep editing", false).clicked() {
                        keep = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = true;
        }
        if save {
            self.save_settings(ctx);
            self.settings.confirm_close = false;
            self.settings_open = false;
        } else if discard {
            if let Some(b) = self.settings.baseline.take() {
                self.cfg = b;
            }
            self.reapply_appearance(ctx);
            self.reregister_hotkey();
            self.reapply_font(ctx);
            self.settings.confirm_close = false;
            self.settings_open = false;
        } else if keep {
            self.settings.confirm_close = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_exercises_the_core_palette() {
        let inks: Vec<Ink> = PREVIEW_LINES.iter().flat_map(|l| l.iter().map(|s| s.ink)).collect();
        // Red, green, yellow, blue, magenta, cyan + default fg all appear.
        for i in 1..=6 {
            assert!(inks.contains(&Ink::Ansi(i)), "ansi {i} missing from the preview");
        }
        assert!(inks.contains(&Ink::Fg));
        // At least one segment paints a background (the ls "executable" highlight).
        assert!(PREVIEW_LINES.iter().flat_map(|l| l.iter()).any(|s| s.bg.is_some()));
        // No empty segments (they'd paint zero-width galleys).
        assert!(PREVIEW_LINES.iter().flat_map(|l| l.iter()).all(|s| !s.text.is_empty()));
    }

    #[test]
    fn ink_color_maps_roles() {
        let t = crate::colors::one_half_dark();
        assert_eq!(ink_color(&t, Ink::Fg), t.fg);
        assert_eq!(ink_color(&t, Ink::Dim), t.ansi[8]);
        assert_eq!(ink_color(&t, Ink::Ansi(3)), t.ansi[3]);
        assert_eq!(ink_color(&t, Ink::Ansi(99)), t.ansi[15]); // clamped, no panic
    }

    #[test]
    fn scheme_filter_is_case_insensitive_substring() {
        let t = crate::colors::one_half_dark();
        let all: Vec<(String, Theme)> =
            ["dracula", "nord", "one-half-dark"].iter().map(|n| ((*n).to_string(), t)).collect();
        assert_eq!(filter_schemes(&all, ""), vec![0, 1, 2]);
        assert_eq!(filter_schemes(&all, "DRAC"), vec![0]);
        assert_eq!(filter_schemes(&all, "  nor "), vec![1]); // trimmed
        assert_eq!(filter_schemes(&all, "o"), vec![1, 2]);
        assert!(filter_schemes(&all, "zzz").is_empty());
    }

    #[test]
    fn name_filter_lowercases_both_sides() {
        let all: Vec<String> = ["Menlo", "JetBrainsMono Nerd Font", "Monaco"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(filter_names(&all, ""), vec![0, 1, 2]);
        assert_eq!(filter_names(&all, "MENLO"), vec![0]); // query lowercased
        assert_eq!(filter_names(&all, "nerd"), vec![1]); // name lowercased
        assert_eq!(filter_names(&all, " mon "), vec![1, 2]); // trimmed substring
        assert!(filter_names(&all, "zzz").is_empty());
    }

    #[test]
    fn scheme_slot_targets_the_resolving_field() {
        assert_eq!(scheme_slot(false, false), SchemeSlot::Fixed);
        assert_eq!(scheme_slot(false, true), SchemeSlot::Fixed);
        assert_eq!(scheme_slot(true, true), SchemeSlot::Light);
        assert_eq!(scheme_slot(true, false), SchemeSlot::Dark);
    }

    #[test]
    fn resolved_theme_name_mirrors_the_reconcile() {
        // follow_system defaults to true.
        let mut a = config::Appearance {
            theme: "fixed".into(),
            theme_light: "light".into(),
            theme_dark: "dark".into(),
            ..Default::default()
        };
        assert_eq!(resolved_theme_name(&a, true), "light");
        assert_eq!(resolved_theme_name(&a, false), "dark");
        a.follow_system = false;
        assert_eq!(resolved_theme_name(&a, true), "fixed");
    }

    #[test]
    fn normalize_matches_pack_naming() {
        assert_eq!(normalize_name("One Half_Dark"), "one-half-dark");
        assert_eq!(normalize_name("nord"), "nord");
    }

    fn run_frame(ctx: &egui::Context, events: Vec<egui::Event>, add: impl FnMut(&mut egui::Ui)) {
        let mut add = add;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| add(ui));
        });
    }

    #[test]
    fn sections_render_headless() {
        // Smoke: the reworked sections (searchable theme dropdowns, cursor preview, new-tab
        // hints, quake fx rows) build real frames without panicking.
        let ctx = egui::Context::default();
        let mut cfg = config::Config::default();
        let mut st = SettingsState::new();
        for _ in 0..2 {
            run_frame(&ctx, vec![], |ui| {
                let _fx = appearance_section(ui, &mut cfg, &mut st);
                let _fx = quake_section(ui, &mut cfg);
                terminal_section(ui, &mut cfg);
            });
        }
        assert!(st.dropdown_open.is_none());
    }

    #[test]
    fn scheme_dropdown_opens_filters_and_picks() {
        let ctx = egui::Context::default();
        let mut st = SettingsState::new();
        let mut value = "one-half-dark".to_string();
        // Frame 1: locate the button. Frames 2-3: click it -> the popup opens.
        run_frame(&ctx, vec![], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", &mut value);
        });
        let center = egui::pos2(30.0, 22.0); // inside the 200x28 button at the panel origin
        run_frame(&ctx, vec![egui::Event::PointerMoved(center)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", &mut value);
        });
        for pressed in [true, false] {
            run_frame(
                &ctx,
                vec![egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                }],
                |ui| {
                    let _ = scheme_dropdown(ui, &mut st, "probe", &mut value);
                },
            );
        }
        assert!(st.dropdown_open.is_some(), "clicking the dropdown must open its popup");
        // Type a filter that matches exactly one scheme, then Esc closes the popup.
        st.dropdown_filter = "tokyo-night".into();
        run_frame(&ctx, vec![], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", &mut value);
        });
        run_frame(
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            |ui| {
                let _ = scheme_dropdown(ui, &mut st, "probe", &mut value);
            },
        );
        assert!(st.dropdown_open.is_none(), "Esc must close the popup");
    }
}
