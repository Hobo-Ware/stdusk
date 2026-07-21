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
    pub(crate) const MINUS: &str = "\u{E32A}";
    pub(crate) const X: &str = "\u{E4F6}";
    pub(crate) const GEAR: &str = "\u{E270}";
    pub(crate) const APP_WINDOW: &str = "\u{E5DA}";
    pub(crate) const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub(crate) const CARET_UP: &str = "\u{E13C}";
    pub(crate) const CARET_DOWN: &str = "\u{E136}";
    pub(crate) const TEXT_AA: &str = "\u{E6EE}"; // case sensitivity
    pub(crate) const ASTERISK: &str = "\u{E0AA}"; // regex
    pub(crate) const BRACKETS_SQUARE: &str = "\u{E85E}"; // whole word
    // Settings-view section + row icons.
    pub(crate) const PALETTE: &str = "\u{E6C8}"; // Appearance
    pub(crate) const SWATCHES: &str = "\u{E5B8}"; // Color scheme
    pub(crate) const TERMINAL_WINDOW: &str = "\u{EAE8}"; // Terminal
    pub(crate) const LIGHTNING: &str = "\u{E2DE}"; // Quake
    pub(crate) const CLOCK_COUNTER_CLOCKWISE: &str = "\u{E1A0}"; // Session
    pub(crate) const INFO: &str = "\u{E2CE}"; // About
    pub(crate) const ARROW_SQUARE_OUT: &str = "\u{E5DE}"; // open-externally rows
    pub(crate) const FOLDER: &str = "\u{E24A}"; // open config folder
    pub(crate) const CHECK: &str = "\u{E182}"; // active scheme mark
    pub(crate) const PUSH_PIN: &str = "\u{E3E2}"; // pinned-tab marker
    // 0.5.0 (all cmap-verified against the vendored font; codepoints from the official CSS).
    pub(crate) const IDENTIFICATION_BADGE: &str = "\u{E6F6}"; // Profiles section
    pub(crate) const KEYBOARD: &str = "\u{E2D8}"; // Hotkeys section
    pub(crate) const PLAY: &str = "\u{E3D0}"; // launch profile
    pub(crate) const COPY: &str = "\u{E1CA}"; // duplicate profile
    pub(crate) const TRASH: &str = "\u{E4A6}"; // delete profile / env row
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

/// Auto-title for an unrenamed tab: the shell's OSC 0/2 title (when dynamic titles are on
/// and it's non-empty) beats the cwd basename; `None` = leave the current title alone.
pub(crate) fn auto_title(dynamic: bool, osc: Option<&str>, cwd: Option<&str>) -> Option<String> {
    match osc {
        Some(t) if dynamic && !t.is_empty() => Some(t.to_string()),
        _ => cwd.map(basename),
    }
}

/// Commit a rename buffer: a trimmed non-empty name renames the tab; an empty or
/// whitespace-only entry means "un-rename" (`None`) - `auto_title` takes back over. Also
/// applied to session-restored titles so a persisted empty rename can't stick.
pub(crate) fn commit_rename(buf: &str) -> Option<String> {
    let t = buf.trim();
    (!t.is_empty()).then(|| t.to_string())
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

/// The window opacity for this frame: the configured base, times `unfocused_mult` while the
/// window is visible but unfocused AND hide-on-focus-loss is off (with it on, an unfocused
/// window is about to hide anyway).
pub(crate) fn effective_opacity(
    base: f32,
    unfocused_mult: f32,
    visible: bool,
    focused: bool,
    hide_on_focus_loss: bool,
) -> f32 {
    if visible && !focused && !hide_on_focus_loss {
        (base * unfocused_mult.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    } else {
        base
    }
}

/// The on-screen sliver (points) the hidden quake window keeps at the bottom edge. LOAD-BEARING:
/// a window with zero pixels on-screen (ordered out or parked fully off-screen) parks eframe's run
/// loop, so the summon hotkey can never reshow it (broke 3x - see `apply_visibility`). Hidden ==
/// parked with this sliver still visible + alpha 0, never fully off-screen. Extend cautiously.
pub(crate) const QUAKE_HIDE_SLIVER: f32 = 2.0;

/// The window's top-left y when hidden: parked so only `QUAKE_HIDE_SLIVER` points remain at the
/// bottom edge. INVARIANT: strictly less than `monitor_h` (the window stays ON-SCREEN) and the
/// visible sliver is exactly `QUAKE_HIDE_SLIVER` - the exact regression guard for the "hotkey can't
/// reshow after hide" bug. Pure so the invariant is testable without a window.
pub(crate) fn quake_hidden_y(monitor_h: f32) -> f32 {
    monitor_h - QUAKE_HIDE_SLIVER
}

/// The quake window's inner size when shown: full monitor width, `height_pct` of its height
/// (rounded). Pure companion to `quake_hidden_y` so the show/hide geometry is testable.
pub(crate) fn quake_shown_size(monitor_w: f32, monitor_h: f32, height_pct: f32) -> (f32, f32) {
    (monitor_w, (monitor_h * height_pct).round())
}

/// Window alpha for the quake show/hide: fully opaque when shown, fully transparent (NOT ordered
/// out) when hidden - alpha-0 hides the load-bearing sliver visually while the window keeps drawing
/// so the run loop stays warm. Pure seam for the invariant "hidden means alpha 0, never removed".
pub(crate) fn quake_alpha(visible: bool) -> f64 {
    if visible { 1.0 } else { 0.0 }
}

/// Always-on-top decision: a dropdown window with hide-on-focus-loss OFF is meant to stay put over
/// other apps, so it floats; every other case (hide-on-blur on, or window mode) is a Normal-level
/// window. Pure; mirrors the mode decisions in `config.rs`.
pub(crate) fn wants_always_on_top(window_mode: bool, hide_on_focus_loss: bool) -> bool {
    !window_mode && !hide_on_focus_loss
}

/// Whether a visible dropdown window should hide THIS frame because focus left it. Only fires once
/// it has actually held focus since showing (`was_focused`), on a real focus loss, when hide-on-blur
/// is enabled, AND only on a real app deactivation (`app_active` false) - a system panel (emoji /
/// character viewer) steals winit's window focus but keeps the app active, so gating on `app_active`
/// stops it dismissing quake. Pure so the whole trigger is table-testable off the render loop.
#[allow(clippy::fn_params_excessive_bools)] // independent focus/mode flags, table-tested below
pub(crate) fn should_hide_on_blur(
    visible: bool,
    was_focused: bool,
    focused: bool,
    hides_on_blur: bool,
    app_active: bool,
) -> bool {
    visible && was_focused && !focused && hides_on_blur && !app_active
}

/// Should keystrokes be kept away from the pty this frame? True while any modal text surface
/// owns the keyboard. The find bar counts only while its text field actually has focus - an
/// open-but-unfocused find bar must not silently swallow terminal input (that read as
/// "keys/backspace no longer reach the shell").
#[allow(clippy::fn_params_excessive_bools)] // independent modal states, table-tested below
pub(crate) fn pty_input_captured(
    search_field_focused: bool,
    renaming: bool,
    palette: bool,
    settings_open: bool,
    pending_pastes: bool,
    pending_close: bool,
) -> bool {
    search_field_focused || renaming || palette || settings_open || pending_pastes || pending_close
}

/// Cursor blink phase: on for the first half of each ~1.06s cycle (xterm-ish cadence).
pub(crate) fn blink_on(time: f64) -> bool {
    time.rem_euclid(1.06) < 0.53
}

/// Grid cell height: the measured glyph height plus the user's `appearance.line_padding`,
/// clamped to the settings slider's 0-8px range (a hand-edited config can hold anything).
pub(crate) fn padded_cell_height(glyph_h: f32, line_padding: f32) -> f32 {
    glyph_h + line_padding.clamp(0.0, 8.0)
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
pub(crate) fn apply_ligatures(s: &str) -> String {
    let mut out = s.to_owned();
    for (pat, glyph) in LIGATURES {
        out = out.replace(pat, &glyph.to_string());
    }
    out
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

/// Tab sizing mode (`appearance.tab_width`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabWidthMode {
    Fixed,
    Dynamic,
}

/// Parse the config `tab_width` string; unknown values fall back to `Fixed` (the default).
pub(crate) fn tab_width_mode(s: &str) -> TabWidthMode {
    match s.to_ascii_lowercase().as_str() {
        "dynamic" | "auto" => TabWidthMode::Dynamic,
        _ => TabWidthMode::Fixed,
    }
}

/// Equal per-tab width for fixed mode: an even share of `avail` (minus inter-tab spacing),
/// capped at the Tabby-like standard width and floored so tabs stay clickable on overflow.
pub(crate) fn fixed_tab_width(avail: f32, n: usize, spacing: f32) -> f32 {
    let share = (avail - spacing * (n.saturating_sub(1)) as f32) / (n.max(1)) as f32;
    share.clamp(TAB_MIN_W, TAB_FIXED_W)
}

/// `terminal.right_click` parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightClickMode {
    Menu,
    Paste,
    Clipboard,
}

/// Parse the config `right_click` string; unknown values fall back to `Menu` (the default).
pub(crate) fn right_click_mode(s: &str) -> RightClickMode {
    match s.to_ascii_lowercase().as_str() {
        "paste" => RightClickMode::Paste,
        "clipboard" => RightClickMode::Clipboard,
        _ => RightClickMode::Menu,
    }
}

/// What a right-click release does on a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightClickAction {
    Menu,
    Paste,
    Copy,
}

/// Tabby's exact right-click semantics (`baseTerminalTab` handleRightMouseUp): paste/clipboard
/// act only on a quick tap (held < 250ms); a hold falls back to the context menu. Clipboard
/// copies when a selection exists, else pastes.
pub(crate) fn right_click_action(
    mode: RightClickMode,
    held_secs: f64,
    has_selection: bool,
) -> RightClickAction {
    match mode {
        RightClickMode::Menu => RightClickAction::Menu,
        _ if held_secs >= 0.25 => RightClickAction::Menu,
        RightClickMode::Clipboard if has_selection => RightClickAction::Copy,
        RightClickMode::Paste | RightClickMode::Clipboard => RightClickAction::Paste,
    }
}

/// The tab toggle-last-tab jumps to: the previously active index, or 0 when it no longer
/// exists (Tabby `AppService.toggleLastTab` resets an out-of-range `lastTabIndex` to 0).
pub(crate) fn toggle_last_target(prev: usize, len: usize) -> usize {
    if prev >= len { 0 } else { prev }
}

/// Where a tracked index (the active tab) lands after moving the element at `from` to `to`
/// (a remove + insert, i.e. the pin/unpin reorder).
pub(crate) fn moved_index(from: usize, to: usize, tracked: usize) -> usize {
    if tracked == from {
        to
    } else if from < tracked && to >= tracked {
        tracked - 1
    } else if from > tracked && to <= tracked {
        tracked + 1
    } else {
        tracked
    }
}

/// The close-tab confirm prompt, or `None` to close silently. A pinned tab always asks; a tab
/// with live child processes states HOW MANY will be terminated (and names a few). `running` is
/// the shell's descendants across the tab's panes (`warn_on_close_running` gating is the caller's).
pub(crate) fn close_confirm_message(pinned: bool, running: &[String]) -> Option<String> {
    match (pinned, running.is_empty()) {
        (false, true) => None, // plain idle tab: close silently
        (true, true) => Some("This tab is pinned.".into()),
        (true, false) => Some(format!("This tab is pinned. {}.", terminate_phrase(running))),
        (false, false) => Some(format!("{} in this tab.", terminate_phrase(running))),
    }
}

/// "It will terminate N running process(es) (a, b, c, ...)." - the shared count+preview phrase for
/// the close/quit confirms. The name list is capped so a big claude tree doesn't overflow the pill.
pub(crate) fn terminate_phrase(running: &[String]) -> String {
    let n = running.len();
    let noun = if n == 1 { "process" } else { "processes" };
    format!("It will terminate {n} running {noun}{}", name_preview(running))
}

/// ` (a, b, c, ...)` for a non-empty list (first 3 names, `...` when more), else empty.
fn name_preview(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let head = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 3 { format!(" ({head}, ...)") } else { format!(" ({head})") }
}

/// The quit confirm body: how much a quit will kill. `procs` running child processes summed
/// across `tabs` tabs. Title ("Quit stdusk?") is drawn by the modal; this is the detail line.
pub(crate) fn quit_confirm_message(procs: usize, tabs: usize) -> String {
    let p_noun = if procs == 1 { "process" } else { "processes" };
    let t_noun = if tabs == 1 { "tab" } else { "tabs" };
    format!("This will terminate {procs} running {p_noun} across {tabs} {t_noun}.")
}

/// Whether a close/quit should stop to confirm: only when the feature is on AND there is actually
/// something running to kill (a bare-shell tab never nags). Pure seam for table tests.
pub(crate) fn should_confirm_running(enabled: bool, running_count: usize) -> bool {
    enabled && running_count > 0
}

/// Notify-on-activity step: `(fire a notification now?, notified-flag after)`. Viewing the tab
/// (active + window visible) re-arms; away, the FIRST output after arming fires exactly once
/// and stays quiet until the tab is viewed again (per-tab `enabled` is the menu toggle).
#[allow(clippy::fn_params_excessive_bools)] // independent per-tab states, table-tested below
pub(crate) fn activity_notification(
    enabled: bool,
    viewed: bool,
    notified: bool,
    output: bool,
) -> (bool, bool) {
    if viewed {
        return (false, false); // viewing re-arms; nothing fires for a tab you're watching
    }
    let fire = enabled && output && !notified;
    (fire, notified || fire)
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

/// "Label (Chord)" tooltip text, or just the label when the action is unbound.
pub(crate) fn shortcut_tip(label: &str, chord: &str) -> String {
    if chord.trim().is_empty() { label.to_string() } else { format!("{label} ({chord})") }
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
    ctx.set_visuals(v);
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

/// Dim scrim + centered "[process exited]" banner over a dead pane (`on_exit = "keep"` or the
/// restart crash-loop fallback). Enter / click handling lives at the pane response.
pub(crate) fn draw_exit_overlay(ui: &egui::Ui, rect: egui::Rect, code: i32) {
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
fn cli_icon_svg(cli: crate::procwatch::Cli) -> Option<&'static [u8]> {
    use crate::procwatch::Cli;
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
    cli: crate::procwatch::Cli,
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
fn paint_cli_chip(p: &egui::Painter, rect: egui::Rect, cli: crate::procwatch::Cli) {
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
pub(crate) const TAB_H: f32 = 34.0; // full tab-bar strip height; tabs fill it (flush underline)
/// Extra tab-strip height in macOS window mode. 0 = the OS traffic lights keep their default
/// position (top-left). Repositioning them to center on the tab row was reverted (an absolute-Y
/// move in the wrong coord space flung them off-screen). Kept as a tunable const - `tabs.rs`
/// honors a non-zero value if the window-mode strip ever wants a taller look.
#[cfg(target_os = "macos")]
pub(crate) const WINDOW_TAB_H_EXTRA: f32 = 0.0;
#[cfg(not(target_os = "macos"))]
pub(crate) const WINDOW_TAB_H_EXTRA: f32 = 0.0;
pub(crate) const TAB_FIXED_W: f32 = 200.0; // fixed-mode standard width (Tabby-like)
pub(crate) const SETTINGS_TAB_W: f32 = 110.0; // right-pinned Settings tab (fixed, spacer math)
const TAB_MIN_W: f32 = 60.0; // fixed-mode floor when the bar overflows
const TAB_PAD_X: f32 = 10.0;
const TAB_SLOT_W: f32 = 18.0; // trailing slot: close-x on hover, CLI badge while one runs
const TAB_GAP: f32 = 6.0;
const TAB_MINI_W: f32 = 15.0; // split-layout preview glyph

/// Flat Tabby-style tab: dark bg (elevated when active), optional per-tab colored underline
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
pub(crate) fn draw_tab(
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
    cli: Option<crate::procwatch::Cli>,
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
            colors::elevated(),
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
                    if let Some(bytes) = crate::keys::key_to_bytes(*key, *modifiers, alt_is_meta) {
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
pub(crate) struct GridStyle {
    pub(crate) cursor: CursorStyle,
    pub(crate) dimmed: bool,      // unfocused pane in a split: fade content
    pub(crate) link_active: bool, // links react to hover/click this frame
    pub(crate) blink: bool,       // blink the cursor (focused pane only)
    pub(crate) ligatures: bool,   // draw symbol ligatures
    pub(crate) min_contrast: f32, // nudge cell fg to this WCAG ratio vs its bg; <=1 = off
}

pub(crate) fn render_grid(
    ui: &mut egui::Ui,
    id_src: &[crate::pane::Side],
    rect: egui::Rect,
    term: &PtyTerm,
    snap: &GridSnap,
    cw: f32,
    ch: f32,
    font: &egui::FontId,
    bold_font: Option<&egui::FontId>, // real bold face, when the family registered one
    style: GridStyle,
    search_marks: &[crate::search::Match], // all find-bar matches (empty when the bar is closed)
) -> egui::Response {
    let GridStyle { cursor, dimmed, link_active, blink, ligatures, min_contrast } = style;
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
    // Minimum contrast (terminal.minimum_contrast): nudge each glyph's fg toward black/white
    // until it meets the WCAG ratio against its effective bg. Applied before the dim fade so
    // an unfocused pane keeps the same relative treatment; free when off (<= 1).
    let ink = |fg: egui::Color32, bg: Option<egui::Color32>| {
        if min_contrast > 1.0 {
            colors::ensure_contrast(fg, bg.unwrap_or_else(colors::bg), min_contrast)
        } else {
            fg
        }
    };
    for r in 0..snap.rows {
        // Symbol ligatures: cells covered by a span skip their own glyph; the span's single
        // glyph is drawn centered across the covered cells (bg/selection stay per-cell).
        let mut lig_skip = vec![false; if ligatures { snap.cols } else { 0 }];
        if ligatures {
            let row_chars: Vec<char> =
                (0..snap.cols).map(|c| snap.cells[r * snap.cols + c].c).collect();
            for (start, len, glyph) in ligature_spans(&row_chars) {
                for f in &mut lig_skip[start..start + len] {
                    *f = true;
                }
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
                    fade(ink(cell.fg, cell.bg)),
                );
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
                continue; // glyph drawn by the span above
            }
            if cell.c != ' ' && cell.c != '\0' {
                let fg = fade(ink(cell.fg, cell.bg));
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
    }

    // All-match search overlay: a dim accent wash over every visible hit (the CURRENT match
    // stands out - it also carries the brighter selection fill via the per-cell path above).
    for (row, col, len) in
        crate::search::visible_matches(search_marks, snap.top_line, snap.rows, snap.cols)
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
    fn auto_title_prefers_osc_then_cwd() {
        assert_eq!(auto_title(true, Some("vim"), Some("/tmp/x")), Some("vim".into()));
        assert_eq!(auto_title(false, Some("vim"), Some("/tmp/x")), Some("x".into())); // dynamic off
        assert_eq!(auto_title(true, Some(""), Some("/tmp/x")), Some("x".into())); // empty = reset
        assert_eq!(auto_title(true, None, Some("/tmp/x")), Some("x".into()));
        assert_eq!(auto_title(true, Some("vim"), None), Some("vim".into()));
        assert_eq!(auto_title(true, None, None), None); // nothing known: leave the title alone
    }

    #[test]
    fn empty_or_whitespace_rename_clears_to_auto_title() {
        // A cleared rename field must un-rename the tab (None), never leave an empty title;
        // auto_title then reasserts (OSC title > cwd basename).
        assert_eq!(commit_rename("build"), Some("build".into()));
        assert_eq!(commit_rename("  build  "), Some("build".into())); // stray spaces trimmed
        assert_eq!(commit_rename(""), None);
        assert_eq!(commit_rename("   "), None);
        assert_eq!(auto_title(true, Some("copilot"), Some("/tmp/x")), Some("copilot".into()));
        assert_eq!(auto_title(true, None, Some("/tmp/x")), Some("x".into()));
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
        assert!(ligature_spans(&row).is_empty());
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
    fn right_click_mode_parse() {
        assert_eq!(right_click_mode("menu"), RightClickMode::Menu);
        assert_eq!(right_click_mode("Paste"), RightClickMode::Paste);
        assert_eq!(right_click_mode("clipboard"), RightClickMode::Clipboard);
        assert_eq!(right_click_mode("nonsense"), RightClickMode::Menu); // fallback = default
        assert_eq!(right_click_mode(""), RightClickMode::Menu);
    }

    #[test]
    fn right_click_action_follows_tabby_250ms_rule() {
        use RightClickAction as A;
        use RightClickMode as M;
        // (mode, held_secs, has_selection) -> action; Tabby baseTerminalTab:656-676.
        let cases = [
            (M::Menu, 0.05, false, A::Menu),
            (M::Menu, 1.0, true, A::Menu),
            (M::Paste, 0.05, false, A::Paste),
            (M::Paste, 0.05, true, A::Paste), // paste ignores the selection
            (M::Paste, 0.25, false, A::Menu), // >= 250ms hold -> menu
            (M::Paste, 2.0, false, A::Menu),
            (M::Clipboard, 0.05, true, A::Copy),
            (M::Clipboard, 0.05, false, A::Paste), // no selection -> paste
            (M::Clipboard, 0.3, true, A::Menu),    // hold beats the selection
        ];
        for (mode, held, sel, want) in cases {
            assert_eq!(right_click_action(mode, held, sel), want, "{mode:?} {held} sel={sel}");
        }
    }

    #[test]
    fn toggle_last_falls_back_to_first_tab() {
        assert_eq!(toggle_last_target(2, 4), 2);
        assert_eq!(toggle_last_target(0, 4), 0);
        assert_eq!(toggle_last_target(4, 4), 0); // out of range (tab closed) -> first
        assert_eq!(toggle_last_target(0, 1), 0); // single tab: no-op
    }

    #[test]
    fn moved_index_tracks_active_across_pin_moves() {
        // (from, to, tracked) -> where the tracked index lands.
        let cases = [
            (2, 0, 2, 0), // the tracked tab itself moved
            (2, 0, 0, 1), // moved from behind to before: shifted right
            (2, 0, 1, 2),
            (0, 2, 1, 0), // moved from before to behind: shifted left
            (0, 2, 2, 1),
            (0, 1, 3, 3), // move entirely before the tracked tab: untouched
            (3, 4, 1, 1), // move entirely after: untouched
        ];
        for (from, to, tracked, want) in cases {
            assert_eq!(moved_index(from, to, tracked), want, "{from}->{to} track {tracked}");
        }
    }

    #[test]
    fn close_confirm_states_process_count_and_pin() {
        let none: &[String] = &[];
        assert_eq!(close_confirm_message(false, none), None); // plain idle tab: close silently
        assert_eq!(
            close_confirm_message(false, &["vim".into()]).as_deref(),
            Some("It will terminate 1 running process (vim) in this tab.")
        );
        assert_eq!(
            close_confirm_message(false, &["claude".into(), "node".into()]).as_deref(),
            Some("It will terminate 2 running processes (claude, node) in this tab.")
        );
        // Pinned always asks, even when idle...
        assert_eq!(close_confirm_message(true, none).as_deref(), Some("This tab is pinned."));
        // ...and the pin reason leads, the process count follows.
        assert_eq!(
            close_confirm_message(true, &["vim".into()]).as_deref(),
            Some("This tab is pinned. It will terminate 1 running process (vim).")
        );
    }

    #[test]
    fn name_preview_caps_at_three_names() {
        assert_eq!(name_preview(&[]), "");
        assert_eq!(name_preview(&["a".into()]), " (a)");
        assert_eq!(
            name_preview(&["a".into(), "b".into(), "c".into(), "d".into()]),
            " (a, b, c, ...)"
        );
    }

    #[test]
    fn quit_confirm_message_pluralizes() {
        assert_eq!(
            quit_confirm_message(1, 1),
            "This will terminate 1 running process across 1 tab."
        );
        assert_eq!(
            quit_confirm_message(5, 2),
            "This will terminate 5 running processes across 2 tabs."
        );
    }

    #[test]
    fn should_confirm_running_needs_both_flag_and_count() {
        assert!(should_confirm_running(true, 1));
        assert!(!should_confirm_running(true, 0)); // nothing running -> no nag
        assert!(!should_confirm_running(false, 3)); // feature off -> never
    }

    #[test]
    fn activity_notification_fires_once_and_rearms_on_view() {
        // (enabled, viewed, notified, output) -> (fire, notified after)
        let cases = [
            (true, false, false, true, (true, true)), // away + output: fire once
            (true, false, true, true, (false, true)), // already fired: stay quiet
            (true, false, false, false, (false, false)), // no output: armed, silent
            (true, true, true, true, (false, false)), // viewing re-arms (and never fires)
            (true, true, false, false, (false, false)),
            (false, false, false, true, (false, false)), // toggle off: never fires
            (false, false, true, true, (false, true)),   // off doesn't clear a stale flag
        ];
        for (enabled, viewed, notified, output, want) in cases {
            assert_eq!(
                activity_notification(enabled, viewed, notified, output),
                want,
                "enabled={enabled} viewed={viewed} notified={notified} output={output}"
            );
        }
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
    fn shortcut_tip_hides_unbound_chords() {
        assert_eq!(shortcut_tip("New tab", "Cmd+T"), "New tab (Cmd+T)");
        assert_eq!(shortcut_tip("New tab", ""), "New tab");
        assert_eq!(shortcut_tip("New tab", "  "), "New tab");
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
    fn tab_width_mode_parse() {
        assert_eq!(tab_width_mode("fixed"), TabWidthMode::Fixed);
        assert_eq!(tab_width_mode("Dynamic"), TabWidthMode::Dynamic);
        assert_eq!(tab_width_mode("auto"), TabWidthMode::Dynamic);
        assert_eq!(tab_width_mode("nonsense"), TabWidthMode::Fixed); // fallback = default
        assert_eq!(tab_width_mode(""), TabWidthMode::Fixed);
    }

    #[test]
    fn fixed_tab_width_shares_evenly_and_clamps() {
        // Plenty of room: capped at the standard width.
        assert_eq!(fixed_tab_width(1200.0, 3, 4.0), TAB_FIXED_W);
        // Overflow: an even share of the bar, minus inter-tab spacing.
        let w = fixed_tab_width(500.0, 4, 4.0);
        assert!((w - (500.0 - 12.0) / 4.0).abs() < 1e-4);
        // Severe overflow: floored so tabs stay clickable.
        assert_eq!(fixed_tab_width(100.0, 10, 4.0), TAB_MIN_W);
        // Degenerate inputs don't divide by zero.
        assert_eq!(fixed_tab_width(300.0, 0, 4.0), TAB_FIXED_W);
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
    fn padded_cell_height_adds_clamped_padding() {
        assert_eq!(padded_cell_height(16.0, 0.0), 16.0);
        assert_eq!(padded_cell_height(16.0, 3.0), 19.0);
        assert_eq!(padded_cell_height(16.0, 99.0), 24.0); // clamped to 8
        assert_eq!(padded_cell_height(16.0, -2.0), 16.0); // never shrinks the cell
    }

    #[test]
    fn apply_ligatures_replaces_sequences() {
        assert_eq!(apply_ligatures("a -> b => c"), "a \u{2192} b \u{21d2} c");
        assert_eq!(apply_ligatures("x... != y"), "x\u{2026} \u{2260} y");
        assert_eq!(apply_ligatures("plain"), "plain"); // untouched
    }

    #[test]
    fn effective_opacity_dims_only_visible_unfocused_no_hide() {
        // (visible, focused, hide_on_focus_loss) -> expected
        let cases = [
            ((true, false, false), 0.45), // the one dimming case: 0.9 * 0.5
            ((true, true, false), 0.9),   // focused: full
            ((true, false, true), 0.9),   // hide-on-blur mode: never dim
            ((false, false, false), 0.9), // hidden: leave alone
        ];
        for ((visible, focused, hide), want) in cases {
            let got = effective_opacity(0.9, 0.5, visible, focused, hide);
            assert!((got - want).abs() < 1e-6, "v={visible} f={focused} h={hide}: {got}");
        }
        // multiplier 1.0 = feature off
        assert_eq!(effective_opacity(0.9, 1.0, true, false, false), 0.9);
    }

    #[test]
    fn hidden_quake_window_stays_on_screen() {
        // The load-bearing invariant that broke 3x: hiding parks the window with a sliver still
        // on-screen (never fully off), or the run loop parks and the hotkey can't reshow it.
        for monitor_h in [600.0_f32, 900.0, 1080.0, 1440.0, 2160.0] {
            let y = quake_hidden_y(monitor_h);
            assert!(y < monitor_h, "hidden y must stay ON-SCREEN (y < monitor_h) at {monitor_h}");
            let sliver = monitor_h - y;
            assert!(sliver > 0.0, "a positive sliver must remain visible at {monitor_h}");
            assert!(
                (sliver - QUAKE_HIDE_SLIVER).abs() < 1e-6,
                "the on-screen sliver must be exactly QUAKE_HIDE_SLIVER at {monitor_h}"
            );
        }
    }

    #[test]
    fn quake_alpha_hides_by_transparency_not_removal() {
        // Shown = opaque, hidden = alpha 0 (still drawn, run loop warm) - NOT ordered out.
        assert_eq!(quake_alpha(true), 1.0);
        assert_eq!(quake_alpha(false), 0.0);
    }

    #[test]
    fn quake_shown_size_fills_width_and_height_fraction() {
        assert_eq!(quake_shown_size(1440.0, 900.0, 0.5), (1440.0, 450.0));
        assert_eq!(quake_shown_size(1920.0, 1080.0, 0.33), (1920.0, 356.0)); // 356.4 rounds down
        assert_eq!(quake_shown_size(1000.0, 800.0, 1.0), (1000.0, 800.0));
    }

    #[test]
    fn always_on_top_only_for_a_pinned_dropdown() {
        // (window_mode, hide_on_focus_loss) -> floats on top
        assert!(wants_always_on_top(false, false)); // dropdown, stays-put mode: float
        assert!(!wants_always_on_top(false, true)); // dropdown, hides on blur: Normal
        assert!(!wants_always_on_top(true, false)); // window mode: always Normal
        assert!(!wants_always_on_top(true, true));
    }

    #[test]
    fn hide_on_blur_fires_only_on_a_real_deactivation() {
        // The full trigger, incl. the app_is_active gate (emoji/character viewer keeps the app
        // active while stealing window focus, so it must NOT dismiss quake).
        // (visible, was_focused, focused, hides_on_blur, app_active) -> hides
        let cases = [
            ((true, true, false, true, false), true), // real deactivation: hide
            ((true, true, false, true, true), false), // system panel: app still active, stay
            ((true, false, false, true, false), false), // never held focus yet: don't vanish
            ((true, true, true, true, false), false), // still focused: stay
            ((true, true, false, false, false), false), // hide-on-blur off: stay
            ((false, true, false, true, false), false), // already hidden: no-op
        ];
        for ((visible, was, focused, hob, active), want) in cases {
            assert_eq!(
                should_hide_on_blur(visible, was, focused, hob, active),
                want,
                "v={visible} was={was} f={focused} hob={hob} active={active}"
            );
        }
    }

    #[test]
    fn pty_capture_requires_a_keyboard_owner() {
        // Nothing open: keys flow to the shell.
        assert!(!pty_input_captured(false, false, false, false, false, false));
        // An open-but-unfocused find bar must NOT capture (the "backspace never reaches the
        // shell" regression); a focused one must.
        assert!(pty_input_captured(true, false, false, false, false, false));
        // Every modal state captures on its own.
        assert!(pty_input_captured(false, true, false, false, false, false)); // rename
        assert!(pty_input_captured(false, false, true, false, false, false)); // palette
        assert!(pty_input_captured(false, false, false, true, false, false)); // settings
        assert!(pty_input_captured(false, false, false, false, true, false)); // paste confirm
        assert!(pty_input_captured(false, false, false, false, false, true)); // close confirm
    }

    #[test]
    fn vendored_cli_icons_rasterize_as_white_glyphs() {
        use crate::procwatch::Cli;
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

    struct TabFrameOut {
        rects: Vec<egui::Rect>,
        clicked: Option<usize>,
        double: Option<usize>,
        closed: Option<usize>,
        dragged: Option<usize>,
        keys: Vec<u8>,
    }

    /// One frame of a minimal real tab bar (two `draw_tab`s + reorder drag sense) above a
    /// focused grid running `collect_input` - the exact structure of the app's render loop.
    fn tab_frame(ctx: &egui::Context, events: Vec<egui::Event>) -> TabFrameOut {
        tab_frame_with(ctx, events, [None, None])
    }

    /// `tab_frame` with per-tab CLI badges (the trailing slot's badge/close-x swap tests).
    fn tab_frame_with(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        clis: [Option<crate::procwatch::Cli>; 2],
    ) -> TabFrameOut {
        let mut out = TabFrameOut {
            rects: Vec::new(),
            clicked: None,
            double: None,
            closed: None,
            dragged: None,
            keys: Vec::new(),
        };
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            egui::Panel::top("tabbar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, cli) in clis.into_iter().enumerate() {
                        let (resp, close) = draw_tab(
                            ui,
                            Some(i + 1),
                            i as u64, // stable id
                            "tab",
                            i == 0,
                            false,
                            None,
                            Progress::None,
                            CmdState::Idle,
                            &[],
                            cli,
                            None,
                        );
                        if close {
                            out.closed = Some(i);
                        } else if resp.double_clicked() {
                            out.double = Some(i);
                        } else if resp.clicked() {
                            out.clicked = Some(i);
                        }
                        if resp.dragged() && ui.input(|inp| inp.pointer.is_decidedly_dragging()) {
                            out.dragged = Some(i);
                        }
                        out.rects.push(resp.rect);
                    }
                });
            });
            egui::CentralPanel::default().show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let g = ui.interact(rect, egui::Id::new("grid"), egui::Sense::click_and_drag());
                g.request_focus();
                out.keys = collect_input(ui, false, false);
            });
        });
        out
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    /// Center of a tab's TRAILING slot (close-x on hover / CLI badge) for its current rect.
    fn slot_pos(r: egui::Rect) -> egui::Pos2 {
        egui::pos2(r.right() - TAB_PAD_X - TAB_SLOT_W / 2.0, r.center().y)
    }

    /// Hover tab `i` (its center), then run one settle frame: the trailing slot appears the
    /// frame AFTER hover is stored, growing a dynamic-width tab - return the settled rects.
    fn hover_tab(ctx: &egui::Context, i: usize) -> Vec<egui::Rect> {
        let warm = tab_frame(ctx, vec![]);
        tab_frame(ctx, vec![egui::Event::PointerMoved(warm.rects[i].center())]);
        tab_frame(ctx, vec![]).rects
    }

    #[test]
    fn debug_geometry_dump() {
        let ctx = egui::Context::default();
        let out = tab_frame(&ctx, vec![]);
        println!("tab rects: {:?}", out.rects);
    }

    #[test]
    fn tab_click_activates_despite_drag_sense() {
        // Regression: the drag-reorder sense layered over tabs swallowed every click.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let p = warm.rects[1].center();
        tab_frame(&ctx, vec![egui::Event::PointerMoved(p)]);
        tab_frame(&ctx, vec![press(p, true)]);
        let up = tab_frame(&ctx, vec![press(p, false)]);
        assert_eq!(up.clicked, Some(1), "plain click must activate the tab");
        assert_eq!(up.dragged, None, "a plain click must not read as a drag");
    }

    #[test]
    fn tab_drag_reorders_without_clicking() {
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let p = warm.rects[1].center();
        tab_frame(&ctx, vec![egui::Event::PointerMoved(p)]);
        tab_frame(&ctx, vec![press(p, true)]);
        let moved = tab_frame(&ctx, vec![egui::Event::PointerMoved(p + egui::vec2(40.0, 0.0))]);
        assert_eq!(moved.dragged, Some(1), "moving past the threshold must report a drag");
        let up = tab_frame(&ctx, vec![press(p + egui::vec2(40.0, 0.0), false)]);
        assert_eq!(up.clicked, None, "a decided drag must not also click");
    }

    #[test]
    fn close_x_still_wins_its_click() {
        // The x is registered after the tab, so it stays on top for clicks (LEDGER fix).
        // The x lives in the hover-only TRAILING slot now, so the pointer must be over the
        // tab BEFORE the click - exactly what a real mouse does.
        let ctx = egui::Context::default();
        let x = slot_pos(hover_tab(&ctx, 0)[0]);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let up = tab_frame(&ctx, vec![press(x, false)]);
        assert_eq!(up.closed, Some(0), "the close-x must win its own click");
        assert_eq!(up.clicked, None);
    }

    #[test]
    fn close_x_closes_an_unfocused_tab() {
        // The x shows on hover for EVERY tab (not just the active one), and clicking it
        // must close - not merely focus - the unfocused tab.
        let ctx = egui::Context::default();
        let x = slot_pos(hover_tab(&ctx, 1)[1]);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let up = tab_frame(&ctx, vec![press(x, false)]);
        assert_eq!(up.closed, Some(1), "the x must close the unfocused tab");
        assert_eq!(up.clicked, None, "the click must not fall through and focus it");
    }

    #[test]
    fn close_x_replaces_the_badge_while_hovered() {
        // A tab with a running AI CLI shows the brand badge in the trailing slot; hovering
        // swaps it for the close-x, and the click must CLOSE (close wins while hovered).
        let ctx = egui::Context::default();
        let clis = [Some(crate::procwatch::Cli::Claude), None];
        let warm = tab_frame_with(&ctx, vec![], clis);
        // Badge-only state (no hover): the slot is already reserved by the running CLI.
        let x = slot_pos(warm.rects[0]);
        tab_frame_with(&ctx, vec![egui::Event::PointerMoved(x)], clis);
        tab_frame_with(&ctx, vec![press(x, true)], clis);
        let up = tab_frame_with(&ctx, vec![press(x, false)], clis);
        assert_eq!(up.closed, Some(0), "hover swaps the badge for the x; close must win");
        assert_eq!(up.clicked, None);
    }

    #[test]
    fn drag_from_the_close_slot_still_reorders() {
        // The x senses only clicks, so a drag STARTING over it must fall through to the tab's
        // click_and_drag widget - the slot swap must not create a reorder dead zone.
        let ctx = egui::Context::default();
        let x = slot_pos(hover_tab(&ctx, 1)[1]);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let moved = tab_frame(&ctx, vec![egui::Event::PointerMoved(x + egui::vec2(40.0, 0.0))]);
        assert_eq!(moved.dragged, Some(1), "dragging from the slot must still reorder");
        let up = tab_frame(&ctx, vec![press(x + egui::vec2(40.0, 0.0), false)]);
        assert_eq!(up.closed, None, "a decided drag must not close the tab");
    }

    #[test]
    fn close_slot_clicks_activate_when_not_hovered_prior() {
        // Without the pointer over the tab there IS no trailing slot: a press landing on the
        // tab's right edge cold hits the tab (the x wasn't registered while unhovered), so
        // the click activates instead of closing.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let slot = slot_pos(warm.rects[1]);
        tab_frame(&ctx, vec![press(slot, true)]);
        let up = tab_frame(&ctx, vec![press(slot, false)]);
        assert_eq!(up.clicked, Some(1), "a cold press on the slot area must click-activate");
        assert_eq!(up.closed, None);
    }

    #[test]
    fn backspace_reaches_the_pty_through_a_real_frame() {
        // End-to-end through egui: a Backspace key event in a frame with the grid focused and
        // no modal open must come out of collect_input as DEL (0x7f).
        let ctx = egui::Context::default();
        tab_frame(&ctx, vec![]);
        let out = tab_frame(
            &ctx,
            vec![egui::Event::Key {
                key: Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }],
        );
        assert_eq!(out.keys, vec![0x7f]);
    }

    // ---- the right-pinned Settings tab (settings-as-a-tab, 1.0.4) ----

    struct SettingsBarOut {
        term_rect: egui::Rect,
        settings_rect: egui::Rect,
        term_clicked: bool,
        settings_clicked: bool,
        settings_closed: bool,
    }

    /// One frame of the tab-bar row while a settings session exists: a terminal tab, the
    /// right-pinning spacer, and the Settings tab (`idx = None`, fixed width) - the exact
    /// structure `tab_bar` builds.
    fn settings_bar_frame(ctx: &egui::Context, events: Vec<egui::Event>) -> SettingsBarOut {
        let mut out = SettingsBarOut {
            term_rect: egui::Rect::NOTHING,
            settings_rect: egui::Rect::NOTHING,
            term_clicked: false,
            settings_clicked: false,
            settings_closed: false,
        };
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            egui::Panel::top("tabbar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (t, _) = draw_tab(
                        ui,
                        Some(1),
                        1,
                        "zsh",
                        false,
                        false,
                        None,
                        Progress::None,
                        CmdState::Idle,
                        &[],
                        None,
                        None,
                    );
                    out.term_clicked = t.clicked();
                    out.term_rect = t.rect;
                    // The Settings tab REPLACES the gear (they never both show), so it pins to
                    // the right edge with only its own width reserved.
                    ui.add_space((ui.available_width() - SETTINGS_TAB_W).max(0.0));
                    let (s, s_close) = draw_tab(
                        ui,
                        None,
                        u64::MAX,
                        "Settings",
                        true,
                        false,
                        None,
                        Progress::None,
                        CmdState::Idle,
                        &[],
                        None,
                        Some(SETTINGS_TAB_W),
                    );
                    out.settings_rect = s.rect;
                    out.settings_clicked = s.clicked();
                    out.settings_closed = s_close;
                });
            });
        });
        out
    }

    #[test]
    fn settings_tab_clicks_activate_switch_and_close() {
        // The settings-as-a-tab contract: clicking the Settings tab re-activates the view,
        // a terminal tab beside it still takes clicks (the switch that HIDES settings while
        // keeping the staged edits), and the trailing close-x reports the guarded close.
        let ctx = egui::Context::default();
        let warm = settings_bar_frame(&ctx, vec![]);
        let s = warm.settings_rect.center();
        settings_bar_frame(&ctx, vec![egui::Event::PointerMoved(s)]);
        settings_bar_frame(&ctx, vec![press(s, true)]);
        let up = settings_bar_frame(&ctx, vec![press(s, false)]);
        assert!(up.settings_clicked, "click must activate the settings tab");
        let t = warm.term_rect.center();
        settings_bar_frame(&ctx, vec![egui::Event::PointerMoved(t)]);
        settings_bar_frame(&ctx, vec![press(t, true)]);
        let up = settings_bar_frame(&ctx, vec![press(t, false)]);
        assert!(up.term_clicked, "terminal tabs must stay clickable next to the settings tab");
        // Close-x: hover the settings tab, settle a frame (the trailing slot appears from
        // LAST frame's hover), then click the slot.
        settings_bar_frame(&ctx, vec![egui::Event::PointerMoved(s)]);
        let settled = settings_bar_frame(&ctx, vec![]);
        let x = slot_pos(settled.settings_rect);
        settings_bar_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        settings_bar_frame(&ctx, vec![press(x, true)]);
        let up = settings_bar_frame(&ctx, vec![press(x, false)]);
        assert!(up.settings_closed, "the settings tab close-x must report the close");
        assert!(!up.settings_clicked, "a close-x click must not double as an activate");
    }

    #[test]
    fn unnumbered_tab_drops_the_prefix() {
        // `idx = None` (the Settings tab) renders without the "N  " number prefix: with the
        // same title and dynamic sizing it must come out strictly narrower than a numbered
        // twin (the prefix is the only width difference).
        let ctx = egui::Context::default();
        let mut widths = (0.0_f32, 0.0_f32);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            ui.horizontal(|ui| {
                let (a, _) = draw_tab(
                    ui,
                    Some(1),
                    10,
                    "Settings",
                    false,
                    false,
                    None,
                    Progress::None,
                    CmdState::Idle,
                    &[],
                    None,
                    None,
                );
                let (b, _) = draw_tab(
                    ui,
                    None,
                    11,
                    "Settings",
                    false,
                    false,
                    None,
                    Progress::None,
                    CmdState::Idle,
                    &[],
                    None,
                    None,
                );
                widths = (a.rect.width(), b.rect.width());
            });
        });
        assert!(widths.1 < widths.0, "unnumbered tab must be narrower (no prefix): {widths:?}");
    }
}
