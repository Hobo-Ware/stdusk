//! View layer: the egui drawing widgets (tab, icon button, grid, toast) plus the pure,
//! unit-testable helpers extracted from the render loop (geometry, text, input mapping).
//! Keeping the math here - free of `Ui`/`Context` - is what makes it testable; `main.rs`'s
//! `eframe::App` loop stays a thin caller.
use eframe::egui;

pub(crate) use stdusk_egui::ui::{
    CursorStyle, GridStyle, SETTINGS_TAB_W, TAB_FIXED_W, TAB_H, TAB_MIN_W, WINDOW_TAB_H_EXTRA,
    apply_ligatures, apply_theme, blink_on, cursor_style, draw_exit_overlay, draw_pane_notice,
    draw_tab, draw_toast, ellipsize, icons, pane_notice_text, progress_fraction, tint, toast_alpha,
};

/// Last path segment of a cwd, for the auto tab title. Trailing slashes ignored; `/` for root.
pub(crate) fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".into();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// Radius of the pending-update dot painted on the settings gear.
pub(crate) const UPDATE_DOT_R: f32 = 3.0;

/// Center for the pending-update dot: inset from the icon's top-right so it reads as a badge on
/// the gear rather than a stray pixel, and stays inside the rect (never clipped by the tab bar).
pub(crate) fn update_dot_center(icon: egui::Rect) -> egui::Pos2 {
    let inset = UPDATE_DOT_R + 1.0;
    egui::pos2(icon.right() - inset, icon.top() + inset)
}

/// Text inserted when files are dropped on a pane: each path POSIX single-quoted (so spaces
/// and shell metacharacters are literal) and space-joined, with a trailing space so the next
/// dropped/typed token stays separated. Empty when no path-backed files were dropped (a drop
/// with only in-memory bytes, e.g. from a browser, has no filesystem path to insert).
pub(crate) fn drop_paste_string(paths: &[std::path::PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str(&stdusk_core::shell::single_quote(&p.to_string_lossy()));
        out.push(' ');
    }
    out
}

/// Is the OS appearance LIGHT? The platform's own answer (`macos::os_dark_mode`) wins, because it
/// is available before the first frame; egui's `system_theme` is the cross-platform fallback and is
/// only as good as what the window system reported. "Nobody knows" reads as DARK - the historical
/// default for a terminal, and what the follow-system reconcile has always done.
pub(crate) fn system_is_light(os_dark: Option<bool>, reported: Option<egui::Theme>) -> bool {
    match os_dark {
        Some(dark) => !dark,
        None => matches!(reported, Some(egui::Theme::Light)),
    }
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const QUAKE_HIDE_SLIVER: f32 = 2.0;

/// The window's top-left y when hidden: parked so only `QUAKE_HIDE_SLIVER` points remain at the
/// bottom edge. INVARIANT: strictly less than `monitor_h` (the window stays ON-SCREEN) and the
/// visible sliver is exactly `QUAKE_HIDE_SLIVER` - the exact regression guard for the "hotkey can't
/// reshow after hide" bug. Pure so the invariant is testable without a window.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn quake_hidden_y(monitor_h: f32) -> f32 {
    monitor_h - QUAKE_HIDE_SLIVER
}

/// The quake window's inner size when shown: full monitor width, `height_pct` of its height
/// (rounded). Pure companion to `quake_hidden_y` so the show/hide geometry is testable.
pub(crate) fn quake_shown_size(monitor_w: f32, monitor_h: f32, height_pct: f32) -> (f32, f32) {
    (monitor_w, (monitor_h * height_pct).round())
}

/// Index of the screen whose frame holds `point`. Frames and point share one coordinate space
/// (AppKit's: bottom-left origin, y up), so no conversion is needed to find the cursor's screen.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn screen_index_at(frames: &[egui::Rect], point: egui::Pos2) -> Option<usize> {
    // Half-open: `Rect::contains` includes `max`, so a point on a shared edge would match the
    // screen on the left instead of the one it is the first column of.
    frames.iter().position(|f| {
        f.min.x <= point.x && point.x < f.max.x && f.min.y <= point.y && point.y < f.max.y
    })
}

/// Convert an AppKit screen frame (bottom-left origin, y up, anchored to the primary screen) to the
/// top-left-origin global space that `ViewportCommand::OuterPosition` uses. `primary_h` is the
/// primary screen's height: the primary screen's top-left is then (0, 0) and screens above or
/// below it get negative or larger y.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn cocoa_to_top_left(frame: egui::Rect, primary_h: f32) -> egui::Rect {
    let top = primary_h - frame.max.y;
    egui::Rect::from_min_size(egui::pos2(frame.min.x, top), frame.size())
}

/// Divide an AppKit-point value by egui's zoom factor before it goes into a `ViewportCommand`.
/// egui-winit scales those commands by `zoom_factor * scale_factor` but winit divides by
/// `scale_factor` alone, so without this a Cmd+= zoom would scale the window geometry - and push
/// the hidden sliver fully off-screen, which parks the run loop. A non-positive zoom is ignored.
pub(crate) fn unzoom(v: egui::Vec2, zoom: f32) -> egui::Vec2 {
    if zoom > 0.0 { v / zoom } else { v }
}

/// Top-left of the parked (hidden) quake window on `screen`: the window's left edge, with only
/// `QUAKE_HIDE_SLIVER` points of it left on that screen. Per-screen twin of `quake_hidden_y`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn quake_hidden_pos(screen: egui::Rect) -> egui::Pos2 {
    egui::pos2(screen.min.x, screen.min.y + quake_hidden_y(screen.height()))
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

/// Grid cell height: the measured glyph height plus the user's `appearance.line_padding`,
/// clamped to the settings slider's 0-8px range (a hand-edited config can hold anything).
pub(crate) fn padded_cell_height(glyph_h: f32, line_padding: f32) -> f32 {
    glyph_h + line_padding.clamp(0.0, 8.0)
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

/// What the running-processes confirm is about to do. Three distinct promises, so the modal can't
/// claim a restart terminates shells it is going to HAND to the successor - nor promise a handoff a
/// build without an `.app` bundle cannot perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuitKind {
    Quit,
    /// Restart without a handoff (dev build): the old behavior, shells die.
    Restart,
    /// Restart that moves the live ptys to the successor - the shells keep running.
    RestartKeepingShells,
}

/// The quit confirm body: what the action does to what's running. `procs` running child processes
/// summed across `tabs` tabs. Title ("Quit stdusk?") is drawn by the modal; this is the detail line.
pub(crate) fn quit_confirm_message(
    procs: usize,
    tabs: usize,
    kind: QuitKind,
    resumable: usize,
) -> String {
    let p_noun = if procs == 1 { "process" } else { "processes" };
    let t_noun = if tabs == 1 { "tab" } else { "tabs" };
    // Only a quit that ends the processes resumes anything: a handoff keeps them alive.
    let resume = match (kind, resumable) {
        (QuitKind::RestartKeepingShells, _) | (_, 0) => String::new(),
        (_, 1) => " 1 agent session resumes on the next launch.".to_owned(),
        (_, n) => format!(" {n} agent sessions resume on the next launch."),
    };
    match kind {
        QuitKind::Quit => {
            format!("This will terminate {procs} running {p_noun} across {tabs} {t_noun}.{resume}")
        }
        QuitKind::Restart => format!(
            "This will terminate {procs} running {p_noun} across {tabs} {t_noun}, then relaunch stdusk.{resume}"
        ),
        // Nothing is terminated here, so the line must not say it is. Scrollback is the one real
        // loss (alacritty's Term has no serialization), so it's stated up front.
        QuitKind::RestartKeepingShells => format!(
            "stdusk will relaunch and reattach {procs} running {p_noun} across {tabs} {t_noun}. Scrollback is not restored."
        ),
    }
}

/// `(title, confirm button)` for the running-processes modal. A restart asks its OWN question:
/// answering "Quit?" when the user clicked Restart is how you get a mis-click.
pub(crate) fn quit_confirm_labels(kind: QuitKind) -> (&'static str, &'static str) {
    match kind {
        QuitKind::Quit => ("Quit stdusk?", "Quit"),
        QuitKind::Restart | QuitKind::RestartKeepingShells => ("Restart stdusk?", "Restart"),
    }
}

/// The one-line note under Settings > About's Restart button. Same honesty rule as the confirm
/// modal: promise a reattach only when the running build can actually hand its ptys over.
pub(crate) fn restart_shell_note(keeps_shells: bool) -> &'static str {
    if keeps_shells {
        "Running shells are reattached, with the screen and recent scrollback."
    } else {
        "Running shells are terminated by the restart."
    }
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

/// The one toast for everything startup has to say: one line per message, in order, shown as
/// long as the longest of them. `None` when there is nothing to say.
pub(crate) fn join_startup_toasts(parts: Vec<(String, f64)>) -> Option<(String, f64)> {
    let secs = parts.iter().map(|(_, secs)| *secs).reduce(f64::max)?;
    let texts: Vec<String> = parts.into_iter().map(|(text, _)| text).collect();
    Some((texts.join("\n"), secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stdusk_core::progress::Progress;
    use stdusk_core::terminal::CmdState;
    use stdusk_egui::ui::{TAB_PAD_X, TAB_SLOT_W, collect_input};

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
    fn the_os_appearance_beats_what_the_window_system_reported() {
        // The OS answer is authoritative and available before the first frame; egui's report only
        // fills in where there is none (non-macOS). "Nobody knows" stays DARK.
        assert!(system_is_light(Some(false), Some(egui::Theme::Dark)));
        assert!(system_is_light(Some(false), None));
        assert!(!system_is_light(Some(true), Some(egui::Theme::Light)));
        assert!(system_is_light(None, Some(egui::Theme::Light)));
        assert!(!system_is_light(None, Some(egui::Theme::Dark)));
        assert!(!system_is_light(None, None));
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
    fn update_dot_sits_inside_the_icon_it_badges() {
        // Clipped or overflowing dots read as rendering glitches, so pin containment: the whole
        // circle must stay within the gear's rect, in its top-right corner.
        let icon = egui::Rect::from_min_size(egui::pos2(100.0, 4.0), egui::vec2(28.0, 26.0));
        let c = update_dot_center(icon);
        assert!(c.x + UPDATE_DOT_R <= icon.right(), "overflows right");
        assert!(c.y - UPDATE_DOT_R >= icon.top(), "overflows top");
        assert!(c.x - UPDATE_DOT_R > icon.center().x, "should hug the right half");
        assert!(c.y + UPDATE_DOT_R < icon.center().y, "should hug the top half");
    }

    #[test]
    fn dropped_files_become_quoted_space_joined_paths() {
        use std::path::PathBuf;
        // Single path with a space is single-quoted; trailing space separates the next token.
        assert_eq!(drop_paste_string(&[PathBuf::from("/tmp/my file.txt")]), "'/tmp/my file.txt' ");
        // Multiple paths join with spaces, each quoted.
        assert_eq!(
            drop_paste_string(&[PathBuf::from("/a/b"), PathBuf::from("/c/d")]),
            "'/a/b' '/c/d' "
        );
        // An embedded single quote is escaped as '\'' so the shell reads it literally.
        assert_eq!(drop_paste_string(&[PathBuf::from("/tmp/it's here")]), "'/tmp/it'\\''s here' ");
        // No path-backed files -> nothing to insert.
        assert_eq!(drop_paste_string(&[]), "");
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
            quit_confirm_message(1, 1, QuitKind::Quit, 0),
            "This will terminate 1 running process across 1 tab."
        );
        assert_eq!(
            quit_confirm_message(5, 2, QuitKind::Quit, 0),
            "This will terminate 5 running processes across 2 tabs."
        );
    }

    #[test]
    fn startup_toasts_join_instead_of_replacing_each_other() {
        let font = ("Font not found: Foo".to_owned(), 3.0);
        let resume = ("Could not resume 1 session: api (directory missing)".to_owned(), 8.0);
        assert_eq!(join_startup_toasts(vec![]), None);
        assert_eq!(join_startup_toasts(vec![font.clone()]), Some(font.clone()));
        assert_eq!(
            join_startup_toasts(vec![resume.clone(), font.clone()]),
            Some((format!("{}\n{}", resume.0, font.0), 8.0))
        );
    }

    #[test]
    fn a_long_startup_toast_wraps_inside_the_window() {
        let long = "1 session ended by a crash signal. Its resume command is typed. Press Enter to reopen it.";
        let (text, _) = join_startup_toasts(vec![
            (format!("Could not resume 3 sessions: api (directory missing), web (transcript missing), x (invalid session id). {long}"), 8.0),
            ("Font not found: Foo".to_owned(), 3.0),
            ("Session reattached - older scrollback did not carry over".to_owned(), 6.0),
        ])
        .unwrap();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(480.0, 300.0));
        let ctx = egui::Context::default();
        let raw = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
        let out = ctx.run_ui(raw, |ui| draw_toast(ui, &text, 1.0));
        let shapes: Vec<_> = out.shapes.iter().map(|s| s.shape.visual_bounding_rect()).collect();
        assert!(!shapes.is_empty(), "the toast paints something");
        let bounds = shapes.into_iter().fold(egui::Rect::NOTHING, egui::Rect::union);
        assert!(screen.contains_rect(bounds), "toast {bounds:?} sticks out of {screen:?}");
        assert!(bounds.height() > 60.0, "the long text wrapped onto several lines: {bounds:?}");
    }

    #[test]
    fn quit_confirm_says_how_many_agent_sessions_resume() {
        assert_eq!(
            quit_confirm_message(2, 1, QuitKind::Quit, 1),
            "This will terminate 2 running processes across 1 tab. 1 agent session resumes on the next launch."
        );
        assert_eq!(
            quit_confirm_message(4, 2, QuitKind::Restart, 3),
            "This will terminate 4 running processes across 2 tabs, then relaunch stdusk. 3 agent sessions resume on the next launch."
        );
        // A handoff restart keeps every process alive, so nothing resumes and nothing is claimed.
        assert!(!quit_confirm_message(4, 2, QuitKind::RestartKeepingShells, 3).contains("resume"));
        assert!(!quit_confirm_message(4, 2, QuitKind::Quit, 0).contains("resume"));
    }

    #[test]
    fn restart_confirm_asks_about_restarting_not_quitting() {
        // Clicking Restart used to raise a "Quit stdusk?" modal with a Quit button - the wrong
        // question, and a mis-click waiting to happen.
        assert_eq!(quit_confirm_labels(QuitKind::Restart), ("Restart stdusk?", "Restart"));
        assert_eq!(
            quit_confirm_labels(QuitKind::RestartKeepingShells),
            ("Restart stdusk?", "Restart")
        );
        assert_eq!(quit_confirm_labels(QuitKind::Quit), ("Quit stdusk?", "Quit"));
        // The restart wording also has to promise the relaunch, since that's the difference.
        let m = quit_confirm_message(2, 1, QuitKind::Restart, 0);
        assert_eq!(
            m,
            "This will terminate 2 running processes across 1 tab, then relaunch stdusk."
        );
        assert!(!quit_confirm_message(2, 1, QuitKind::Quit, 0).contains("relaunch"));
    }

    #[test]
    fn handoff_restart_never_claims_it_terminates_anything() {
        // A restart that hands the ptys over keeps every shell alive, so the modal must drop the
        // "terminate" claim entirely - and must name the one thing that IS lost.
        let m = quit_confirm_message(3, 2, QuitKind::RestartKeepingShells, 2);
        assert_eq!(
            m,
            "stdusk will relaunch and reattach 3 running processes across 2 tabs. Scrollback is not restored."
        );
        assert!(!m.contains("terminate"));
        // Without a handoff the wording stays honest about the kill.
        assert!(quit_confirm_message(3, 2, QuitKind::Restart, 0).contains("terminate"));
        // Same split for the About note.
        assert!(restart_shell_note(true).contains("reattached"));
        assert!(!restart_shell_note(true).contains("terminated"));
        assert!(restart_shell_note(false).contains("terminated"));
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
    fn padded_cell_height_adds_clamped_padding() {
        assert_eq!(padded_cell_height(16.0, 0.0), 16.0);
        assert_eq!(padded_cell_height(16.0, 3.0), 19.0);
        assert_eq!(padded_cell_height(16.0, 99.0), 24.0); // clamped to 8
        assert_eq!(padded_cell_height(16.0, -2.0), 16.0); // never shrinks the cell
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
    fn screen_index_at_finds_the_screen_under_the_cursor() {
        // Primary 1440x900 at the origin, a 1920x1080 screen to its right (AppKit space).
        let frames = [
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0)),
            egui::Rect::from_min_size(egui::pos2(1440.0, -180.0), egui::vec2(1920.0, 1080.0)),
        ];
        assert_eq!(screen_index_at(&frames, egui::pos2(100.0, 100.0)), Some(0));
        assert_eq!(screen_index_at(&frames, egui::pos2(2000.0, 500.0)), Some(1));
        assert_eq!(screen_index_at(&frames, egui::pos2(-50.0, 10.0)), None); // outside every screen
        // The shared edge at x=1440 belongs to the right screen, not the primary.
        assert_eq!(screen_index_at(&frames, egui::pos2(1440.0, 500.0)), Some(1));
        assert_eq!(screen_index_at(&frames, egui::pos2(1439.9, 500.0)), Some(0));
    }

    #[test]
    fn cocoa_to_top_left_anchors_the_primary_screen_at_the_origin() {
        let primary = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0));
        assert_eq!(cocoa_to_top_left(primary, 900.0).min, egui::pos2(0.0, 0.0));
        // A 1080-tall screen on the right whose bottom sits 180 below the primary's: its top
        // lines up with the primary's top, so its top-left y is 0.
        let right =
            egui::Rect::from_min_size(egui::pos2(1440.0, -180.0), egui::vec2(1920.0, 1080.0));
        let r = cocoa_to_top_left(right, 900.0);
        assert_eq!((r.min, r.size()), (egui::pos2(1440.0, 0.0), egui::vec2(1920.0, 1080.0)));
        // A screen to the left of the primary keeps its negative x.
        let left = egui::Rect::from_min_size(egui::pos2(-1920.0, 0.0), egui::vec2(1920.0, 1080.0));
        assert_eq!(cocoa_to_top_left(left, 900.0).min, egui::pos2(-1920.0, -180.0));
        // A screen stacked above the primary gets a negative y.
        let above = egui::Rect::from_min_size(egui::pos2(0.0, 900.0), egui::vec2(1440.0, 900.0));
        assert_eq!(cocoa_to_top_left(above, 900.0).min, egui::pos2(0.0, -900.0));
    }

    #[test]
    fn quake_hidden_pos_leaves_only_the_sliver_on_its_own_screen() {
        // The sliver must stay on THIS screen, not on the primary one, or macOS parks the run loop.
        let screen =
            egui::Rect::from_min_size(egui::pos2(1440.0, 100.0), egui::vec2(1920.0, 1080.0));
        let p = quake_hidden_pos(screen);
        assert_eq!(p, egui::pos2(1440.0, 100.0 + 1080.0 - QUAKE_HIDE_SLIVER));
    }

    #[test]
    fn a_zoomed_ui_still_parks_the_sliver_on_screen() {
        // egui-winit + winit turn a sent point into `sent * zoom` AppKit points; model that.
        let screen =
            egui::Rect::from_min_size(egui::pos2(1728.0, -323.0), egui::vec2(2560.0, 1440.0));
        for zoom in [0.5_f32, 0.9, 1.0, 1.1, 2.0] {
            let parked = quake_hidden_pos(screen);
            let sent = unzoom(parked.to_vec2(), zoom);
            let landed = egui::pos2(sent.x * zoom, sent.y * zoom);
            assert!(
                (landed.x - parked.x).abs() < 0.01 && (landed.y - parked.y).abs() < 0.01,
                "zoom {zoom}"
            );
            assert!((screen.max.y - landed.y - QUAKE_HIDE_SLIVER).abs() < 0.01, "zoom {zoom}");
        }
        assert_eq!(unzoom(egui::vec2(10.0, 20.0), 0.0), egui::vec2(10.0, 20.0)); // bad zoom: unchanged
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

    struct TabFrameOut {
        rects: Vec<egui::Rect>,
        clicked: Option<usize>,
        double: Option<usize>,
        closed: Option<usize>,
        dragged: Option<usize>,
        keys: Vec<u8>,
        /// App actions the `[hotkeys]` step fired this frame (empty when no binds are given).
        actions: Vec<crate::keys::HotkeyAction>,
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
        clis: [Option<stdusk_core::procwatch::Cli>; 2],
    ) -> TabFrameOut {
        tab_frame_hk(ctx, events, clis, None)
    }

    /// `tab_frame` with the render loop's `[hotkeys]` step in front: match the chords against
    /// `hotkeys`, then consume them before the grid's `collect_input` runs (main.rs order).
    fn hotkey_frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        hotkeys: &crate::config::Hotkeys,
    ) -> TabFrameOut {
        tab_frame_hk(ctx, events, [None, None], Some(hotkeys))
    }

    fn tab_frame_hk(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        clis: [Option<stdusk_core::procwatch::Cli>; 2],
        hotkeys: Option<&crate::config::Hotkeys>,
    ) -> TabFrameOut {
        let mut out = TabFrameOut {
            rects: Vec::new(),
            clicked: None,
            double: None,
            closed: None,
            dragged: None,
            keys: Vec::new(),
            actions: Vec::new(),
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
            if let Some(hk) = hotkeys {
                let guards = crate::keys::HotkeyGuards {
                    text_modal: false,
                    hard_modal: false,
                    can_switch_tabs: true,
                    repo_grouping: true,
                };
                let matched = ui.input(|i| crate::keys::matched_hotkeys(&i.events, hk, guards));
                ui.ctx().input_mut(|i| crate::keys::consume_hotkey_events(&mut i.events, &matched));
                out.actions = matched.into_iter().map(|(a, _)| a).collect();
            }
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
                out.keys = collect_input(ui, false, false, false);
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
        let clis = [Some(stdusk_core::procwatch::Cli::Claude), None];
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

    fn fkey_press(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn a_bound_function_key_runs_the_action_and_stays_out_of_the_pty() {
        // The reported bug: `[hotkeys] find = "F3"` opened the find bar AND htop got F3.
        // Real frame structure: hotkey step, tab bar, focused grid, collect_input.
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let none = Modifiers::default();
        let bare_f5 = crate::config::Hotkeys { find: "F5".into(), ..Default::default() };
        let unbound = crate::config::Hotkeys { find: String::new(), ..Default::default() };
        let default_hk = crate::config::Hotkeys::default();
        // (label, hotkeys, key press, expected actions, expected pty bytes)
        let cases: [(&str, &crate::config::Hotkeys, Modifiers, Vec<_>, &[u8]); 4] = [
            ("bound F5", &bare_f5, none, vec![crate::keys::HotkeyAction::Find], b""),
            ("unbound F5", &unbound, none, vec![], b"\x1b[15~"),
            ("Shift+F5, only bare F5 bound", &bare_f5, shift, vec![], b"\x1b[15;2~"),
            ("default binds leave F5 alone", &default_hk, none, vec![], b"\x1b[15~"),
        ];
        for (label, hk, m, actions, bytes) in cases {
            let ctx = egui::Context::default();
            hotkey_frame(&ctx, vec![], hk); // warm-up: layout exists
            let out = hotkey_frame(&ctx, vec![fkey_press(Key::F5, m)], hk);
            assert_eq!(out.actions, actions, "{label}");
            assert_eq!(out.keys, bytes, "{label}");
        }
    }

    #[test]
    fn a_consumed_hotkey_does_not_swallow_the_typing_around_it() {
        // Only the matched press leaves the frame: keys typed in the same frame still arrive.
        let hk = crate::config::Hotkeys { find: "F3".into(), ..Default::default() };
        let ctx = egui::Context::default();
        hotkey_frame(&ctx, vec![], &hk);
        let out = hotkey_frame(
            &ctx,
            vec![
                egui::Event::Text("l".into()),
                fkey_press(Key::F3, Modifiers::default()),
                fkey_press(Key::Backspace, Modifiers::default()),
            ],
            &hk,
        );
        assert_eq!(out.actions, vec![crate::keys::HotkeyAction::Find]);
        assert_eq!(out.keys, b"l\x7f");
    }

    #[test]
    fn a_bound_alt_chord_sends_neither_its_key_nor_its_composed_text() {
        // macOS Option+K: the key event, then Text("\u{2da}"). altIsMeta is off (collect_input
        // gets false in the frame helper), so the Text would reach the shell as UTF-8.
        let alt = Modifiers { alt: true, ..Modifiers::default() };
        let press = || vec![fkey_press(Key::K, alt), egui::Event::Text("\u{2da}".into())];
        let bound = crate::config::Hotkeys { clear: "Alt+K".into(), ..Default::default() };
        let unbound = crate::config::Hotkeys::default();
        let ctx = egui::Context::default();
        hotkey_frame(&ctx, vec![], &bound);
        let out = hotkey_frame(&ctx, press(), &bound);
        assert_eq!(out.actions, vec![crate::keys::HotkeyAction::Clear]);
        assert!(out.keys.is_empty(), "bound Alt+K must send nothing: {:?}", out.keys);
        let ctx = egui::Context::default();
        hotkey_frame(&ctx, vec![], &unbound);
        let out = hotkey_frame(&ctx, press(), &unbound);
        assert_eq!(out.actions, []);
        assert_eq!(out.keys, "\u{2da}".as_bytes(), "unbound Alt+K keeps its composed text");
    }

    #[test]
    fn two_bound_chords_in_one_frame_both_leave_the_pty_alone() {
        let hk = crate::config::Hotkeys {
            find: "F3".into(),
            zoom_in: "F4".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        hotkey_frame(&ctx, vec![], &hk);
        let out = hotkey_frame(
            &ctx,
            vec![
                fkey_press(Key::F3, Modifiers::default()),
                fkey_press(Key::F4, Modifiers::default()),
            ],
            &hk,
        );
        assert_eq!(
            out.actions,
            vec![crate::keys::HotkeyAction::Find, crate::keys::HotkeyAction::Zoom(1)]
        );
        assert!(out.keys.is_empty(), "{:?}", out.keys);
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
