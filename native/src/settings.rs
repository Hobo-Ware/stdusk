//! Settings: a full-height, Tabby-style preferences view swapped into the central area while
//! `settings_open` (a real panel, not an `egui::Window` - windows don't render in the
//! screenshot harness). Left: section nav. Right: a roomy content pane; the Color scheme
//! section browses every embedded scheme with live palette previews and a fake-shell preview
//! card. Controls edit `self.cfg` in place (live-apply); nothing persists until Save writes
//! the TOML back to the config file.

use eframe::egui;

use crate::colors::{self, Theme};
use crate::ui::{self, icons};
use crate::{Stdusk, config, themes};

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
}

impl SettingsState {
    pub(crate) fn new() -> Self {
        Self {
            section: Section::Appearance,
            filter: String::new(),
            hover_preview: None,
            scroll_to_active: false,
        }
    }

    /// Switch section; entering the scheme browser jumps its list to the active scheme.
    pub(crate) fn open_section(&mut self, section: Section) {
        self.section = section;
        self.scroll_to_active = section == Section::ColorScheme;
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

/// One settings row: title (+ optional dim description) left, control right. Call inside `rows`.
fn row(ui: &mut egui::Ui, name: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LABEL_W);
            ui.label(egui::RichText::new(name).size(14.0).color(colors::fg()));
            if !desc.is_empty() {
                ui.label(egui::RichText::new(desc).size(11.0).color(colors::dim()));
            }
        });
        ui.add_space(16.0);
        control(ui);
    });
}

/// A fraction slider displayed as a percentage ("85%").
fn pct_slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.add(
        egui::Slider::new(value, range)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .custom_parser(|s| {
                s.trim().trim_end_matches('%').parse::<f64>().ok().map(|v| v / 100.0)
            }),
    );
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

// ---- color-scheme section drawing ----

/// The fake-shell preview card: `PREVIEW_LINES` painted on the scheme's own bg, plus a
/// cursor block after the prompt.
fn preview_card(ui: &mut egui::Ui, theme: &Theme) {
    let width = ui.available_width().min(CONTENT_MAX_W);
    let font = egui::FontId::monospace(12.5);
    let line_h = 19.0;
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
            let galley = painter.layout_no_wrap(run.text.to_owned(), font.clone(), color);
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
            // Cursor block right after the typed command.
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x + 2.0, y), egui::vec2(7.0, 15.0)),
                1.0,
                theme.cursor,
            );
        }
    }
}

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

// ---- plain sections (pure config edits) ----

fn appearance_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    title(ui, "Appearance");
    let a = &mut cfg.appearance;
    rows(ui, |ui| {
        row(ui, "Follow system appearance", "Switch themes with the macOS light/dark mode", |ui| {
            ui::toggle_switch(ui, &mut a.follow_system);
        });
        if a.follow_system {
            row(ui, "Light theme", "Applied while macOS is light", |ui| {
                ui::text_field(ui, &mut a.theme_light, "scheme name", 180.0, colors::fg());
            });
            row(ui, "Dark theme", "Applied while macOS is dark", |ui| {
                ui::text_field(ui, &mut a.theme_dark, "scheme name", 180.0, colors::fg());
            });
        } else {
            row(ui, "Theme", "Pick visually in the Color scheme section", |ui| {
                ui::text_field(ui, &mut a.theme, "scheme name", 180.0, colors::fg());
            });
        }
        row(ui, "Opacity", "Window background transparency", |ui| {
            pct_slider(ui, &mut a.opacity, 0.4..=1.0);
        });
        row(ui, "Font size", "", |ui| {
            ui.add(egui::Slider::new(&mut a.font_size, 9.0..=24.0).fixed_decimals(0).suffix(" pt"));
        });
    });
}

fn terminal_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    title(ui, "Terminal");
    let t = &mut cfg.terminal;

    subheading(ui, "Behavior");
    rows(ui, |ui| {
        row(ui, "Scrollback lines", "History kept per pane", |ui| {
            ui.add(egui::DragValue::new(&mut t.scrollback_lines).range(100..=1_000_000));
        });
        row(ui, "Shell integration", "OSC 133 command marks for done/failed state", |ui| {
            ui::toggle_switch(ui, &mut t.shell_integration);
        });
        row(ui, "Detect progress", "Track % output as a tab progress bar", |ui| {
            ui::toggle_switch(ui, &mut t.detect_progress);
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

    subheading(ui, "Rendering");
    rows(ui, |ui| {
        row(ui, "Cursor style", "", |ui| {
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
        row(ui, "Bold in bright colors", "Draw bold text with the bright ANSI palette", |ui| {
            ui::toggle_switch(ui, &mut t.bold_bright);
        });
        row(ui, "Ligatures", "Draw -> => != >= <= as single glyphs", |ui| {
            ui::toggle_switch(ui, &mut t.ligatures);
        });
    });
}

fn quake_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    title(ui, "Quake");
    let q = &mut cfg.quake;
    rows(ui, |ui| {
        row(ui, "Global hotkey", "Restart to apply", |ui| {
            ui::text_field(ui, &mut q.hotkey, "e.g. Ctrl+Grave, F13", 180.0, colors::fg());
        });
        row(ui, "Window height", "Fraction of the screen the window drops down", |ui| {
            pct_slider(ui, &mut q.height_pct, 0.2..=0.9);
        });
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
}

fn session_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
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
        preview_card(ui, &self.settings.hover_preview.unwrap_or(active_theme));
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

    /// Sticky footer: Save / Close / Revert, right-aligned. Returns true when Close was hit.
    fn settings_footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut close = false;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui::action_button(ui, "Save", true).clicked()
                && let Some(p) = config::ensure_and_path()
                && std::fs::write(p, config::config_to_toml(&self.cfg)).is_ok()
            {
                let now = ctx.input(|i| i.time);
                self.toast = Some(("Saved".into(), now + 1.4));
            }
            if ui::action_button(ui, "Close", false).clicked() {
                close = true;
            }
            if ui::action_button(ui, "Revert", false).clicked() {
                self.cfg = config::Config::load();
                let system_light =
                    matches!(ctx.input(|i| i.raw.system_theme), Some(egui::Theme::Light));
                let want = resolved_theme_name(&self.cfg.appearance, system_light);
                colors::set(colors::by_name(&want));
                ui::apply_theme(ctx);
                self.theme_name = want;
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
        let opacity = self.cfg.appearance.opacity;
        let mut close = false;
        // Sampled BEFORE the panels run: a focused text field (scheme search, hotkey) consumes
        // Esc to drop focus; only the NEXT Esc should close the view.
        let field_focused = ctx.memory(|m| m.focused().is_some());

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
                // Cap the column width and ease it off the sidebar on wide windows (a hard-left
                // column next to a quake-wide dead zone reads unbalanced).
                let col_w = ui.available_width().min(CONTENT_MAX_W);
                let pad = ((ui.available_width() - col_w) / 2.0).clamp(0.0, 120.0);
                let full = ui.available_rect_before_wrap();
                let col = egui::Rect::from_min_size(
                    egui::pos2(full.left() + pad, full.top()),
                    egui::vec2(col_w, full.height()),
                );
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(col));
                let ui = &mut ui;
                ui.spacing_mut().slider_width = 190.0;
                match self.settings.section {
                    // The scheme browser manages its own scrolling (fixed head, scrolling list).
                    Section::ColorScheme => self.scheme_section(ui, ctx),
                    section => {
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            ui.set_max_width(CONTENT_MAX_W.min(ui.available_width()));
                            match section {
                                Section::Appearance => appearance_section(ui, &mut self.cfg),
                                Section::Terminal => terminal_section(ui, &mut self.cfg),
                                Section::Quake => quake_section(ui, &mut self.cfg),
                                Section::Session => session_section(ui, &mut self.cfg),
                                Section::About => about_section(ui),
                                Section::ColorScheme => unreachable!(),
                            }
                            ui.add_space(16.0);
                        });
                    }
                }
            });

        // Esc closes - but not while a hard modal (rename/paste/palette) or the find bar owns
        // it, and not on the press that just unfocused a text field.
        let modal_owns_esc = self.renaming.is_some()
            || !self.pending_pastes.is_empty()
            || self.palette.is_some()
            || self.search.is_some();
        if !modal_owns_esc && !field_focused && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.settings_open = false;
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
}
