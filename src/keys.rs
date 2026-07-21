//! Keyboard + input encoding: key presses -> pty bytes (`key_to_bytes`, `ctrl_letter`, `digit`),
//! Alt+wheel scroll bytes, and the `[hotkeys]` chord parser/matcher (`parse_hotkey_spec`,
//! `key_from_name`, `hotkey_matches`). Pure functions split out of `ui.rs` so the keyboard
//! table stays unit-testable and separate from the egui widgets.
use eframe::egui;

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
        // Option+Enter sends meta+Return (ESC+CR); apps like Claude Code read this as "insert
        // newline" vs a bare CR "submit". Enter has no composed glyph, so this is unconditional
        // on alt (not gated on alt_is_meta). Cmd+Alt+Enter already returned None above.
        Key::Enter if mods.alt => b"\x1b\r".to_vec(),
        // Shift+Enter sends a bare LF; apps like Claude Code read this as "insert newline" vs a
        // bare CR "submit" (the byte its `/terminal-setup` maps Shift+Enter to). LF, not the
        // CSI-u form `ESC[13;2u`, because `key_to_bytes` never negotiates the kitty keyboard
        // protocol (see terminal.rs) so an un-negotiated CSI-u would be misread. If an app wants
        // the CSI-u form, that's a kitty-protocol project, not a byte swap here.
        Key::Enter if mods.shift => vec![b'\n'],
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

/// Bytes an Alt+wheel tick sends instead of scrolling (Tabby `baseTerminalTab` mousewheel
/// handler): one SS3 up/down arrow per line - positive `lines` (wheel up) = `ESC O A`.
pub(crate) fn alt_scroll_bytes(lines: i32) -> Vec<u8> {
    let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
    seq.repeat(lines.unsigned_abs() as usize)
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

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui::{Key, Modifiers};

    use super::{alt_scroll_bytes, ctrl_letter, hotkey_matches, key_to_bytes, parse_hotkey_spec};

    fn mods(ctrl: bool, alt: bool, command: bool) -> Modifiers {
        Modifiers { alt, ctrl, shift: false, mac_cmd: command, command }
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
        // Option+Enter -> meta+Return (ESC+CR): apps read it as "insert newline", not "submit".
        assert_eq!(
            key_to_bytes(Key::Enter, mods(false, true, false), false),
            Some(vec![0x1b, b'\r'])
        );
        // Cmd+Alt+Enter stays an app pane binding (maximize), not forwarded.
        assert_eq!(key_to_bytes(Key::Enter, mods(false, true, true), false), None);
        // Shift+Enter -> bare LF ("insert newline"); distinct from plain Enter's CR ("submit")
        // and Option+Enter's ESC+CR. All three coexist.
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::Enter, shift, false), Some(vec![b'\n']));
        assert_eq!(key_to_bytes(Key::Enter, Modifiers::default(), false), Some(vec![b'\r']));
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
}
