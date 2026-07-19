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
    if alt_is_meta && mods.alt && !mods.command {
        if let Some(n) = ctrl_letter(key) {
            return Some(vec![0x1b, b'a' + n - 1]);
        }
        if let Some(d) = digit(key) {
            return Some(vec![0x1b, b'0' + d]);
        }
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
        Key::Delete => b"\x1b[3~".to_vec(), // forward delete
        Key::Insert => b"\x1b[2~".to_vec(),
        // Shift+Home/End/PageUp/PageDown are app scrollback bindings - don't forward those.
        Key::Home | Key::End | Key::PageUp | Key::PageDown if mods.shift => return None,
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
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

/// Digit value for `Key::Num0..Num9`, or `None` (for altIsMeta ESC-digit mapping).
fn digit(key: egui::Key) -> Option<u8> {
    use egui::Key;
    let n = match key {
        Key::Num0 => 0,
        Key::Num1 => 1,
        Key::Num2 => 2,
        Key::Num3 => 3,
        Key::Num4 => 4,
        Key::Num5 => 5,
        Key::Num6 => 6,
        Key::Num7 => 7,
        Key::Num8 => 8,
        Key::Num9 => 9,
        _ => return None,
    };
    Some(n)
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

/// Bytes an Alt+wheel tick sends instead of scrolling (Tabby `baseTerminalTab` mousewheel
/// handler): one SS3 up/down arrow per line - positive `lines` (wheel up) = `ESC O A`.
pub(crate) fn alt_scroll_bytes(lines: i32) -> Vec<u8> {
    let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
    seq.repeat(lines.unsigned_abs() as usize)
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

/// The close-tab confirm prompt, or `None` to close silently. A pinned tab always asks;
/// otherwise only a running child process does (`warn_on_close_running` gating is the caller's).
pub(crate) fn close_confirm_message(pinned: bool, busy: Option<&str>) -> Option<String> {
    match (pinned, busy) {
        (true, _) => Some("This tab is pinned.".into()),
        (false, Some(name)) => Some(format!("{name} is still running in this tab.")),
        (false, None) => None,
    }
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

/// Parse a `[hotkeys]` chord spec ("Cmd+Shift+T", "Cmd+,", "F13") into (modifiers, key).
/// `None` for anything unparseable - a garbage spec must never match a key press.
/// Rules: the LAST `+`-part is the key, everything before it a modifier; single-character
/// keys (letters/digits/punctuation) and nav keys REQUIRE Cmd/Ctrl/Alt (a bare "T" bind
/// would swallow typing); only F-keys may be bound bare. "+"/"=" both mean `Equals` (they
/// share the physical key; `hotkey_matches` normalizes the pressed side the same way).
/// Cmd-modified C/X/V (any extra modifiers included) are REJECTED: egui-winit folds those
/// presses into `Event::Copy/Cut/Paste` and never emits the `Event::Key`, so such a bind
/// could never fire - a red field beats a silently dead bind.
pub(crate) fn parse_hotkey_spec(spec: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let parts: Vec<&str> = spec.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let (&key_part, mod_parts) = parts.split_last()?;
    let mut mods = egui::Modifiers::NONE;
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => {
                mods.command = true;
                mods.mac_cmd = true;
            }
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "option" | "opt" => mods.alt = true,
            "shift" => mods.shift = true,
            _ => return None, // unknown modifier: never match
        }
    }
    let key = key_from_name(key_part)?;
    let is_fkey = matches!(key_part.to_ascii_lowercase().strip_prefix('f'), Some(d) if d.parse::<u8>().is_ok());
    if !(is_fkey || mods.command || mods.ctrl || mods.alt) {
        return None; // bare / shift-only single keys would shadow typing
    }
    if mods.command && matches!(key, egui::Key::C | egui::Key::X | egui::Key::V) {
        return None; // folded into Copy/Cut/Paste events - the Key event never arrives
    }
    Some((mods, key))
}

/// Friendly key name -> `egui::Key`. Case-insensitive; accepts punctuation literals.
#[allow(clippy::too_many_lines)] // a flat name table; splitting it would obscure it
fn key_from_name(name: &str) -> Option<egui::Key> {
    use egui::Key;
    let n = name.to_ascii_lowercase();
    // F-keys first so "f1" doesn't fall into the single-letter branch.
    if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
        let fkeys = [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
            Key::F13,
            Key::F14,
            Key::F15,
            Key::F16,
            Key::F17,
            Key::F18,
            Key::F19,
            Key::F20,
        ];
        return (1..=20).contains(&num).then(|| fkeys[usize::from(num) - 1]);
    }
    let key = match n.as_str() {
        "a" => Key::A,
        "b" => Key::B,
        "c" => Key::C,
        "d" => Key::D,
        "e" => Key::E,
        "f" => Key::F,
        "g" => Key::G,
        "h" => Key::H,
        "i" => Key::I,
        "j" => Key::J,
        "k" => Key::K,
        "l" => Key::L,
        "m" => Key::M,
        "n" => Key::N,
        "o" => Key::O,
        "p" => Key::P,
        "q" => Key::Q,
        "r" => Key::R,
        "s" => Key::S,
        "t" => Key::T,
        "u" => Key::U,
        "v" => Key::V,
        "w" => Key::W,
        "x" => Key::X,
        "y" => Key::Y,
        "z" => Key::Z,
        "0" | "num0" => Key::Num0,
        "1" | "num1" => Key::Num1,
        "2" | "num2" => Key::Num2,
        "3" | "num3" => Key::Num3,
        "4" | "num4" => Key::Num4,
        "5" | "num5" => Key::Num5,
        "6" | "num6" => Key::Num6,
        "7" | "num7" => Key::Num7,
        "8" | "num8" => Key::Num8,
        "9" | "num9" => Key::Num9,
        "," | "comma" => Key::Comma,
        "." | "period" => Key::Period,
        ";" | "semicolon" => Key::Semicolon,
        "/" | "slash" => Key::Slash,
        "\\" | "backslash" => Key::Backslash,
        "-" | "minus" => Key::Minus,
        "=" | "plus" | "equals" => Key::Equals, // shared physical key, normalized
        "`" | "grave" | "backtick" | "backquote" => Key::Backtick,
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" | "arrowup" => Key::ArrowUp,
        "down" | "arrowdown" => Key::ArrowDown,
        "left" | "arrowleft" => Key::ArrowLeft,
        "right" | "arrowright" => Key::ArrowRight,
        _ => return None,
    };
    Some(key)
}

/// "Label (Chord)" tooltip text, or just the label when the action is unbound.
pub(crate) fn shortcut_tip(label: &str, chord: &str) -> String {
    if chord.trim().is_empty() { label.to_string() } else { format!("{label} ({chord})") }
}

/// Does a pressed (key, modifiers) match a `[hotkeys]` spec? EXACT modifier match (Cmd+T does
/// not fire on Cmd+Shift+T and vice versa); a spec that doesn't parse (or is empty = unbound)
/// never matches. Pressed `Plus` is normalized to `Equals` so "Cmd+=" keeps zooming on layouts
/// where the same key reports either. NOTE (macOS): `mods.command` is Cmd, `mods.ctrl` is the
/// real Ctrl - the comparison relies on that split.
pub(crate) fn hotkey_matches(spec: &str, key: egui::Key, mods: egui::Modifiers) -> bool {
    let Some((want_mods, want_key)) = parse_hotkey_spec(spec) else {
        return false;
    };
    let key = if key == egui::Key::Plus { egui::Key::Equals } else { key };
    key == want_key
        && mods.command == want_mods.command
        && mods.ctrl == want_mods.ctrl
        && mods.alt == want_mods.alt
        && mods.shift == want_mods.shift
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
pub(crate) const TAB_FIXED_W: f32 = 200.0; // fixed-mode standard width (Tabby-like)
const TAB_MIN_W: f32 = 60.0; // fixed-mode floor when the bar overflows
const TAB_PAD_X: f32 = 10.0;
const TAB_SLOT_W: f32 = 18.0; // leading slot: CLI chip, swapped for the close-x on hover
const TAB_GAP: f32 = 6.0;
const TAB_MINI_W: f32 = 15.0; // split-layout preview glyph

/// Flat Tabby-style tab: dark bg (elevated when active), optional per-tab colored underline
/// flush with the strip's bottom edge, a split-layout preview glyph, and progress as a thin bar
/// on the TOP edge. The leading slot holds the CLI chip; hovering the tab swaps it for the
/// close-x (hidden otherwise - even on the active tab), so the two can never overlap.
/// `width` = `Some(px)` for fixed mode (title ellipsized to fit), `None` sizes to content.
/// Returns (click+drag response, close-clicked). `layout` = `Pane::miniature()` leaf rects
/// (glyph shown when >1). `tab_id` seeds the interact id so drag-reorder tracking survives
/// index swaps (ui.md).
#[allow(clippy::too_many_lines)] // one widget, mostly geometry + paint
pub(crate) fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
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
    let prefix = format!("{idx}  ");
    let mini_w = if layout.len() > 1 { TAB_MINI_W + TAB_GAP } else { 0.0 };
    let pin_w = if pinned { 14.0 } else { 0.0 };
    let fixed_chrome = TAB_PAD_X * 2.0 + TAB_SLOT_W + TAB_GAP + mini_w + pin_w;
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
    let slot = egui::Rect::from_min_size(
        egui::pos2(rect.left() + TAB_PAD_X, rect.center().y - TAB_SLOT_W / 2.0),
        egui::vec2(TAB_SLOT_W, TAB_SLOT_W),
    );
    let mut x = slot.right() + TAB_GAP;
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
    // Pinned marker: a small push-pin at the tab's right edge (the index stays as-is).
    if pinned {
        p.text(
            egui::pos2(rect.right() - TAB_PAD_X, rect.center().y),
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
    let mut tab_resp =
        ui.interact(rect, ui.id().with(("tab", tab_id)), egui::Sense::click_and_drag());
    if truncated {
        tab_resp = tab_resp.on_hover_text(title);
    }
    // Leading slot: the close-x only while the pointer is over the tab (replacing the CLI chip,
    // so they can't overlap); the chip - or nothing - otherwise. Use `contains_pointer` (true
    // across the whole tab rect, incl. the x) rather than `hovered` so moving onto the x
    // doesn't drop the tab's hover state and make the x flicker.
    let mut close = false;
    if tab_resp.contains_pointer() {
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
    style: GridStyle,
    search_marks: &[crate::search::Match], // all find-bar matches (empty when the bar is closed)
) -> egui::Response {
    let GridStyle { cursor, dimmed, link_active, blink, ligatures, min_contrast } = style;
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
                    font.clone(),
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
                if cell.wide {
                    // A wide glyph (CJK/emoji) owns this cell AND the spacer after it: draw it
                    // horizontally centered across the two, top-aligned like its neighbors.
                    painter.text(
                        egui::pos2(pos.x + cw, pos.y),
                        egui::Align2::CENTER_TOP,
                        cell.c,
                        font.clone(),
                        fg,
                    );
                } else {
                    painter.text(pos, egui::Align2::LEFT_TOP, cell.c, font.clone(), fg);
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
                    painter.text(pos, anchor, under.c, font.clone(), fade(colors::bg()));
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
    fn alt_is_meta_sends_esc_prefixed_letters() {
        let alt = mods(false, true, false);
        assert_eq!(key_to_bytes(Key::B, alt, true), Some(vec![0x1b, b'b']));
        assert_eq!(key_to_bytes(Key::Num3, alt, true), Some(vec![0x1b, b'3'])); // digits too
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
    fn alt_scroll_sends_ss3_arrows_per_line() {
        assert_eq!(alt_scroll_bytes(1), b"\x1bOA".to_vec()); // wheel up = up arrow
        assert_eq!(alt_scroll_bytes(-1), b"\x1bOB".to_vec());
        assert_eq!(alt_scroll_bytes(3), b"\x1bOA\x1bOA\x1bOA".to_vec());
        assert_eq!(alt_scroll_bytes(-2), b"\x1bOB\x1bOB".to_vec());
        assert!(alt_scroll_bytes(0).is_empty());
    }

    #[test]
    fn ctrl_shift_arrows_send_nothing_to_the_pty() {
        // Ctrl+Shift+Up/Down are the line-step scroll hotkeys (Tabby default binding) -
        // the ctrl branch maps arrows to None, so no reservation is needed.
        let m = Modifiers { ctrl: true, shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::ArrowUp, m, false), None);
        assert_eq!(key_to_bytes(Key::ArrowDown, m, false), None);
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
    fn close_confirm_pinned_beats_busy() {
        assert_eq!(close_confirm_message(false, None), None); // plain tab: close silently
        assert_eq!(
            close_confirm_message(false, Some("vim")).as_deref(),
            Some("vim is still running in this tab.")
        );
        assert_eq!(close_confirm_message(true, None).as_deref(), Some("This tab is pinned."));
        assert_eq!(
            close_confirm_message(true, Some("vim")).as_deref(),
            Some("This tab is pinned.") // pinned always asks, with the pin as the reason
        );
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
    fn hotkey_matches_exact_chords_only() {
        let cmd = mods(false, false, true);
        let cmd_shift = Modifiers { shift: true, ..cmd };
        let ctrl = mods(true, false, false);
        // (spec, key, mods) -> matches?
        let cases = [
            ("Cmd+T", Key::T, cmd, true),
            ("cmd+t", Key::T, cmd, true),        // case-insensitive
            ("Cmd+T", Key::T, cmd_shift, false), // superset modifiers don't fire
            ("Cmd+Shift+T", Key::T, cmd_shift, true),
            ("Cmd+Shift+T", Key::T, cmd, false), // subset modifiers don't fire
            ("Cmd+T", Key::W, cmd, false),
            ("Cmd+,", Key::Comma, cmd, true),
            ("Cmd+Comma", Key::Comma, cmd, true),
            ("Cmd+0", Key::Num0, cmd, true),
            ("Ctrl+K", Key::K, ctrl, true), // ctrl chords are matchable (see reserved-combo note)
            ("Ctrl+K", Key::K, cmd, false), // Cmd is not Ctrl on macOS
            ("Cmd+Up", Key::ArrowUp, cmd, true),
            ("F13", Key::F13, Modifiers::default(), true), // F-keys may be bare
            ("Shift+F5", Key::F5, Modifiers { shift: true, ..Modifiers::default() }, true),
        ];
        for (spec, key, m, want) in cases {
            assert_eq!(hotkey_matches(spec, key, m), want, "{spec} vs {key:?}");
        }
    }

    #[test]
    fn hotkey_plus_and_equals_share_the_key() {
        let cmd = mods(false, false, true);
        assert!(hotkey_matches("Cmd+=", Key::Equals, cmd));
        assert!(hotkey_matches("Cmd+=", Key::Plus, cmd)); // shifted layouts report Plus
        assert!(hotkey_matches("Cmd+Plus", Key::Equals, cmd));
        assert!(hotkey_matches("Cmd+-", Key::Minus, cmd));
    }

    #[test]
    fn garbage_hotkey_specs_never_match() {
        let cmd = mods(false, false, true);
        for spec in ["", "   ", "nonsense", "Cmd+", "Cmd+Nope", "Hyper+T", "Cmd+F99", "+++"] {
            for key in [Key::T, Key::Comma, Key::F13, Key::Enter] {
                assert!(!hotkey_matches(spec, key, cmd), "{spec:?} must never match {key:?}");
                assert!(!hotkey_matches(spec, key, Modifiers::default()));
            }
        }
    }

    #[test]
    fn shortcut_tip_hides_unbound_chords() {
        assert_eq!(shortcut_tip("New tab", "Cmd+T"), "New tab (Cmd+T)");
        assert_eq!(shortcut_tip("New tab", ""), "New tab");
        assert_eq!(shortcut_tip("New tab", "  "), "New tab");
    }

    #[test]
    fn bare_single_keys_are_rejected_but_fkeys_pass() {
        // A bare letter/digit/punct bind would swallow typing - parse refuses it.
        assert_eq!(parse_hotkey_spec("T"), None);
        assert_eq!(parse_hotkey_spec("7"), None);
        assert_eq!(parse_hotkey_spec(","), None);
        assert_eq!(parse_hotkey_spec("Shift+T"), None); // shift-only too (it's just typing 'T')
        assert_eq!(parse_hotkey_spec("Enter"), None);
        assert!(parse_hotkey_spec("F13").is_some());
        assert!(parse_hotkey_spec("Cmd+T").is_some());
        assert!(parse_hotkey_spec("Alt+Space").is_some());
    }

    #[test]
    fn cmd_clipboard_chords_are_rejected_as_unmatchable() {
        // egui-winit folds ANY Cmd-modified C/X/V press into Event::Copy/Cut/Paste and
        // returns before pushing the Key event (egui-winit 0.35 lib.rs is_copy_command et
        // al.), so a bind on those chords could never fire. Parse refuses them upfront -
        // the settings field turns red instead of shipping a silently dead bind.
        for spec in ["Cmd+C", "Cmd+Shift+C", "Cmd+X", "Cmd+V", "Cmd+Alt+V", "Ctrl+Cmd+X"] {
            assert_eq!(parse_hotkey_spec(spec), None, "{spec} must be rejected");
        }
        // Without Cmd they're ordinary chords (Ctrl+C keeps its copy-or-SIGINT intercept -
        // a rebind there double-fires by design, same as any terminal-bound chord).
        assert!(parse_hotkey_spec("Ctrl+C").is_some());
        assert!(parse_hotkey_spec("Ctrl+Shift+V").is_some());
    }

    #[test]
    fn rebound_terminal_chords_double_fire_by_design() {
        // Reserved-combo integrity (LEDGER 0.5.0): the DEFAULT binds are chosen so key_to_bytes
        // sends nothing for them (no double-fire). A user rebind onto a terminal-bound chord
        // (e.g. Ctrl+K = Ctrl-L-style kill) matches the app action AND still reaches the pty -
        // documented behavior, asserted here so it can't silently change.
        let cmd = mods(false, false, true);
        let ctrl = mods(true, false, false);
        // Defaults: app-only (no pty bytes). Cmd+letter chords are unmapped in key_to_bytes.
        for (spec, key) in [("Cmd+T", Key::T), ("Cmd+W", Key::W), ("Cmd+O", Key::O)] {
            assert!(hotkey_matches(spec, key, cmd));
            assert_eq!(key_to_bytes(key, cmd, false), None, "{spec} must not leak to the pty");
        }
        // A custom Ctrl+letter rebind collides: both the app action and the control byte fire.
        assert!(hotkey_matches("Ctrl+K", Key::K, ctrl));
        assert_eq!(key_to_bytes(Key::K, ctrl, false), Some(vec![11]));
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

    #[test]
    fn nav_and_edit_keys_map_to_csi() {
        let none = Modifiers::default();
        assert_eq!(key_to_bytes(Key::Delete, none, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_to_bytes(Key::Insert, none, false), Some(b"\x1b[2~".to_vec()));
        assert_eq!(key_to_bytes(Key::Home, none, false), Some(b"\x1b[H".to_vec()));
        assert_eq!(key_to_bytes(Key::End, none, false), Some(b"\x1b[F".to_vec()));
        assert_eq!(key_to_bytes(Key::PageUp, none, false), Some(b"\x1b[5~".to_vec()));
        assert_eq!(key_to_bytes(Key::PageDown, none, false), Some(b"\x1b[6~".to_vec()));
        // Shift variants are app scrollback bindings - reserved.
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        for k in [Key::Home, Key::End, Key::PageUp, Key::PageDown] {
            assert_eq!(key_to_bytes(k, shift, false), None, "{k:?} must stay an app bind");
        }
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
                    for i in 0..2usize {
                        let (resp, close) = draw_tab(
                            ui,
                            i + 1,
                            i as u64, // stable id
                            "tab",
                            i == 0,
                            false,
                            None,
                            Progress::None,
                            CmdState::Idle,
                            &[],
                            None,
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
        // The x is hover-only now (it swaps into the leading slot), so the pointer must be
        // over the tab BEFORE the click - exactly what a real mouse does.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let r = warm.rects[0];
        let x = egui::pos2(r.left() + TAB_PAD_X + TAB_SLOT_W / 2.0, r.center().y);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let up = tab_frame(&ctx, vec![press(x, false)]);
        assert_eq!(up.closed, Some(0), "the close-x must win its own click");
        assert_eq!(up.clicked, None);
    }

    #[test]
    fn close_x_closes_an_unfocused_tab() {
        // The x shows on hover for EVERY tab now (not just the active one), and clicking it
        // must close - not merely focus - the unfocused tab.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let r = warm.rects[1];
        let x = egui::pos2(r.left() + TAB_PAD_X + TAB_SLOT_W / 2.0, r.center().y);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let up = tab_frame(&ctx, vec![press(x, false)]);
        assert_eq!(up.closed, Some(1), "the x must close the unfocused tab");
        assert_eq!(up.clicked, None, "the click must not fall through and focus it");
    }

    #[test]
    fn drag_from_the_close_slot_still_reorders() {
        // The x senses only clicks, so a drag STARTING over it must fall through to the tab's
        // click_and_drag widget - the slot swap must not create a reorder dead zone.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let r = warm.rects[1];
        let x = egui::pos2(r.left() + TAB_PAD_X + TAB_SLOT_W / 2.0, r.center().y);
        tab_frame(&ctx, vec![egui::Event::PointerMoved(x)]);
        tab_frame(&ctx, vec![press(x, true)]);
        let moved = tab_frame(&ctx, vec![egui::Event::PointerMoved(x + egui::vec2(40.0, 0.0))]);
        assert_eq!(moved.dragged, Some(1), "dragging from the slot must still reorder");
        let up = tab_frame(&ctx, vec![press(x + egui::vec2(40.0, 0.0), false)]);
        assert_eq!(up.closed, None, "a decided drag must not close the tab");
    }

    #[test]
    fn close_slot_clicks_activate_when_not_hovered_prior() {
        // Without the pointer over the tab, the leading slot is NOT a close button: a press
        // landing there cold hits the tab (the x wasn't registered while unhovered), so the
        // click activates instead of closing.
        let ctx = egui::Context::default();
        let warm = tab_frame(&ctx, vec![]);
        let r = warm.rects[1];
        let slot = egui::pos2(r.left() + TAB_PAD_X + TAB_SLOT_W / 2.0, r.center().y);
        tab_frame(&ctx, vec![press(slot, true)]);
        let up = tab_frame(&ctx, vec![press(slot, false)]);
        assert_eq!(up.clicked, Some(1), "a cold press on the slot must click-activate the tab");
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
}
