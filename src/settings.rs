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
    Profiles,
    Hotkeys,
    Quake,
    Session,
    About,
}

impl Section {
    const ALL: [Self; 8] = [
        Self::Appearance,
        Self::ColorScheme,
        Self::Terminal,
        Self::Profiles,
        Self::Hotkeys,
        Self::Quake,
        Self::Session,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::ColorScheme => "Color scheme",
            Self::Terminal => "Terminal",
            Self::Profiles => "Profiles",
            Self::Hotkeys => "Hotkeys",
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
            Self::Profiles => icons::IDENTIFICATION_BADGE,
            Self::Hotkeys => icons::KEYBOARD,
            Self::Quake => icons::LIGHTNING,
            Self::Session => icons::CLOCK_COUNTER_CLOCKWISE,
            Self::About => icons::INFO,
        }
    }
}

/// Settings-view state that outlives a frame (selected section, scheme search) plus the
/// one-frame hover handoff for the scheme preview card.
#[allow(clippy::struct_excessive_bools)] // independent one-shot flags, not a state machine
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
    dropdown_hl: Option<usize>,   // keyboard-highlighted row (index into the popup's filtered list)
    dropdown_scroll: bool,        // scroll the popup list to the highlight once (it just moved)
    dropdown_bright: Option<BrightFilter>, // open dropdown's chips; None = auto (follow the slot)
    force_dropdown: Option<String>, // open this dropdown (by id salt) on first render - shot harness
    scheme_bright: Option<BrightFilter>, // brightness chips; None = auto (follow the slot)
    profile_sel: Option<usize>,     // profile expanded in the Profiles editor
    profile_loaded: Option<usize>,  // which profile index the edit buffers below reflect
    profile_args: String,           // args line as typed (parsed into the Vec on change)
    profile_env: Vec<(String, String)>, // env rows as typed (folded into the map on change)
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
            dropdown_hl: None,
            dropdown_scroll: false,
            dropdown_bright: None,
            force_dropdown: None,
            scheme_bright: None,
            profile_sel: None,
            profile_loaded: None,
            profile_args: String::new(),
            profile_env: Vec::new(),
        }
    }

    /// Switch section; entering the scheme browser jumps its list to the active scheme. The
    /// profile edit buffers reload too (the config may have been replaced underneath).
    pub(crate) fn open_section(&mut self, section: Section) {
        self.section = section;
        self.scroll_to_active = section == Section::ColorScheme;
        self.dropdown_open = None;
        self.scheme_bright = None; // back to the auto pre-filter on (re)entry
        self.profile_loaded = None;
    }

    /// Expand a profile into the inline editor (the screenshot harness's Profiles shot).
    pub(crate) fn select_profile(&mut self, i: usize) {
        self.profile_sel = Some(i);
        self.profile_loaded = None;
    }

    /// Open the searchable dropdown with this id salt on its first render (screenshot harness:
    /// `STDUSK_SHOT_DROPDOWN` - a floating popup can't be pointer-driven headless).
    pub(crate) fn force_dropdown(&mut self, id_salt: String) {
        self.force_dropdown = Some(id_salt);
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

/// WCAG-AA legibility floor for the scheme-browser previews. The live grid nudges glyphs to the
/// user's `terminal.minimum_contrast` (default 4); the browser applies THIS floor unconditionally
/// so every scheme is readable while you're picking one, regardless of the live setting.
pub(crate) const PREVIEW_MIN_CONTRAST: f32 = 4.5;

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

/// Scheme-browser brightness filter (the All / Light / Dark chips).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BrightFilter {
    All,
    Light,
    Dark,
}

/// The brightness pre-filter for a picker targeting a specific config slot: the Light slot
/// opens on light schemes, Dark on dark ones, the manual fixed theme on all. Shared by the
/// scheme browser and the Appearance theme dropdowns.
pub(crate) fn slot_bright_filter(slot: SchemeSlot) -> BrightFilter {
    match slot {
        SchemeSlot::Fixed => BrightFilter::All,
        SchemeSlot::Light => BrightFilter::Light,
        SchemeSlot::Dark => BrightFilter::Dark,
    }
}

/// The filter the scheme browser opens on: following the system, a pick lands in the CURRENT
/// OS appearance's slot - pre-filter to schemes of that brightness. Manual mode shows all.
pub(crate) fn default_bright_filter(follow_system: bool, system_light: bool) -> BrightFilter {
    slot_bright_filter(scheme_slot(follow_system, system_light))
}

/// Does a scheme of the given darkness pass the brightness filter?
pub(crate) fn bright_allows(filter: BrightFilter, dark: bool) -> bool {
    match filter {
        BrightFilter::All => true,
        BrightFilter::Light => !dark,
        BrightFilter::Dark => dark,
    }
}

/// Arrow step over a dropdown popup's filtered list: Down from no highlight lands on the top
/// row, Up on the bottom one; steps wrap at both ends. `None` when the list is empty.
pub(crate) fn move_highlight(cur: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (cur, down) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => (i + len - 1) % len,
    })
}

/// The row Enter commits in a dropdown popup: the keyboard highlight when one is set
/// (clamped - the filter may have shrunk under it), else the TOP match, so typing a query
/// and hitting Enter picks the first hit. `None` on an empty list.
pub(crate) fn commit_index(hl: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(hl.map_or(0, |i| i.min(len - 1)))
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

/// Split a profile's arguments line into argv entries: whitespace separates, single/double
/// quotes group (and are stripped), backslash escapes the next char (outside single quotes).
/// An unterminated quote runs to the end - lenient, since the editor re-parses per keystroke.
pub(crate) fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false; // an empty quoted token ("") still counts
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => cur.push(chars.next().unwrap_or('\\')),
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_token = true;
            }
            None if c == '\\' => {
                cur.push(chars.next().unwrap_or('\\'));
                in_token = true;
            }
            None if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            None => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}

/// Render argv entries back into one editable line: entries with whitespace / quotes /
/// backslashes (or empty ones) get double-quoted with inner escapes, the rest stay bare.
/// Round-trips: `split_args(&join_args(a)) == a`.
pub(crate) fn join_args(args: &[String]) -> String {
    let quote = |a: &String| -> String {
        let plain = !a.is_empty()
            && !a.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'));
        if plain {
            return a.clone();
        }
        let mut out = String::from("\"");
        for c in a.chars() {
            if matches!(c, '"' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    };
    args.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// Fold the editor's env rows into the profile map: blank keys dropped (half-typed rows),
/// keys trimmed, duplicate keys last-write-wins (what the spawned shell would see anyway).
pub(crate) fn env_rows_to_map(
    rows: &[(String, String)],
) -> std::collections::BTreeMap<String, String> {
    rows.iter()
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| (k.trim().to_string(), v.clone()))
        .collect()
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
    ui::focus_ring(ui, &resp, 9.0);
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
    ui::focus_ring(ui, &resp, 8.0);
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
            // The preview must show what the LIVE grid shows, not raw theme colors: the terminal
            // nudges every glyph to `minimum_contrast`, so a low-contrast scheme reads fine live
            // but looked unreadable here. Floor the ink against its actual bg (WCAG AA) so every
            // scheme previews legibly - no scheme-data mangling needed.
            let run_bg = run.bg.map_or(theme.bg, |b| ink_color(theme, b));
            let color =
                colors::ensure_contrast(ink_color(theme, run.ink), run_bg, PREVIEW_MIN_CONTRAST);
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
        // UI label, not preview fidelity: a handful of pack schemes ship fg-vs-bg under 3:1
        // (a11y audit) - nudge so every row name stays readable. The swatches show the truth.
        colors::ensure_contrast(t.fg, t.bg, PREVIEW_MIN_CONTRAST),
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
    ui::focus_ring(ui, &resp, 9.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---- searchable dropdowns (theme slots + font family) ----

/// Keyboard state for one open searchable-dropdown popup, read once per frame by the `list`
/// closure: Up/Down move `st.dropdown_hl` over the `len` filtered rows (the search field
/// keeps widget focus - the highlight is popup state, not egui focus; a TextEdit's event
/// filter claims the arrows so egui can't move focus away either). Returns the row Enter
/// commits, honored only while the keyboard sits on the search field (or nothing), so
/// Enter on a focused chip/row activates that widget instead of double-firing a pick.
fn dropdown_nav(
    ui: &egui::Ui,
    st: &mut SettingsState,
    field: egui::Id,
    len: usize,
) -> Option<usize> {
    let (up, down, enter) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
        )
    });
    if up != down {
        st.dropdown_hl = move_highlight(st.dropdown_hl, len, down);
        st.dropdown_scroll = true; // the list scrolls to follow the keyboard highlight
    } else if len == 0 {
        st.dropdown_hl = None;
    } else if let Some(hl) = st.dropdown_hl
        && hl >= len
    {
        st.dropdown_hl = Some(len - 1); // the filter shrank under the highlight
    }
    let field_owns = ui.ctx().memory(|m| m.focused().is_none_or(|f| f == field));
    (enter && field_owns).then(|| commit_index(st.dropdown_hl, len)).flatten()
}

/// Combo-style button + searchable-overlay scaffold shared by the scheme and font dropdowns:
/// paints the closed button (current `label` + caret), owns the open/filter/focus-once state
/// (`st.dropdown_*`, one open at a time), and dismisses on pick, Esc, or an outside press.
/// `list` draws the filtered rows inside the popup (keyboard nav via `dropdown_nav`, keyed by
/// the passed search-field id) and returns true when an item was picked.
fn searchable_dropdown(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    id_salt: &str,
    label: &str,
    hint: &str,
    list: impl FnOnce(&mut egui::Ui, &mut SettingsState, egui::Id) -> bool,
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
    ui::focus_ring(ui, &resp, 7.0);
    if resp.clicked() {
        st.dropdown_open = if open { None } else { Some(id) };
        st.dropdown_filter.clear();
        st.dropdown_bright = None; // back to the slot's auto pre-filter on every open
        st.dropdown_focus = true;
        st.dropdown_hl = None;
    }
    // Screenshot harness: open this dropdown on first render (a floating popup can't be
    // pointer-driven headless), same state as a click.
    if st.force_dropdown.as_deref() == Some(id_salt) {
        st.force_dropdown = None;
        st.dropdown_open = Some(id);
        st.dropdown_filter.clear();
        st.dropdown_bright = None;
        st.dropdown_hl = None;
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
                if r.changed() {
                    st.dropdown_hl = None; // a new query restarts at "top match"
                }
                ui.add_space(6.0);
                picked = list(ui, st, r.id);
            });
        });
    // Close on pick, Esc, or a press outside both the popup and its button. The settings
    // view's own Esc handling samples `dropdown_open` BEFORE the panels run, so the press
    // that closes this popup never also closes settings.
    let dismissed = ui.ctx().input(|i| {
        i.key_pressed(egui::Key::Escape)
            || (i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p) && !rect.contains(p)))
    });
    if picked || dismissed {
        st.dropdown_open = None;
        st.dropdown_hl = None;
    }
    picked
}

/// One row of a searchable-dropdown popup: selection fill + accent text when active, a
/// hover-strength fill while keyboard-`highlighted` (the popup sits on an elevated surface,
/// so `hover_elevated` - plain `hover` vanishes there), hover fill otherwise.
fn dropdown_row(ui: &mut egui::Ui, text: &str, active: bool, highlighted: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 6.0, colors::selection());
    } else if highlighted {
        p.rect_filled(rect, 6.0, colors::hover_elevated());
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
    ui::focus_ring(ui, &resp, 6.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A searchable scheme picker (all three theme slots). Opens pre-filtered to the brightness
/// of the slot it sets (`slot_bright_filter`: Light slot -> light schemes, Dark -> dark,
/// manual fixed theme -> all), with All/Light/Dark chips inside the popup as the escape
/// hatch; search combines with the filter. Hovering a row hands the scheme to
/// `st.hover_preview` so the section's preview card follows it. Returns true when a scheme
/// was picked into `value`.
fn scheme_dropdown(
    ui: &mut egui::Ui,
    st: &mut SettingsState,
    id_salt: &str,
    slot: SchemeSlot,
    value: &mut String,
) -> bool {
    let all = themes::all_schemes();
    let label = value.clone();
    let hint = format!("Search {} schemes", all.len());
    searchable_dropdown(ui, st, id_salt, &label, &hint, |ui, st, field| {
        let mut bright = st.dropdown_bright.unwrap_or_else(|| slot_bright_filter(slot));
        ui.horizontal(|ui| {
            for (label, f) in [
                ("All", BrightFilter::All),
                ("Light", BrightFilter::Light),
                ("Dark", BrightFilter::Dark),
            ] {
                if ui::chip(ui, label, bright == f).clicked() {
                    st.dropdown_bright = Some(f);
                    bright = f; // applies this frame, not next
                }
            }
        });
        ui.add_space(6.0);
        let shown: Vec<usize> = filter_schemes(all, &st.dropdown_filter)
            .into_iter()
            .filter(|&i| bright_allows(bright, colors::theme_is_dark(&all[i].1)))
            .collect();
        if shown.is_empty() {
            st.dropdown_hl = None;
            ui.label(egui::RichText::new("No schemes match.").color(colors::dim()));
            return false;
        }
        let commit = dropdown_nav(ui, st, field, shown.len());
        // The keyboard highlight drives the section's live preview card, exactly like
        // pointer hover (which, set inside the row loop, wins while actually hovering).
        if let Some(hl) = st.dropdown_hl {
            st.hover_preview = Some(all[shown[hl]].1);
        }
        let mut picked = false;
        let mut list = egui::ScrollArea::vertical().max_height(230.0);
        if std::mem::take(&mut st.dropdown_scroll)
            && let Some(hl) = st.dropdown_hl
        {
            // Keep the moved highlight in view (show_rows steps by row height + spacing).
            let step = 24.0 + ui.spacing().item_spacing.y;
            list = list.vertical_scroll_offset((hl as f32 * step - 103.0).max(0.0));
        }
        list.show_rows(ui, 24.0, shown.len(), |ui, range| {
            for i in range {
                let (name, theme) = &all[shown[i]];
                let name = name.as_str();
                let resp = dropdown_row(
                    ui,
                    name,
                    name == normalize_name(value),
                    st.dropdown_hl == Some(i),
                );
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
        });
        if let Some(ci) = commit {
            all[shown[ci]].0.clone_into(value);
            picked = true;
        }
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
    searchable_dropdown(ui, st, id_salt, &label, &hint, |ui, st, field| {
        let shown = filter_names(all, &st.dropdown_filter);
        let with_default = st.dropdown_filter.trim().is_empty();
        if shown.is_empty() && !with_default {
            st.dropdown_hl = None;
            ui.label(egui::RichText::new("No fonts match.").color(colors::dim()));
            return false;
        }
        let total = shown.len() + usize::from(with_default);
        // Row i -> family name ("" = the bundled-default reset row while unfiltered).
        let row_name = |i: usize| {
            if with_default && i == 0 {
                ""
            } else {
                all[shown[i - usize::from(with_default)]].as_str()
            }
        };
        let commit = dropdown_nav(ui, st, field, total);
        let mut picked = false;
        let mut list = egui::ScrollArea::vertical().max_height(230.0);
        if std::mem::take(&mut st.dropdown_scroll)
            && let Some(hl) = st.dropdown_hl
        {
            let step = 24.0 + ui.spacing().item_spacing.y;
            list = list.vertical_scroll_offset((hl as f32 * step - 103.0).max(0.0));
        }
        list.show_rows(ui, 24.0, total, |ui, range| {
            for i in range {
                let name = row_name(i);
                let text = if name.is_empty() { DEFAULT_LABEL } else { name };
                if dropdown_row(ui, text, name == value, st.dropdown_hl == Some(i)).clicked() {
                    name.clone_into(value);
                    picked = true;
                }
            }
        });
        if let Some(ci) = commit {
            row_name(ci).clone_into(value);
            picked = true;
        }
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
    // Unfocused-dim is a quake-only effect; window mode greys it out (see quake_section).
    let window_mode = config::is_window_mode(cfg);

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
                scheme_dropdown(ui, st, "theme_light", SchemeSlot::Light, &mut a.theme_light);
            });
            row(ui, "Dark theme", "Applied while macOS is dark", |ui| {
                scheme_dropdown(ui, st, "theme_dark", SchemeSlot::Dark, &mut a.theme_dark);
            });
        } else {
            row(ui, "Theme", "Also browsable in the Color scheme section", |ui| {
                scheme_dropdown(ui, st, "theme_fixed", SchemeSlot::Fixed, &mut a.theme);
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
            "No effect while Hide on focus loss is on, or in window mode",
            |ui| {
                ui.add_enabled_ui(!window_mode, |ui| {
                    pct_slider(ui, &mut q.unfocused_opacity, 0.2..=1.0);
                });
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
        row(ui, "Right click", "Paste/Copy act on a quick tap; holding opens the menu", |ui| {
            ui.horizontal(|ui| {
                for (label, value) in
                    [("Menu", "menu"), ("Paste", "paste"), ("Copy or paste", "clipboard")]
                {
                    let selected =
                        ui::right_click_mode(&t.right_click) == ui::right_click_mode(value);
                    if ui::chip(ui, label, selected).clicked() {
                        t.right_click = value.into();
                    }
                }
            });
        });
        row(ui, "Focus follows mouse", "Hovering a split pane focuses it (no click)", |ui| {
            ui::toggle_switch(ui, &mut t.focus_follows_mouse);
        });
    });

    // (Cursor / ligatures / bold-bright / opacity live in Appearance - one preview there
    // covers them all. Terminal is behavior only.)
}

// ---- Profiles section ----

/// Side effects the Profiles section needs applied by the caller (which has `&mut Stdusk`).
struct ProfilesFx {
    launch: Option<usize>, // spawn a tab with this profile (index into cfg.profiles)
}

/// Deferred list mutations (collected while iterating, applied after - ui.md §2).
enum ProfileAct {
    Add,
    Duplicate(usize),
    Delete(usize),
}

/// A small hover-highlighted Phosphor icon hit-box painted INSIDE another widget's rect
/// (the profile rows' trailing Launch/Duplicate/Delete). Registered after the row's own
/// interact so it wins its clicks (same ordering trick as the tab close-x).
fn inline_icon(ui: &mut egui::Ui, id: egui::Id, center: egui::Pos2, icon: &str, tip: &str) -> bool {
    let rect = egui::Rect::from_center_size(center, egui::vec2(24.0, 24.0));
    let resp = ui.interact(rect, id, egui::Sense::click()).on_hover_text(tip);
    let hovered = resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, 6.0, colors::hover_elevated());
    }
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(14.0),
        if hovered { colors::fg() } else { colors::dim() },
    );
    ui::focus_ring(ui, &resp, 6.0);
    resp.clicked()
}

/// One profile list row: color dot + name + shell summary, with trailing Launch / Duplicate /
/// Delete icons. Returns (row clicked, launch, duplicate, delete).
fn profile_row(
    ui: &mut egui::Ui,
    i: usize,
    p: &config::Profile,
    selected: bool,
) -> (bool, bool, bool, bool) {
    let w = ui.available_width().min(CONTENT_MAX_W);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 46.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 9.0, colors::selection());
    } else if resp.hovered() {
        painter.rect_filled(rect, 9.0, colors::hover());
    }
    let stroke = if selected {
        egui::Stroke::new(1.5, colors::accent())
    } else {
        egui::Stroke::new(1.0, colors::border())
    };
    painter.rect_stroke(rect, 9.0, stroke, egui::StrokeKind::Inside);
    // Color dot: the profile's tab color, or a hollow ring for "no color".
    let dot = egui::pos2(rect.left() + 20.0, rect.center().y);
    match p.color.as_deref().and_then(crate::session::hex_to_color) {
        Some(c) => {
            painter.circle_filled(dot, 6.0, c);
        }
        None => {
            painter.circle_stroke(dot, 5.5, egui::Stroke::new(1.5, colors::dim()));
        }
    }
    painter.text(
        egui::pos2(rect.left() + 38.0, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        &p.name,
        egui::FontId::proportional(13.5),
        colors::fg(),
    );
    let shell = p.shell.as_deref().unwrap_or("default shell ($SHELL)");
    painter.text(
        egui::pos2(rect.left() + 38.0, rect.center().y + 8.0),
        egui::Align2::LEFT_CENTER,
        shell,
        egui::FontId::proportional(11.0),
        colors::dim(),
    );
    ui::focus_ring(ui, &resp, 9.0);
    // Trailing icons, right to left: Delete, Duplicate, Launch.
    let mut x = rect.right() - 22.0;
    let mut hits = [false; 3];
    for (slot, (icon, tip)) in [
        (icons::TRASH, "Delete profile"),
        (icons::COPY, "Duplicate profile"),
        (icons::PLAY, "Launch (new tab with this profile)"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = ui.id().with(("profile_icon", i, slot));
        hits[slot] = inline_icon(ui, id, egui::pos2(x, rect.center().y), icon, tip);
        x -= 30.0;
    }
    (resp.clicked(), hits[2], hits[1], hits[0])
}

/// The inline profile editor (shown for the selected row): plain fields edit the profile
/// directly; args/env go through `SettingsState` buffers so half-typed quotes and blank env
/// rows survive re-render (the parsed result lands in `cfg.profiles` on every change).
fn profile_editor(ui: &mut egui::Ui, cfg: &mut config::Config, st: &mut SettingsState, i: usize) {
    if st.profile_loaded != Some(i) {
        st.profile_loaded = Some(i);
        st.profile_args = join_args(&cfg.profiles[i].args);
        st.profile_env = cfg.profiles[i].env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    let p = &mut cfg.profiles[i];
    subheading(ui, "Edit profile");
    rows(ui, |ui| {
        row(ui, "Name", "", |ui| {
            ui::text_field(ui, &mut p.name, "profile name", 200.0, colors::fg());
        });
        row_full(ui, "Shell", "Command run for this profile's tabs", "Empty uses $SHELL", |ui| {
            let mut buf = p.shell.clone().unwrap_or_default();
            if ui::text_field(ui, &mut buf, "/bin/zsh", 200.0, colors::fg()).changed() {
                p.shell = (!buf.is_empty()).then_some(buf);
            }
        });
        row_full(
            ui,
            "Arguments",
            "Extra shell arguments",
            "Space-separated; quote to keep spaces",
            |ui| {
                if ui::text_field(ui, &mut st.profile_args, "-l -i", 200.0, colors::fg()).changed()
                {
                    p.args = split_args(&st.profile_args);
                }
            },
        );
        row_full(ui, "Working directory", "", "~ expands to your home", |ui| {
            let mut buf = p.cwd.clone().unwrap_or_default();
            if ui::text_field(ui, &mut buf, "~/Git", 200.0, colors::fg()).changed() {
                p.cwd = (!buf.is_empty()).then_some(buf);
            }
        });
        row(ui, "Tab color", "", |ui| {
            ui.vertical(|ui| {
                if ui::chip(ui, "No color", p.color.is_none()).clicked() {
                    p.color = None;
                }
                ui.add_space(4.0);
                for swatch_row in colors::tab_colors().chunks(6) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for &c in swatch_row {
                            let hex = crate::session::color_to_hex(c);
                            if ui::color_swatch(ui, c, p.color.as_deref() == Some(hex.as_str()))
                                .clicked()
                            {
                                p.color = Some(hex);
                            }
                        }
                    });
                }
            });
        });
    });

    subheading(ui, "Environment");
    let mut env_changed = false;
    let mut remove: Option<usize> = None;
    for (ri, (k, v)) in st.profile_env.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            env_changed |= ui::text_field(ui, k, "NAME", 150.0, colors::fg()).changed();
            ui.label(egui::RichText::new("=").color(colors::dim()));
            env_changed |= ui::text_field(ui, v, "value", 200.0, colors::fg()).changed();
            if ui::icon_button(ui, icons::TRASH, "Remove variable").clicked() {
                remove = Some(ri);
            }
        });
    }
    if let Some(ri) = remove {
        st.profile_env.remove(ri);
        env_changed = true;
    }
    ui.add_space(4.0);
    if ui::action_button(ui, "Add variable", false).clicked() {
        st.profile_env.push((String::new(), String::new()));
    }
    if env_changed {
        p.env = env_rows_to_map(&st.profile_env);
    }
}

/// The Profiles section: list + Add / Duplicate / Delete / Launch, with a click-to-edit
/// inline panel. All edits live in `cfg.profiles`; the footer Save persists them.
fn profiles_section(
    ui: &mut egui::Ui,
    cfg: &mut config::Config,
    st: &mut SettingsState,
) -> ProfilesFx {
    title(ui, "Profiles");
    ui.label(
        egui::RichText::new(
            "Named launch presets: shell, arguments, directory, environment, tab color. \
             Launch from here, the tab bar's + (right-click), the tab menu, or the palette.",
        )
        .size(11.5)
        .color(colors::dim()),
    );
    ui.add_space(12.0);

    let mut fx = ProfilesFx { launch: None };
    let mut act: Option<ProfileAct> = None;
    ui.spacing_mut().item_spacing.y = 6.0;
    for (i, p) in cfg.profiles.iter().enumerate() {
        let selected = st.profile_sel == Some(i);
        let (clicked, launch, dup, del) = profile_row(ui, i, p, selected);
        if launch {
            fx.launch = Some(i);
        }
        if dup {
            act = Some(ProfileAct::Duplicate(i));
        }
        if del {
            act = Some(ProfileAct::Delete(i));
        }
        if clicked {
            st.profile_sel = if selected { None } else { Some(i) };
            st.profile_loaded = None;
        }
    }
    if cfg.profiles.is_empty() {
        ui.label(egui::RichText::new("No profiles yet.").color(colors::dim()));
    }
    ui.add_space(8.0);
    if ui::action_button(ui, "Add profile", true).clicked() {
        act = Some(ProfileAct::Add);
    }
    match act {
        Some(ProfileAct::Add) => {
            cfg.profiles.push(config::Profile {
                name: format!("profile {}", cfg.profiles.len() + 1),
                shell: None,
                args: Vec::new(),
                cwd: None,
                env: std::collections::BTreeMap::new(),
                color: None,
            });
            st.profile_sel = Some(cfg.profiles.len() - 1);
            st.profile_loaded = None;
        }
        Some(ProfileAct::Duplicate(i)) => {
            let mut copy = cfg.profiles[i].clone();
            copy.name.push_str(" copy");
            cfg.profiles.insert(i + 1, copy);
            st.profile_sel = Some(i + 1);
            st.profile_loaded = None;
        }
        Some(ProfileAct::Delete(i)) => {
            cfg.profiles.remove(i);
            st.profile_sel = None;
            st.profile_loaded = None;
        }
        None => {}
    }
    if let Some(i) = st.profile_sel {
        if i < cfg.profiles.len() {
            profile_editor(ui, cfg, st, i);
        } else {
            st.profile_sel = None; // selection outlived the list (delete)
        }
    }
    fx
}

// ---- Hotkeys section ----

/// Side effects the Hotkeys section needs applied by the caller (invalid-spec toast).
struct HotkeysFx {
    invalid: Option<String>, // a field committed (blur) with an unparseable chord
}

/// One hotkey row: action label + editable chord field. Invalid non-empty specs render red
/// live and toast on blur; they simply never fire (empty = unbound on purpose).
fn hotkey_row(
    ui: &mut egui::Ui,
    name: &str,
    default: &str,
    value: &mut String,
    fx: &mut HotkeysFx,
) {
    row(ui, name, &format!("Default {default}"), |ui| {
        let invalid = !value.trim().is_empty() && ui::parse_hotkey_spec(value).is_none();
        let color = if invalid { colors::red() } else { colors::fg() };
        let r = ui::text_field(ui, value, "unbound", 160.0, color);
        if r.lost_focus() && invalid {
            fx.invalid = Some(value.clone());
        }
    });
}

/// The Hotkeys section: every remappable app action with its current chord. Edits live in
/// `cfg.hotkeys` and apply instantly; Save persists the `[hotkeys]` table.
fn hotkeys_section(ui: &mut egui::Ui, cfg: &mut config::Config) -> HotkeysFx {
    title(ui, "Hotkeys");
    ui.label(
        egui::RichText::new(
            "Chords are modifiers (Cmd / Ctrl / Alt / Shift) + a key, e.g. Cmd+Shift+K. \
             Applies as you type; empty = unbound. A bind that collides with a terminal key \
             (e.g. Ctrl+letter) also reaches the shell. The global summon hotkey lives in Quake.",
        )
        .size(11.5)
        .color(colors::dim()),
    );
    let mut fx = HotkeysFx { invalid: None };
    let d = config::Hotkeys::default();
    let h = &mut cfg.hotkeys;

    subheading(ui, "Tabs");
    rows(ui, |ui| {
        hotkey_row(ui, "New tab", &d.new_tab, &mut h.new_tab, &mut fx);
        hotkey_row(ui, "Close pane / tab", &d.close, &mut h.close, &mut fx);
        hotkey_row(ui, "Reopen closed tab", &d.reopen, &mut h.reopen, &mut fx);
        hotkey_row(ui, "Toggle last tab", &d.toggle_last_tab, &mut h.toggle_last_tab, &mut fx);
    });
    subheading(ui, "Panes");
    rows(ui, |ui| {
        hotkey_row(ui, "Split right", &d.split_right, &mut h.split_right, &mut fx);
        hotkey_row(ui, "Split down", &d.split_down, &mut h.split_down, &mut fx);
        hotkey_row(ui, "Broadcast input", &d.broadcast, &mut h.broadcast, &mut fx);
    });
    subheading(ui, "Terminal");
    rows(ui, |ui| {
        hotkey_row(ui, "Find", &d.find, &mut h.find, &mut fx);
        hotkey_row(ui, "Select all", &d.select_all, &mut h.select_all, &mut fx);
        hotkey_row(ui, "Clear terminal", &d.clear, &mut h.clear, &mut fx);
        hotkey_row(ui, "Zoom in", &d.zoom_in, &mut h.zoom_in, &mut fx);
        hotkey_row(ui, "Zoom out", &d.zoom_out, &mut h.zoom_out, &mut fx);
        hotkey_row(ui, "Zoom reset", &d.zoom_reset, &mut h.zoom_reset, &mut fx);
    });
    subheading(ui, "App");
    rows(ui, |ui| {
        hotkey_row(ui, "Command palette", &d.palette, &mut h.palette, &mut fx);
        hotkey_row(ui, "Settings", &d.settings, &mut h.settings, &mut fx);
    });
    ui.add_space(14.0);
    if ui::action_button(ui, "Reset to defaults", false).clicked() {
        *h = d;
    }
    fx
}

/// Side effects the Quake section needs applied by the caller (which has `&mut Stdusk`).
struct QuakeFx {
    hotkey_commit: bool,  // the hotkey field lost focus - re-register if it changed
    height_changed: bool, // the height slider moved - re-apply the window size live
    mode_changed: bool,   // dropdown<->window picked - re-apply chrome/hotkey/activation live
}

fn quake_section(ui: &mut egui::Ui, cfg: &mut config::Config) -> QuakeFx {
    title(ui, "Quake");
    let mut fx = QuakeFx { hotkey_commit: false, height_changed: false, mode_changed: false };
    // Window mode greys out the quake-only options below (they no longer apply), so the UI can't
    // lie about what's in effect.
    let window_mode = config::is_window_mode(cfg);

    subheading(ui, "Mode");
    rows(ui, |ui| {
        row_full(
            ui,
            "Window mode",
            "Dropdown drops from the top on the hotkey; Window is a normal resizable macOS window",
            "Window mode disables the global hotkey and the Dock/focus options; chrome applies on restart",
            |ui| {
                ui.horizontal(|ui| {
                    for (label, value) in [("Dropdown", "dropdown"), ("Window", "window")] {
                        let selected =
                            config::window_mode(&cfg.quake.mode) == config::window_mode(value);
                        if ui::chip(ui, label, selected).clicked() && !selected {
                            cfg.quake.mode = value.into();
                            fx.mode_changed = true;
                        }
                    }
                });
            },
        );
    });

    let q = &mut cfg.quake;
    subheading(ui, "Window");
    rows(ui, |ui| {
        row(ui, "Global hotkey", "Applies when the field loses focus", |ui| {
            ui.add_enabled_ui(!window_mode, |ui| {
                let r =
                    ui::text_field(ui, &mut q.hotkey, "e.g. Ctrl+Grave, F13", 180.0, colors::fg());
                if r.lost_focus() {
                    fx.hotkey_commit = true;
                }
            });
        });
        row(ui, "Window height", "Fraction of the screen the window drops down", |ui| {
            ui.add_enabled_ui(!window_mode, |ui| {
                fx.height_changed = pct_slider(ui, &mut q.height_pct, 0.2..=0.9);
            });
        });
    });

    subheading(ui, "Focus & Dock");
    rows(ui, |ui| {
        row(ui, "Hide on focus loss", "Hide when another app takes focus", |ui| {
            ui.add_enabled_ui(!window_mode, |ui| {
                ui::toggle_switch(ui, &mut q.hide_on_focus_loss);
            });
        });
        row(
            ui,
            "Follow active desktop",
            "Drop onto whichever Space is active when summoned",
            |ui| {
                ui.add_enabled_ui(!window_mode, |ui| {
                    ui::toggle_switch(ui, &mut q.follow_active_space);
                });
            },
        );
        row(ui, "Hide from Dock", "Run as an accessory app (no Dock icon)", |ui| {
            ui.add_enabled_ui(!window_mode, |ui| {
                ui::toggle_switch(ui, &mut q.hide_from_dock);
            });
        });
        row(ui, "Menu-bar icon", "Status item with Show/Hide and Quit", |ui| {
            ui::toggle_switch(ui, &mut q.menu_bar_icon);
        });
        row(ui, "Dock icon while visible", "Show the Dock icon only while shown", |ui| {
            ui.add_enabled_ui(!window_mode, |ui| {
                ui::toggle_switch(ui, &mut q.dock_when_visible);
            });
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
        row_full(
            ui,
            "Auto sync",
            "Pull on launch, push on save",
            "Needs a sync repo above",
            |ui| {
                ui.add_enabled_ui(!cfg.sync.repo.trim().is_empty(), |ui| {
                    ui::toggle_switch(ui, &mut cfg.sync.auto);
                });
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
    /// Open (or re-activate) the settings view and dismiss the find bar (the workspace it
    /// searches is being swapped out). Only a FRESH session snapshots the unsaved-changes
    /// baseline - re-activating the existing Settings tab must not bless staged edits as
    /// clean.
    pub(crate) fn open_settings(&mut self) {
        if self.search.take().is_some() {
            self.tabs[self.active].focused_term().clear_selection();
        }
        if !self.settings_tab {
            self.settings.baseline = Some(self.cfg.clone());
        }
        self.settings_tab = true;
        self.settings.confirm_close = false;
        self.settings_open = true;
    }

    /// CLOSE the settings session (footer Close / Esc / the Settings tab's x) - or, with
    /// unsaved changes, show the confirm modal instead. Ends the Settings tab; a mere tab
    /// switch away never comes through here.
    pub(crate) fn request_close_settings(&mut self) {
        let dirty =
            self.settings.baseline.as_ref().is_some_and(|b| config::config_dirty(b, &self.cfg));
        if dirty {
            self.settings.confirm_close = true;
        } else {
            self.settings_open = false;
            self.settings_tab = false;
        }
    }

    /// Gear / Cmd+, : toggle the settings VIEW. Hiding is a tab-switch away - the Settings
    /// tab and its staged edits stay; the unsaved-changes guard runs only on explicit close.
    pub(crate) fn toggle_settings(&mut self) {
        if self.settings_open {
            self.settings_open = false;
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

        // Brightness chips: pre-filtered to the slot being set while following the system
        // (picking there can only land in that slot anyway), all schemes in manual mode; the
        // user can always override. Resets to the auto default on re-entry (open_section).
        let auto = default_bright_filter(self.cfg.appearance.follow_system, system_light);
        let mut bright = self.settings.scheme_bright.unwrap_or(auto);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(icons::MAGNIFYING_GLASS).size(15.0).color(colors::dim()));
            ui::text_field(
                ui,
                &mut self.settings.filter,
                &format!("Search {} color schemes", all.len()),
                260.0,
                colors::fg(),
            );
            ui.add_space(10.0);
            for (label, f) in [
                ("All", BrightFilter::All),
                ("Light", BrightFilter::Light),
                ("Dark", BrightFilter::Dark),
            ] {
                if ui::chip(ui, label, bright == f).clicked() {
                    self.settings.scheme_bright = Some(f);
                    bright = f; // applies this frame, not next
                }
            }
        });
        ui.add_space(10.0);

        // Uniform-height rows through show_rows so 195 palette strips scroll smoothly.
        self.settings.hover_preview = None;
        let shown: Vec<usize> = filter_schemes(all, &self.settings.filter)
            .into_iter()
            .filter(|&i| bright_allows(bright, colors::theme_is_dark(&all[i].1)))
            .collect();
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
    /// With `[sync] auto` on, every successful Save also pushes (Save is the only disk
    /// write, so this is THE autosync push hook); an in-flight sync op absorbs rapid saves.
    fn save_settings(&mut self, ctx: &egui::Context) {
        if let Some(p) = config::ensure_and_path()
            && std::fs::write(p, config::config_to_toml(&self.cfg)).is_ok()
        {
            self.settings.baseline = Some(self.cfg.clone());
            self.reregister_hotkey();
            self.reapply_font(ctx);
            let now = ctx.input(|i| i.time);
            self.toast = Some(("Saved".into(), now + 1.4));
            let repo = self.cfg.sync.repo.trim();
            if sync::should_autosync(self.cfg.sync.auto, !repo.is_empty(), self.sync_busy) {
                self.sync_busy = true;
                sync::spawn(sync::Op::Push, repo.to_owned(), &self.sync_slot, ctx.clone());
            }
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
    /// The profile edit buffers reload too - they'd otherwise show the replaced config.
    pub(crate) fn rebaseline_settings(&mut self) {
        self.settings.baseline = Some(self.cfg.clone());
        self.settings.profile_loaded = None;
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
                self.settings.profile_loaded = None; // buffers reload from the restored config
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
        let mut profiles_fx: Option<ProfilesFx> = None;
        let mut hotkeys_fx: Option<HotkeysFx> = None;
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
                    // STDUSK_SHOT_FOCUS: hand keyboard focus to the active section's row
                    // BEFORE it draws, so the shared focus ring lands in the very first pass
                    // (the harness captures pass-1 content; headless frames can't Tab).
                    if self.screenshot.is_some()
                        && self.settings.section == s
                        && std::env::var("STDUSK_SHOT_FOCUS").is_ok()
                    {
                        let id = ui.next_auto_id();
                        ui.ctx().memory_mut(|m| m.request_focus(id));
                    }
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
                                        Section::Profiles => {
                                            profiles_fx = Some(profiles_section(
                                                ui,
                                                &mut self.cfg,
                                                &mut self.settings,
                                            ));
                                        }
                                        Section::Hotkeys => {
                                            hotkeys_fx = Some(hotkeys_section(ui, &mut self.cfg));
                                        }
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
            if fx.mode_changed {
                self.apply_quake_mode(ctx);
            }
            if fx.hotkey_commit && self.reregister_hotkey() {
                let now = ctx.input(|i| i.time);
                self.toast = Some((format!("Hotkey: {}", self.cfg.quake.hotkey), now + 1.4));
            }
            if fx.height_changed && self.screenshot.is_none() && self.visible {
                crate::apply_visibility(ctx, true, self.cfg.quake.height_pct);
            }
        }

        // Profiles-section side effects: Launch spawns a tab with the profile (visible in the
        // tab bar right above the settings view).
        if let Some(fx) = profiles_fx
            && let Some(i) = fx.launch
            && let Some(name) = self.cfg.profiles.get(i).map(|p| p.name.clone())
        {
            self.apply_tab_action(Some(crate::tabs::TabAction::NewWithProfile(i)), ctx);
            let now = ctx.input(|i| i.time);
            self.toast = Some((format!("Launched {name}"), now + 1.4));
        }

        // Hotkeys-section side effects: an invalid chord committed on blur toasts (the bind
        // itself is harmless - an unparseable spec never matches).
        if let Some(fx) = hotkeys_fx
            && let Some(bad) = fx.invalid
        {
            let now = ctx.input(|i| i.time);
            self.toast = Some((format!("Invalid hotkey: {bad}"), now + 2.2));
        }

        // Kick off a settings push/pull; a push saves first so the repo gets what you see.
        // With autosync on, the save itself may have started the push - don't stack a second.
        if let Some(op) = sync_op {
            if op == sync::Op::Push {
                self.save_settings(ctx);
            }
            if !self.sync_busy {
                self.sync_busy = true;
                sync::spawn(op, self.cfg.sync.repo.trim().to_owned(), &self.sync_slot, ctx.clone());
            }
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
            self.settings_tab = false;
        } else if discard {
            if let Some(b) = self.settings.baseline.take() {
                self.cfg = b;
            }
            self.settings.profile_loaded = None; // buffers reload from the restored config
            self.reapply_appearance(ctx);
            self.reregister_hotkey();
            self.reapply_font(ctx);
            self.settings.confirm_close = false;
            self.settings_open = false;
            self.settings_tab = false;
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
    fn bright_filter_defaults_to_the_slot_being_set() {
        // Following the system, a pick can only land in the current appearance's slot -
        // pre-filter to that brightness. Manual mode starts on All.
        assert_eq!(default_bright_filter(true, true), BrightFilter::Light);
        assert_eq!(default_bright_filter(true, false), BrightFilter::Dark);
        assert_eq!(default_bright_filter(false, true), BrightFilter::All);
        assert_eq!(default_bright_filter(false, false), BrightFilter::All);
    }

    #[test]
    fn slot_bright_filter_pre_filters_each_theme_dropdown() {
        // The Appearance dropdowns open pre-filtered to the slot they set: Light theme ->
        // light schemes, Dark theme -> dark; the manual Theme dropdown stays unfiltered.
        assert_eq!(slot_bright_filter(SchemeSlot::Light), BrightFilter::Light);
        assert_eq!(slot_bright_filter(SchemeSlot::Dark), BrightFilter::Dark);
        assert_eq!(slot_bright_filter(SchemeSlot::Fixed), BrightFilter::All);
    }

    #[test]
    fn bright_filter_partitions_light_and_dark() {
        // (filter, scheme is dark) -> shown?
        let cases = [
            (BrightFilter::All, true, true),
            (BrightFilter::All, false, true),
            (BrightFilter::Dark, true, true),
            (BrightFilter::Dark, false, false),
            (BrightFilter::Light, true, false),
            (BrightFilter::Light, false, true),
        ];
        for (f, dark, want) in cases {
            assert_eq!(bright_allows(f, dark), want, "{f:?} dark={dark}");
        }
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

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn args_split_handles_quotes_and_escapes() {
        let cases: [(&str, &[&str]); 10] = [
            ("", &[]),
            ("   ", &[]),
            ("-l -i", &["-l", "-i"]),
            ("  -c   'echo hi'  ", &["-c", "echo hi"]), // single quotes group
            ("-c \"echo hi there\"", &["-c", "echo hi there"]), // double quotes group
            ("a\\ b c", &["a b", "c"]),                 // backslash-escaped space
            ("\"a\\\"b\"", &["a\"b"]),                  // escaped quote inside double quotes
            ("'don\"t'", &["don\"t"]),                  // double quote literal in single quotes
            ("\"\" x", &["", "x"]),                     // empty quoted arg survives
            ("\"unterminated rest", &["unterminated rest"]), // lenient: runs to the end
        ];
        for (input, want) in cases {
            assert_eq!(split_args(input), v(want), "input {input:?}");
        }
    }

    #[test]
    fn args_join_round_trips_through_split() {
        let cases: [&[&str]; 5] = [
            &[],
            &["-l", "-i"],
            &["-c", "echo hi there"],
            &["with\"quote", "with\\slash", ""],
            &["plain", "two words", "'single'"],
        ];
        for args in cases {
            let args = v(args);
            assert_eq!(split_args(&join_args(&args)), args, "{args:?}");
        }
        // Plain args stay readable (no gratuitous quoting).
        assert_eq!(join_args(&v(&["-l", "-i"])), "-l -i");
        assert_eq!(join_args(&v(&["a b"])), "\"a b\"");
    }

    #[test]
    fn env_rows_drop_blank_keys_and_last_write_wins() {
        let rows = vec![
            ("A".to_string(), "1".to_string()),
            (String::new(), "ignored".to_string()), // half-typed row: dropped
            ("  ".to_string(), "ignored".to_string()),
            (" B ".to_string(), "2".to_string()), // key trimmed
            ("A".to_string(), "3".to_string()),   // duplicate: last write wins
        ];
        let map = env_rows_to_map(&rows);
        assert_eq!(map.len(), 2);
        assert_eq!(map["A"], "3");
        assert_eq!(map["B"], "2");
    }

    #[test]
    fn highlight_moves_and_wraps() {
        // (cur, len, down) -> next
        let cases = [
            (None, 5, true, Some(0)),     // Down from nothing: top row
            (None, 5, false, Some(4)),    // Up from nothing: bottom row
            (Some(0), 5, true, Some(1)),  // plain step
            (Some(4), 5, true, Some(0)),  // wraps at the end
            (Some(0), 5, false, Some(4)), // wraps at the top
            (Some(2), 0, true, None),     // empty list clears the highlight
            (None, 0, false, None),
        ];
        for (cur, len, down, want) in cases {
            assert_eq!(move_highlight(cur, len, down), want, "cur={cur:?} len={len} down={down}");
        }
    }

    #[test]
    fn commit_falls_back_to_the_top_match() {
        assert_eq!(commit_index(None, 3), Some(0)); // no highlight: Enter picks the top match
        assert_eq!(commit_index(Some(2), 3), Some(2));
        assert_eq!(commit_index(Some(9), 3), Some(2)); // the filter shrank: clamp
        assert_eq!(commit_index(None, 0), None);
        assert_eq!(commit_index(Some(1), 0), None);
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
        // hints, quake fx rows, profiles list + editor, hotkey rows) build real frames
        // without panicking.
        let ctx = egui::Context::default();
        let mut cfg = config::Config::default();
        cfg.profiles.push(config::Profile {
            name: "work".into(),
            shell: Some("/bin/zsh".into()),
            args: vec!["-l".into()],
            cwd: Some("~/Git".into()),
            env: [("AWS_PROFILE".to_string(), "work".to_string())].into(),
            color: Some("#61afef".into()),
        });
        cfg.hotkeys.clear = "garbage!!".into(); // invalid chord renders red, must not panic
        let mut st = SettingsState::new();
        st.profile_sel = Some(0); // exercise the inline editor incl. env rows
        for _ in 0..2 {
            run_frame(&ctx, vec![], |ui| {
                let _fx = appearance_section(ui, &mut cfg, &mut st);
                let _fx = quake_section(ui, &mut cfg);
                terminal_section(ui, &mut cfg);
                let _fx = profiles_section(ui, &mut cfg, &mut st);
                let _fx = hotkeys_section(ui, &mut cfg);
            });
        }
        assert!(st.dropdown_open.is_none());
        // The editor buffers loaded from the selected profile.
        assert_eq!(st.profile_loaded, Some(0));

        // Window mode renders the same section (with its quake-only rows greyed) without panic,
        // and reports no live side effects (mode/hotkey/height unchanged by a static render).
        cfg.quake.mode = "window".into();
        for _ in 0..2 {
            run_frame(&ctx, vec![], |ui| {
                let fx = quake_section(ui, &mut cfg);
                assert!(!fx.mode_changed && !fx.hotkey_commit && !fx.height_changed);
                let _fx = appearance_section(ui, &mut cfg, &mut st); // greys unfocused-opacity
            });
        }
        assert!(config::is_window_mode(&cfg));
        assert_eq!(st.profile_args, "-l");
        assert_eq!(st.profile_env, vec![("AWS_PROFILE".to_string(), "work".to_string())]);
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// Click open the "probe" scheme dropdown (locate + pointer press/release) and run one
    /// settle frame so the popup's search field holds focus with its arrow-claiming event
    /// filter installed (arrows then move the HIGHLIGHT, never egui focus).
    fn open_scheme_dropdown(ctx: &egui::Context, st: &mut SettingsState, value: &mut String) {
        run_frame(ctx, vec![], |ui| {
            let _ = scheme_dropdown(ui, st, "probe", SchemeSlot::Fixed, value);
        });
        let center = egui::pos2(30.0, 22.0); // inside the 200x28 button at the panel origin
        run_frame(ctx, vec![egui::Event::PointerMoved(center)], |ui| {
            let _ = scheme_dropdown(ui, st, "probe", SchemeSlot::Fixed, value);
        });
        for pressed in [true, false] {
            run_frame(
                ctx,
                vec![egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                }],
                |ui| {
                    let _ = scheme_dropdown(ui, st, "probe", SchemeSlot::Fixed, value);
                },
            );
        }
        assert!(st.dropdown_open.is_some(), "clicking the dropdown must open its popup");
        run_frame(ctx, vec![], |ui| {
            let _ = scheme_dropdown(ui, st, "probe", SchemeSlot::Fixed, value);
        });
    }

    #[test]
    fn scheme_dropdown_opens_filters_and_picks() {
        let ctx = egui::Context::default();
        let mut st = SettingsState::new();
        let mut value = "one-half-dark".to_string();
        open_scheme_dropdown(&ctx, &mut st, &mut value);
        // Type a filter that matches exactly one scheme, then Esc closes the popup.
        st.dropdown_filter = "tokyo-night".into();
        run_frame(&ctx, vec![], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        run_frame(&ctx, vec![key(egui::Key::Escape)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert!(st.dropdown_open.is_none(), "Esc must close the popup");
    }

    #[test]
    fn dropdown_arrows_move_the_highlight_and_enter_commits() {
        let ctx = egui::Context::default();
        let mut st = SettingsState::new();
        let mut value = "one-half-dark".to_string();
        open_scheme_dropdown(&ctx, &mut st, &mut value);
        run_frame(&ctx, vec![key(egui::Key::ArrowDown)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert_eq!(st.dropdown_hl, Some(0), "ArrowDown from nothing highlights the top row");
        run_frame(&ctx, vec![key(egui::Key::ArrowDown)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert_eq!(st.dropdown_hl, Some(1));
        assert!(st.hover_preview.is_some(), "the keyboard highlight must feed the preview card");
        // Enter commits the highlighted row (index 1 of the unfiltered list) and closes.
        let all = themes::all_schemes();
        let expect = all[filter_schemes(all, "")[1]].0.clone();
        run_frame(&ctx, vec![key(egui::Key::Enter)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert_eq!(value, expect, "Enter must commit the highlighted scheme");
        assert!(st.dropdown_open.is_none(), "a commit closes the popup");
        assert_eq!(st.dropdown_hl, None, "the highlight resets on close");
    }

    #[test]
    fn dropdown_typing_keeps_filtering_and_esc_closes_without_committing() {
        let ctx = egui::Context::default();
        let mut st = SettingsState::new();
        let mut value = "one-half-dark".to_string();
        open_scheme_dropdown(&ctx, &mut st, &mut value);
        // Arrows move the highlight WITHOUT stealing focus from the search field: typing
        // right after two ArrowDown presses still lands in the filter.
        for _ in 0..2 {
            run_frame(&ctx, vec![key(egui::Key::ArrowDown)], |ui| {
                let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
            });
        }
        run_frame(&ctx, vec![egui::Event::Text("tokyo".into())], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert_eq!(st.dropdown_filter, "tokyo", "typing must keep filtering after arrows");
        assert_eq!(st.dropdown_hl, None, "a new query restarts at the top match");
        run_frame(&ctx, vec![key(egui::Key::Escape)], |ui| {
            let _ = scheme_dropdown(ui, &mut st, "probe", SchemeSlot::Fixed, &mut value);
        });
        assert!(st.dropdown_open.is_none(), "Esc must close the popup");
        assert_eq!(value, "one-half-dark", "Esc must never commit a pick");
    }
}
