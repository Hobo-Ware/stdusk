//! egui terminal rendering: the grid painter, tab chip, toasts and overlays, and the pty input
//! collector. Reads the process-wide theme in `colors`.

use stdusk_core::agent_restore::PaneNotice;
use stdusk_core::colors;
use stdusk_core::progress::Progress;
use stdusk_core::terminal::{CmdState, GridSnap, PtyTerm};

/// Phosphor icon codepoints (font vendored in assets/Phosphor.ttf, MIT).
pub mod icons {
    pub const PLUS: &str = "\u{E3D4}";
    pub(crate) const MINUS: &str = "\u{E32A}";
    pub const X: &str = "\u{E4F6}";
    pub const GEAR: &str = "\u{E270}";
    pub const APP_WINDOW: &str = "\u{E5DA}";
    pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub const CARET_UP: &str = "\u{E13C}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const TEXT_AA: &str = "\u{E6EE}"; // case sensitivity
    pub const ASTERISK: &str = "\u{E0AA}"; // regex
    pub const BRACKETS_SQUARE: &str = "\u{E85E}"; // whole word
    // Settings-view section + row icons.
    pub const PALETTE: &str = "\u{E6C8}"; // Appearance
    pub const SWATCHES: &str = "\u{E5B8}"; // Color scheme
    pub const TERMINAL_WINDOW: &str = "\u{EAE8}"; // Terminal
    pub const LIGHTNING: &str = "\u{E2DE}"; // Quake
    pub const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}"; // Session
    pub const INFO: &str = "\u{E2CE}"; // About
    pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}"; // open-externally rows
    pub const FOLDER: &str = "\u{E24A}"; // open config folder
    pub const CHECK: &str = "\u{E182}"; // active scheme mark
    pub(crate) const PUSH_PIN: &str = "\u{E3E2}"; // pinned-tab marker
    // 0.5.0 (all cmap-verified against the vendored font; codepoints from the official CSS).
    pub const IDENTIFICATION_BADGE: &str = "\u{E6F6}"; // Profiles section
    pub const KEYBOARD: &str = "\u{E2D8}"; // Hotkeys section
    pub const PLAY: &str = "\u{E3D0}"; // launch profile
    pub const COPY: &str = "\u{E1CA}"; // duplicate profile
    pub const TRASH: &str = "\u{E4A6}"; // delete profile / env row
}

// ---- pure helpers (no egui state; unit-tested below) ----

/// Truncate to `max` chars with an ellipsis; returns (shown, was_truncated).
pub fn ellipsize(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    (format!("{head}…"), true)
}

/// Map a pointer offset from the grid origin to a grid point: (buffer line, column, right-half).
/// `top_line` is the buffer line of viewport row 0 (negative while scrolled into history).
pub fn pos_to_cell(
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

/// Fill fraction (0..=1) for a tab progress bar, or `None` to hide it. Determinate states
/// fill by percentage; error/indeterminate fill fully (color carries the meaning).
pub fn progress_fraction(p: Progress) -> Option<f32> {
    match p {
        Progress::None => None,
        Progress::Normal(v) | Progress::Paused(v) => Some(f32::from(v) / 100.0),
        Progress::Error(_) | Progress::Indeterminate => Some(1.0),
    }
}

/// Toast opacity (0..=1): full until `remaining_s` drops below `fade_window_s`, then linear out.
pub fn toast_alpha(remaining_s: f64, fade_window_s: f64) -> f32 {
    (remaining_s / fade_window_s).clamp(0.0, 1.0) as f32
}

/// Symbol "ligatures": common code sequences drawn as one Unicode glyph spanning their cells.
/// Visual only - the grid, selection, and copy all keep the real characters. (True OpenType
/// ligatures need text shaping, which egui doesn't do.) Longest match wins; conservative table
/// so glyph coverage is safe in the bundled fonts.
const LIGATURES: &[(&str, char)] = &[
    ("...", '\u{2026}'), // …
    ("->", '\u{2192}'),  // →
    ("=>", '\u{21d2}'),  // ⇒
    ("!=", '\u{2260}'),  // ≠
    (">=", '\u{2265}'),  // ≥
    ("<=", '\u{2264}'),  // ≤
];

/// Find ligature spans in one row: `(start col, char len, glyph)`, non-overlapping, longest-first.
pub(crate) fn ligature_spans(row: &[char]) -> Vec<(usize, usize, char)> {
    let mut out = Vec::new();
    let mut i = 0;
    'outer: while i < row.len() {
        for (pat, glyph) in LIGATURES {
            let pl = pat.chars().count();
            if i + pl <= row.len() && row[i..i + pl].iter().copied().eq(pat.chars()) {
                out.push((i, pl, *glyph));
                i += pl;
                continue 'outer;
            }
        }
        i += 1;
    }
    out
}

/// Replace every ligature sequence in `s` with its single glyph (settings preview text; the
/// grid uses `ligature_spans` per-cell). Table order is longest-first so "..." wins over "..".
pub fn apply_ligatures(s: &str) -> String {
    let mut out = s.to_owned();
    for (pat, glyph) in LIGATURES {
        out = out.replace(pat, &glyph.to_string());
    }
    out
}

/// Terminal cursor shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Beam,
}

/// Parse the config `cursor` string; unknown values fall back to `Block` (the default).
pub fn cursor_style(s: &str) -> CursorStyle {
    match s.to_ascii_lowercase().as_str() {
        "underline" | "under" => CursorStyle::Underline,
        "beam" | "bar" | "ibeam" => CursorStyle::Beam,
        _ => CursorStyle::Block,
    }
}

/// Apply window opacity to a fill color (straight alpha).
pub fn tint(c: egui::Color32, opacity: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (opacity.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

pub fn apply_theme(ctx: &egui::Context) {
    // Start from the matching egui base (light widgets on light themes - a dark base under a
    // light theme yields dark-on-dark controls) then derive every widget fill from the theme.
    let mut v = if colors::is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = colors::bg();
    v.window_fill = colors::elevated();
    v.window_stroke = egui::Stroke::new(1.0, colors::border());
    v.extreme_bg_color = colors::bg();
    v.override_text_color = Some(colors::fg());
    v.selection.bg_fill = colors::selection();
    v.selection.stroke = egui::Stroke::new(1.0, colors::accent());
    v.hyperlink_color = colors::accent();
    let base = colors::elevated();
    // Hover/active fills must be a real step past `elevated` - menus/popups draw ON elevated,
    // so `hover()` (elevated at partial alpha) gave menu rows no visible hover feedback.
    let strong = colors::hover_elevated();
    for (w, fill) in [
        (&mut v.widgets.noninteractive, colors::bg()),
        (&mut v.widgets.inactive, base),
        (&mut v.widgets.hovered, strong),
        (&mut v.widgets.active, strong),
        (&mut v.widgets.open, base),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.fg_stroke = egui::Stroke::new(1.0, colors::fg());
        w.bg_stroke = egui::Stroke::new(1.0, colors::border());
    }
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, colors::dim());
    // egui keeps one style per OS theme and `set_visuals` only writes the ACTIVE one. When the OS
    // appearance differs from the configured scheme, the other style stayed at egui's defaults and
    // popups/menus painted light. Write both and pin the active theme to the scheme's polarity.
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.set_theme(if colors::is_dark() { egui::Theme::Dark } else { egui::Theme::Light });
}

/// Paint a small pill-shaped status toast centered near the bottom edge. A long message wraps
/// inside the window, and a newline starts a new line. `fade` in 0..1 scales opacity so the
/// message dissolves as it expires.
pub fn draw_toast(ui: &egui::Ui, msg: &str, fade: f32) {
    let a = |c: egui::Color32, base: u8| {
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (f32::from(base) * fade) as u8)
    };
    let area = ui.max_rect();
    let font = egui::FontId::proportional(13.0);
    let pad = egui::vec2(14.0, 8.0);
    let wrap_width = (area.width() - 2.0 * (pad.x + 16.0)).max(120.0);
    let galley = ui.painter().layout(msg.to_owned(), font, colors::fg(), wrap_width);
    let size = galley.size() + pad * 2.0;
    let center = egui::pos2(area.center().x, area.bottom() - 34.0 - (size.y - 32.0).max(0.0) / 2.0);
    let rect = egui::Rect::from_center_size(center, size);
    let p = ui.painter();
    p.rect_filled(rect, 8.0, a(colors::elevated(), 235));
    p.rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, a(colors::border(), 255)),
        egui::StrokeKind::Inside,
    );
    p.galley_with_override_text_color(rect.min + pad, galley, a(colors::fg(), 255));
}

/// The one line a pane's restore notice says.
pub fn pane_notice_text(notice: PaneNotice) -> String {
    match notice {
        PaneNotice::NotResumed { kind, reason } => {
            format!("{} session not resumed: {}", kind.display_name(), reason.text())
        }
        PaneNotice::CrashHint { kind } => {
            format!(
                "{} session ended by a crash signal. Press Enter to reopen it.",
                kind.display_name()
            )
        }
    }
}

/// Paint a dim notice pill along the bottom edge of a pane, inside `pane`. This is stdusk's own
/// chrome, drawn over the grid. It never goes into the pty or the terminal grid. A text that is
/// too long for a narrow pane wraps. Returns the pill's rectangle.
pub fn draw_pane_notice(ui: &egui::Ui, pane: egui::Rect, text: &str) -> egui::Rect {
    let font = egui::FontId::proportional(12.0);
    let (pad, margin) = (egui::vec2(10.0, 5.0), 8.0);
    let wrap_width = (pane.width() - 2.0 * (margin + pad.x)).max(60.0);
    let galley = ui.painter().layout(text.to_owned(), font, colors::dim(), wrap_width);
    let size = galley.size() + pad * 2.0;
    let pill = egui::Rect::from_min_size(
        egui::pos2(pane.center().x - size.x / 2.0, pane.bottom() - margin - size.y),
        size,
    );
    let p = ui.painter_at(pane);
    p.rect_filled(pill, 6.0, colors::elevated().gamma_multiply(0.92));
    p.rect_stroke(pill, 6.0, egui::Stroke::new(1.0, colors::border()), egui::StrokeKind::Inside);
    p.galley(pill.min + pad, galley, colors::dim());
    pill
}

/// Dim scrim + centered "[process exited]" banner over a dead pane (`on_exit = "keep"` or the
/// restart crash-loop fallback). Enter / click handling lives at the pane response.
pub fn draw_exit_overlay(ui: &egui::Ui, rect: egui::Rect, code: i32) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(110));
    p.text(
        rect.center() - egui::vec2(0.0, 11.0),
        egui::Align2::CENTER_CENTER,
        format!("[process exited: {code}]"),
        egui::FontId::proportional(14.0),
        colors::fg(),
    );
    p.text(
        rect.center() + egui::vec2(0.0, 11.0),
        egui::Align2::CENTER_CENTER,
        "press Enter or click to restart",
        egui::FontId::proportional(11.5),
        colors::dim(),
    );
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
fn paint_mini_layout(p: &egui::Painter, box_rect: egui::Rect, rects: &[egui::Rect], active: bool) {
    let inner = box_rect.shrink(1.0);
    let col = if active { colors::fg() } else { colors::dim() };
    for r in rects {
        let cell = egui::Rect::from_min_max(
            inner.min + egui::vec2(r.min.x * inner.width(), r.min.y * inner.height()),
            inner.min + egui::vec2(r.max.x * inner.width(), r.max.y * inner.height()),
        );
        p.rect_filled(cell, 1.0, col);
    }
}

/// Vendored Simple Icons SVG (CC0-1.0, https://simpleicons.org) for a CLI's brand mark, or
/// `None` when no official slug exists: OpenAI's icon was removed from Simple Icons upstream
/// (so Codex keeps the letter chip) and aider never had one.
fn cli_icon_svg(cli: stdusk_core::procwatch::Cli) -> Option<&'static [u8]> {
    use stdusk_core::procwatch::Cli;
    Some(match cli {
        Cli::Claude => include_bytes!("../assets/icons/anthropic.svg"),
        Cli::Gemini => include_bytes!("../assets/icons/googlegemini.svg"),
        Cli::Copilot => include_bytes!("../assets/icons/githubcopilot.svg"),
        Cli::Ollama => include_bytes!("../assets/icons/ollama.svg"),
        Cli::Cursor => include_bytes!("../assets/icons/cursor.svg"),
        Cli::Codex | Cli::Aider => return None,
    })
}

/// Rasterize a solid-fill brand SVG to a WHITE glyph (RGB forced white, alpha from the render)
/// so the badge paints it with `tint` = brand color. `None` on parse/render failure.
fn rasterize_white(svg: &[u8], px: u32) -> Option<egui::ColorImage> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let scale = px as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity((px * px * 4) as usize);
    for p in pixmap.pixels() {
        rgba.extend_from_slice(&[255, 255, 255, p.alpha()]);
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([px as usize, px as usize], &rgba))
}

/// The brand icon texture for a CLI badge, rasterized once per (cli, px) and cached in egui's
/// per-context memory (so headless tests and the app never share GPU handles). `None` = no
/// icon / rasterization failed - the caller falls back to the letter chip.
fn cli_icon_texture(
    ctx: &egui::Context,
    cli: stdusk_core::procwatch::Cli,
    px: u32,
) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("cli-icon", cli.label(), px));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return cached;
    }
    let tex = cli_icon_svg(cli).and_then(|svg| rasterize_white(svg, px)).map(|img| {
        ctx.load_texture(format!("cli-icon-{}", cli.label()), img, egui::TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

/// A compact brand-colored chip marking a known AI CLI running in the tab: a small rounded
/// square in the CLI's brand color with its initial letter. The fallback badge for CLIs
/// without a vendored brand icon (see `cli_icon_svg`).
fn paint_cli_chip(p: &egui::Painter, rect: egui::Rect, cli: stdusk_core::procwatch::Cli) {
    let col = cli.color();
    // Contrast ink by the brand color's luminance so the initial reads on light and dark chips.
    let lum = 0.299 * f32::from(col.r()) + 0.587 * f32::from(col.g()) + 0.114 * f32::from(col.b());
    let ink = if lum > 150.0 { egui::Color32::from_rgb(24, 24, 24) } else { egui::Color32::WHITE };
    let initial = cli.label().chars().next().unwrap_or('?').to_ascii_uppercase();
    p.rect_filled(rect, 4.0, col);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(10.0),
        ink,
    );
}

/// Tab geometry: shared with the tab bar so the row height / spacer math can't rot.
pub const TAB_H: f32 = 34.0; // full tab-bar strip height; tabs fill it (flush underline)

/// Extra tab-strip height in macOS window mode. 0 = the OS traffic lights keep their default
/// position (top-left). Repositioning them to center on the tab row was reverted (an absolute-Y
/// move in the wrong coord space flung them off-screen). Kept as a tunable const - `tabs.rs`
/// honors a non-zero value if the window-mode strip ever wants a taller look.
pub const WINDOW_TAB_H_EXTRA: f32 = 0.0;

pub const TAB_FIXED_W: f32 = 200.0; // fixed-mode standard width (Tabby-like)

pub const SETTINGS_TAB_W: f32 = 110.0; // right-pinned Settings tab (fixed, spacer math)

pub const TAB_MIN_W: f32 = 60.0; // fixed-mode floor when the bar overflows

pub const TAB_PAD_X: f32 = 10.0;

pub const TAB_SLOT_W: f32 = 18.0; // trailing slot: close-x on hover, CLI badge while one runs

const TAB_GAP: f32 = 6.0;

const TAB_MINI_W: f32 = 15.0; // split-layout preview glyph

/// Flat Tabby-style tab: strip bg (lifted `active_tab` fill when active), optional per-tab colored underline
/// flush with the strip's bottom edge, a split-layout preview glyph, and progress as a thin bar
/// on the TOP edge. The TRAILING (right) slot exists only while relevant: it shows the CLI
/// brand badge while an AI CLI runs in the tab, and hovering the tab swaps it for the close-x
/// (close wins while hovered), so the two can never overlap. With neither state the slot is
/// gone and the title gets the full width - no space is permanently reserved.
/// `width` = `Some(px)` for fixed mode (title ellipsized to fit), `None` sizes to content.
/// Returns (click+drag response, close-clicked). `layout` = `Pane::miniature()` leaf rects
/// (glyph shown when >1). `tab_id` seeds the interact id so drag-reorder tracking survives
/// index swaps (ui.md). `idx = None` drops the number prefix (the Settings tab - it has no
/// Cmd+N binding to advertise).
#[allow(clippy::too_many_lines)] // one widget, mostly geometry + paint
pub fn draw_tab(
    ui: &mut egui::Ui,
    idx: Option<usize>,
    tab_id: u64,
    title: &str,
    active: bool,
    pinned: bool,
    color: Option<egui::Color32>,
    progress: Progress,
    cmd: CmdState,
    layout: &[egui::Rect],
    cli: Option<stdusk_core::procwatch::Cli>,
    width: Option<f32>,
) -> (egui::Response, bool) {
    let font = egui::FontId::monospace(12.0);
    let char_w = ui.painter().layout_no_wrap("0".into(), font.clone(), colors::fg()).size().x;
    let prefix = idx.map_or_else(String::new, |i| format!("{i}  "));
    let mini_w = if layout.len() > 1 { TAB_MINI_W + TAB_GAP } else { 0.0 };
    let pin_w = if pinned { 14.0 } else { 0.0 };
    // Trailing-slot presence: hover state comes from LAST frame's response (stored below) -
    // this frame's rect isn't allocated yet, and a predicted rect would oscillate in dynamic
    // width mode. One frame of lag; egui repaints on pointer movement anyway.
    let tab_iid = ui.id().with(("tab", tab_id));
    let hover_id = tab_iid.with("hovered");
    let hovered_last = ui.ctx().data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
    let slot_shown = hovered_last || cli.is_some();
    let slot_w = if slot_shown { TAB_SLOT_W + TAB_GAP } else { 0.0 };
    let fixed_chrome = TAB_PAD_X * 2.0 + slot_w + mini_w + pin_w;
    let (shown, truncated, tab_w) = if let Some(w) = width {
        // Ellipsize to whatever fits the fixed width (monospace: chars scale linearly).
        let chars = ((w - fixed_chrome) / char_w) as usize;
        let (shown, truncated) = ellipsize(title, chars.saturating_sub(prefix.len()).max(1));
        (shown, truncated, w)
    } else {
        let (shown, truncated) = ellipsize(title, 14);
        let text_w = (prefix.chars().count() + shown.chars().count()) as f32 * char_w;
        (shown, truncated, fixed_chrome + text_w)
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(tab_w, TAB_H), egui::Sense::hover());
    let p = ui.painter();
    if active {
        p.rect_filled(
            rect,
            egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, // top-rounded tab shape
            colors::active_tab(),
        );
    }
    let mut x = rect.left() + TAB_PAD_X;
    if layout.len() > 1 {
        let mini = egui::Rect::from_min_size(
            egui::pos2(x, rect.center().y - TAB_MINI_W / 2.0),
            egui::vec2(TAB_MINI_W, TAB_MINI_W),
        );
        paint_mini_layout(p, mini, layout, active);
        x += TAB_MINI_W + TAB_GAP;
    }
    let fg = if active { colors::fg() } else { colors::dim() };
    p.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{prefix}{shown}"),
        font,
        fg,
    );
    // Trailing slot at the right edge; the pinned push-pin sits just left of it (or takes the
    // edge itself while the slot is absent). The index stays on the left as-is.
    let slot = egui::Rect::from_center_size(
        egui::pos2(rect.right() - TAB_PAD_X - TAB_SLOT_W / 2.0, rect.center().y),
        egui::vec2(TAB_SLOT_W, TAB_SLOT_W),
    );
    if pinned {
        let pin_x = if slot_shown { slot.left() - TAB_GAP } else { rect.right() - TAB_PAD_X };
        p.text(
            egui::pos2(pin_x, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            icons::PUSH_PIN,
            egui::FontId::proportional(11.0),
            fg,
        );
    }
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
    // Per-tab color underline - only when the user set a color. The tab fills the strip's full
    // height, so `rect.bottom()` IS the strip's bottom edge: the underline sits flush against
    // the terminal area (Tabby-style), painted on the deco layer so it draws over the hairline.
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
    //
    // ONE widget senses click AND drag (activate / double-click rename / context menu / reorder).
    // Never layer a separate drag-only interact on top: egui's hit test drops the click hit
    // entirely when the topmost widget under the pointer senses only drags (hit_test.rs), which
    // is exactly the bug that killed all tab clicks in 0.2.2. Id comes from the stable tab id,
    // not the loop index, so egui keeps tracking the same drag across reorder swaps.
    let mut tab_resp = ui.interact(rect, tab_iid, egui::Sense::click_and_drag());
    if truncated {
        tab_resp = tab_resp.on_hover_text(title);
    }
    // Trailing slot: the close-x only while the pointer is over the tab (replacing the CLI
    // badge, so they can't overlap - close wins while hovered); the badge - or nothing -
    // otherwise. Use `contains_pointer` (true across the whole tab rect, incl. the x) rather
    // than `hovered` so moving onto the x doesn't drop the tab's hover state and make the x
    // flicker. Stored for next frame's slot-presence decision (see the top of the fn).
    let hovered = tab_resp.contains_pointer();
    ui.ctx().data_mut(|d| d.insert_temp(hover_id, hovered));
    let mut close = false;
    if hovered {
        let xr = ui
            .interact(slot, ui.id().with(("close", tab_id)), egui::Sense::click())
            .on_hover_text("Close (Cmd+W)");
        let xh = xr.hovered();
        if xh {
            // hover() reads on the bar fill, but vanishes over the active tab's elevated fill.
            let fill = if active { colors::hover_elevated() } else { colors::hover() };
            ui.painter().rect_filled(slot, 5.0, fill);
        }
        ui.painter().text(
            slot.center(),
            egui::Align2::CENTER_CENTER,
            icons::X,
            egui::FontId::proportional(13.0),
            if xh { colors::fg() } else { colors::dim() },
        );
        if xr.clicked() {
            close = true;
        }
    } else if let Some(cli) = cli {
        let badge = egui::Rect::from_center_size(slot.center(), egui::vec2(14.0, 14.0));
        // Real brand mark where Simple Icons has one, tinted the brand color; rasterized at 2x
        // the on-screen pixel size so it stays crisp on retina. Letter chip otherwise.
        let px = (14.0 * ui.ctx().pixels_per_point() * 2.0).round() as u32;
        match cli_icon_texture(ui.ctx(), cli, px) {
            Some(tex) => {
                ui.painter().image(
                    tex.id(),
                    badge,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    cli.color(),
                );
            }
            None => paint_cli_chip(ui.painter(), badge, cli),
        }
    }
    (tab_resp, close)
}

/// Translate this frame's key/text events into bytes for the pty.
pub fn collect_input(
    ui: &egui::Ui,
    alt_is_meta: bool,
    intercept_ctrl_c: bool,
    app_cursor: bool,
) -> Vec<u8> {
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
                    if let Some(bytes) =
                        crate::keys::key_to_bytes(*key, *modifiers, alt_is_meta, app_cursor)
                    {
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
/// Per-pane render options (bundled - render_grid was accumulating bool params).
// Independent render toggles, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub struct GridStyle {
    pub cursor: CursorStyle,
    pub dimmed: bool,      // unfocused pane in a split: fade content
    pub link_active: bool, // links react to hover/click this frame
    pub blink: bool,       // blink the cursor (focused pane only)
    pub ligatures: bool,   // draw symbol ligatures
    pub min_contrast: f32, // nudge cell fg to this WCAG ratio vs its bg; <=1 = off
    pub app_mouse: bool,   // the app gets pointer reports: no local selection or links
}

pub fn render_grid(
    ui: &mut egui::Ui,
    id_src: &[stdusk_core::pane::Side],
    rect: egui::Rect,
    term: &PtyTerm,
    snap: &GridSnap,
    cw: f32,
    ch: f32,
    font: &egui::FontId,
    bold_font: Option<&egui::FontId>, // real bold face, when the family registered one
    style: GridStyle,
    search_marks: &[stdusk_core::search::Match], // all find-bar matches (empty when the bar is closed)
) -> egui::Response {
    let GridStyle { cursor, dimmed, link_active, blink, ligatures, min_contrast, app_mouse } =
        style;
    // BOLD cells switch to the real bold face when one exists; metrics stay derived from the
    // regular face (a bold glyph may run a hair wider - Tabby-equivalent tradeoff).
    let cell_font = |bold: bool| if bold { bold_font.unwrap_or(font) } else { font };
    let resp = ui.interact(rect, egui::Id::new(id_src), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let origin = rect.min;
    let hit = |p: egui::Pos2| {
        pos_to_cell(p.x - origin.x, p.y - origin.y, cw, ch, snap.cols, snap.rows, snap.top_line)
    };

    // Clickable links: when active (enabled + configured modifier held, or no modifier), underline
    // the link under the pointer and open it on click. Kept off the selection path so plain drags
    // still select text.
    let mut link_underlines: Vec<(f32, f32, f32)> = Vec::new(); // (x0, x1, y) per wrapped row
    if link_active
        && !app_mouse
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && rect.contains(p)
    {
        let row = (((p.y - origin.y) / ch) as usize).min(snap.rows.saturating_sub(1));
        let col = (((p.x - origin.x) / cw) as usize).min(snap.cols.saturating_sub(1));
        let rows: Vec<String> = (0..snap.rows)
            .map(|r| (0..snap.cols).map(|c| snap.cells[r * snap.cols + c].c).collect())
            .collect();
        if let Some(link) = stdusk_core::links::link_at(&rows, row, col) {
            link_underlines.extend(link.spans.iter().map(|s| {
                (
                    origin.x + s.col as f32 * cw,
                    origin.x + (s.col + s.len) as f32 * cw,
                    origin.y + (s.row as f32 + 1.0) * ch - 1.5,
                )
            }));
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                stdusk_core::links::open(&link.text, link.kind, term.cwd().as_deref());
            }
        }
    }
    if !app_mouse {
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
    }

    // Unfocused panes fade their CONTENT (Tabby-style) by scaling its alpha down, leaving the
    // blank cells transparent at the window's global opacity - so the glass stays uniform and
    // only the content recedes (no opaque scrim that would change see-through).
    let fade = |c: egui::Color32| if dimmed { c.gamma_multiply(0.5) } else { c };
    // Minimum contrast (terminal.minimum_contrast): nudge each glyph's fg toward black/white
    // until it meets the WCAG ratio against its effective bg. Applied before the dim fade so
    // an unfocused pane keeps the same relative treatment; free when off (<= 1).
    // SGR 2 (faint) fades toward the effective bg AFTER the floor, so hint/ghost text stays
    // visibly dimmer than normal text instead of being pulled back to full contrast.
    let mut contrast = colors::ContrastMemo::new(min_contrast);
    let mut ink = |cell: &stdusk_core::terminal::CellSnap| {
        let eff_bg = cell.bg.unwrap_or_else(colors::bg);
        let fg = if min_contrast > 1.0 { contrast.ensure(cell.fg, eff_bg) } else { cell.fg };
        if cell.dim { colors::faint(fg, eff_bg) } else { fg }
    };
    for r in 0..snap.rows {
        // Symbol ligatures: cells covered by a span skip their own glyph; the span's single
        // glyph spans the covered cells (bg/selection stay per-cell). Spans are COLLECTED here
        // but painted after the cell loop - painting them first let the per-cell bg rects cover
        // them, which made every ligature invisible on any cell with an explicit background
        // (diff views, highlighted regions).
        let mut lig_skip = vec![false; if ligatures { snap.cols } else { 0 }];
        let mut lig_spans = Vec::new();
        if ligatures {
            let row_chars: Vec<char> =
                (0..snap.cols).map(|c| snap.cells[r * snap.cols + c].c).collect();
            for (start, len, glyph) in ligature_spans(&row_chars) {
                for f in &mut lig_skip[start..start + len] {
                    *f = true;
                }
                lig_spans.push((start, len, glyph));
            }
        }
        // Index-driven on purpose: c addresses three parallel sources (cells, pixel x, lig_skip).
        #[allow(clippy::needless_range_loop)]
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
            if ligatures && lig_skip[c] {
                continue; // glyph drawn by the span pass below
            }
            if cell.c != ' ' && cell.c != '\0' {
                let fg = fade(ink(cell));
                let f = cell_font(cell.bold);
                if cell.wide {
                    // A wide glyph (CJK/emoji) owns this cell AND the spacer after it: draw it
                    // horizontally centered across the two, top-aligned like its neighbors.
                    painter.text(
                        egui::pos2(pos.x + cw, pos.y),
                        egui::Align2::CENTER_TOP,
                        cell.c,
                        f.clone(),
                        fg,
                    );
                } else {
                    painter.text(pos, egui::Align2::LEFT_TOP, cell.c, f.clone(), fg);
                }
            }
        }
        // Ligature glyphs last, so the row's bg / selection fills can't paint over them.
        for (start, len, glyph) in lig_spans {
            let cell = &snap.cells[r * snap.cols + start];
            let span = egui::Rect::from_min_size(
                origin + egui::vec2(start as f32 * cw, r as f32 * ch),
                egui::vec2(len as f32 * cw, ch),
            );
            painter.text(
                span.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                cell_font(cell.bold).clone(),
                fade(ink(cell)),
            );
        }
    }

    // All-match search overlay: a dim accent wash over every visible hit (the CURRENT match
    // stands out - it also carries the brighter selection fill via the per-cell path above).
    for (row, col, len) in
        stdusk_core::search::visible_matches(search_marks, snap.top_line, snap.rows, snap.cols)
    {
        painter.rect_filled(
            egui::Rect::from_min_size(
                origin + egui::vec2(col as f32 * cw, row as f32 * ch),
                egui::vec2(len as f32 * cw, ch),
            ),
            2.0,
            fade(colors::search_match()),
        );
    }

    // Underline the hovered (command-held) link, one segment per row it wraps over.
    for (x0, x1, y) in link_underlines {
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
                let under = &snap.cells[cr * snap.cols + cc];
                // A wide glyph's block cursor covers both of its cells (xterm-style).
                let w = if under.wide { cw * 2.0 } else { cw };
                painter.rect_filled(egui::Rect::from_min_size(cpos, egui::vec2(w, ch)), 0.0, cur);
                // Redraw the glyph under the block in the background color so it stays legible.
                if under.c != ' ' && under.c != '\0' {
                    let (pos, anchor) = if under.wide {
                        (egui::pos2(cpos.x + cw, cpos.y), egui::Align2::CENTER_TOP)
                    } else {
                        (cpos, egui::Align2::LEFT_TOP)
                    };
                    painter.text(
                        pos,
                        anchor,
                        under.c,
                        cell_font(under.bold).clone(),
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
    id_src: &[stdusk_core::pane::Side],
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

/// Cursor blink phase: on for the first half of each ~1.06s cycle (xterm-ish cadence).
pub fn blink_on(time: f64) -> bool {
    time.rem_euclid(1.06) < 0.53
}

#[cfg(test)]
mod tests {
    use super::*;
    use stdusk_core::repaint::Repaint;

    #[test]
    fn theme_visuals_apply_whatever_the_os_theme_is() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let want = colors::elevated();
        for os in [egui::Theme::Dark, egui::Theme::Light] {
            ctx.set_theme(os);
            assert_eq!(ctx.global_style().visuals.window_fill, want, "style for {os:?}");
        }
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
    fn ligature_spans_detect_sequences() {
        let row: Vec<char> = "a -> b => c".chars().collect();
        assert_eq!(ligature_spans(&row), vec![(2, 2, '\u{2192}'), (7, 2, '\u{21d2}')]);
        // Longest-first: "..." is one span, not overlapping shorter matches; != >= <= work.
        let row: Vec<char> = "x... != >= <=".chars().collect();
        assert_eq!(
            ligature_spans(&row),
            vec![(1, 3, '\u{2026}'), (5, 2, '\u{2260}'), (8, 2, '\u{2265}'), (11, 2, '\u{2264}')]
        );
        // No false positives in plain text.
        let row: Vec<char> = "plain text - no = pairs".chars().collect();
        assert_eq!(ligature_spans(&row), []);
    }

    #[test]
    fn a_pane_notice_names_the_agent_and_the_reason() {
        use stdusk_core::agent_restore::SkipReason;
        use stdusk_core::agents::AgentKind::{Claude, Codex};
        let cases = [
            (
                PaneNotice::NotResumed { kind: Claude, reason: SkipReason::TranscriptMissing },
                "Claude session not resumed: transcript missing",
            ),
            (
                PaneNotice::NotResumed { kind: Codex, reason: SkipReason::DirectoryMissing },
                "Codex session not resumed: directory missing",
            ),
            (
                PaneNotice::NotResumed { kind: Claude, reason: SkipReason::Duplicate },
                "Claude session not resumed: duplicate of another pane",
            ),
            (
                PaneNotice::NotResumed { kind: Codex, reason: SkipReason::NotHandedOver },
                "Codex session not resumed: shell not handed over",
            ),
            (
                PaneNotice::NotResumed { kind: Claude, reason: SkipReason::ShellBusy },
                "Claude session not resumed: shell busy at start",
            ),
            (
                PaneNotice::CrashHint { kind: Claude },
                "Claude session ended by a crash signal. Press Enter to reopen it.",
            ),
            (
                PaneNotice::CrashHint { kind: Codex },
                "Codex session ended by a crash signal. Press Enter to reopen it.",
            ),
        ];
        for (notice, want) in cases {
            assert_eq!(pane_notice_text(notice), want);
        }
    }

    #[test]
    fn a_pane_notice_stays_inside_its_pane_even_when_the_pane_is_narrow() {
        let text =
            pane_notice_text(PaneNotice::CrashHint { kind: stdusk_core::agents::AgentKind::Codex });
        for width in [600.0, 220.0] {
            let pane = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(width, 200.0));
            let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 300.0));
            let ctx = egui::Context::default();
            let raw = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let mut pill = egui::Rect::NOTHING;
            let _ = ctx.run_ui(raw, |ui| pill = draw_pane_notice(ui, pane, &text));
            assert!(pane.contains_rect(pill), "width {width}: {pill:?} sticks out of {pane:?}");
            assert!(pill.bottom() > pane.bottom() - 20.0, "the pill sits at the bottom edge");
        }
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

    #[test]
    fn apply_ligatures_replaces_sequences() {
        assert_eq!(apply_ligatures("a -> b => c"), "a \u{2192} b \u{21d2} c");
        assert_eq!(apply_ligatures("x... != y"), "x\u{2026} \u{2260} y");
        assert_eq!(apply_ligatures("plain"), "plain"); // untouched
    }

    #[test]
    fn vendored_cli_icons_rasterize_as_white_glyphs() {
        use stdusk_core::procwatch::Cli;
        // Every CLI with a vendored Simple Icons SVG must parse + render to a non-empty white
        // glyph (a broken asset would silently fall back to the letter chip).
        for cli in [Cli::Claude, Cli::Gemini, Cli::Copilot, Cli::Ollama, Cli::Cursor] {
            let svg = cli_icon_svg(cli).unwrap_or_else(|| panic!("{cli:?} must have an icon"));
            let img = rasterize_white(svg, 28).unwrap_or_else(|| panic!("{cli:?} must render"));
            assert_eq!(img.size, [28, 28]);
            // RGB forced white so egui tint = brand color works; Color32 stores premultiplied
            // alpha, so only fully-opaque pixels read back 255 (edges scale with their alpha).
            let solid: Vec<_> = img.pixels.iter().filter(|p| p.a() == 255).collect();
            assert!(!solid.is_empty(), "{cli:?} rendered no solid pixels");
            assert!(solid.iter().all(|p| p.r() == 255 && p.g() == 255 && p.b() == 255));
        }
        // No Simple Icons slug (OpenAI removed upstream; aider never had one): chip fallback.
        assert!(cli_icon_svg(Cli::Codex).is_none());
        assert!(cli_icon_svg(Cli::Aider).is_none());
    }

    // ---- headless egui frames: the tab-bar hit-test regressions (egui 0.35 drops the click
    // hit when a drag-only widget sits on top - so the tab senses click+drag as ONE widget) ----

    #[test]
    fn blink_phase_toggles() {
        assert!(blink_on(0.0));
        assert!(blink_on(0.52));
        assert!(!blink_on(0.54));
        assert!(blink_on(1.07)); // next cycle
    }

    #[test]
    fn a_pane_notice_draws_each_frame_until_a_key_reaches_the_pane() {
        // The frames run as the workspace runs them: read the keys, send them to the pane, and
        // draw the pane's notice over the grid.
        use stdusk_core::agents::AgentKind;
        let mut opts = stdusk_core::test_support::spawn_opts("/bin/sh", &["-c", "cat"]);
        opts.agent_tracking = Some(stdusk_core::agent_pane::AgentSetup {
            pending_input: None,
            fallback_delay: std::time::Duration::from_secs(60),
        });
        let mut term = PtyTerm::spawn(60, 5, Repaint::none(), &opts);
        term.set_notice(PaneNotice::CrashHint { kind: AgentKind::Codex });
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 400.0));
        let frame = |term: &mut PtyTerm, events: Vec<egui::Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(screen),
                events,
                focused: true,
                ..Default::default()
            };
            let (mut drawn, mut keys) = (false, Vec::new());
            let _ = ctx.run_ui(raw, |ui| {
                keys = collect_input(ui, false, false, false);
                if let Some(notice) = term.notice() {
                    let text = pane_notice_text(notice);
                    let pill = draw_pane_notice(ui, screen, &text);
                    drawn = pill.width() > 0.0 && screen.contains_rect(pill);
                }
            });
            if !keys.is_empty() {
                term.send(&keys);
            }
            (drawn, keys)
        };
        assert_eq!(frame(&mut term, vec![]), (true, vec![]), "drawn on the first frame");
        assert_eq!(frame(&mut term, vec![]), (true, vec![]), "and on every frame after");
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let (drawn, keys) = frame(&mut term, vec![enter]);
        assert_eq!(keys, b"\r", "the key still reaches the pty");
        assert!(drawn, "the frame that sent the key drew before the pane changed");
        assert!(!frame(&mut term, vec![]).0, "gone from the next frame on");
    }
}
