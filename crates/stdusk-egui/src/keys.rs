//! Keyboard input encoding: egui key presses -> the bytes a pty expects (`key_to_bytes`,
//! `ctrl_letter`) and Alt+wheel scroll bytes. Pure functions, unit-tested without a window.
/// Bytes a key press sends to the pty, or `None` when the key is unmapped (plain text is
/// handled separately via `Event::Text`). F-keys are encoded first (terminfo, any modifiers);
/// then `Ctrl+letter` wins over the rest; then macOS natural-editing: `Option+←/→` word,
/// `Cmd+←/→` line ends, `Option/Cmd+Backspace` deletes.
pub fn key_to_bytes(
    key: egui::Key,
    mods: egui::Modifiers,
    alt_is_meta: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    use egui::Key;
    // Cursor + Home/End keys switch CSI (`ESC [ x`) <-> SS3 (`ESC O x`) with the app's DECCKM /
    // application-cursor-keys mode. TUIs that set it (vim, less, some pickers) may bind only the
    // SS3 form; zsh binds both. `app_cursor` = the focused term's TermMode::APP_CURSOR.
    let cursor_key = |tail: u8| vec![0x1b, if app_cursor { b'O' } else { b'[' }, tail];
    // Before the Ctrl early return: Ctrl+F5 is `ESC[15;5~`, not "Ctrl+non-letter => nothing".
    if let Some(code) = fkey_code(key) {
        // Cmd+F-key is left for app bindings, not forwarded.
        return (!mods.command).then(|| fkey_bytes(code, mods));
    }
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
        Key::Home => cursor_key(b'H'),
        Key::End => cursor_key(b'F'),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::ArrowUp => cursor_key(b'A'),
        Key::ArrowDown => cursor_key(b'B'),
        Key::ArrowRight if mods.alt => b"\x1bf".to_vec(), // forward word (readline)
        Key::ArrowRight if mods.command => vec![0x05],    // Ctrl-E: end of line
        Key::ArrowRight => cursor_key(b'C'),
        Key::ArrowLeft if mods.alt => b"\x1bb".to_vec(), // backward word (readline)
        Key::ArrowLeft if mods.command => vec![0x01],    // Ctrl-A: start of line
        Key::ArrowLeft => cursor_key(b'D'),
        _ => return None,
    };
    Some(bytes)
}

/// How an F-key is spelled on the wire in `xterm-256color` (the pty's `TERM`).
#[derive(Clone, Copy)]
enum FKey {
    /// F1-F4: `ESC O <byte>` bare, `ESC [ 1 ; <mod> <byte>` modified.
    Letter(u8),
    /// F5-F12: `ESC [ <n> ~` bare, `ESC [ <n> ; <mod> ~` modified.
    Tilde(u8),
}

/// The wire form of F1-F12, or `None` for any other key. F13+ stay unmapped on purpose: F13 is
/// the usual quake-summon key, which the global hotkey takes before the terminal sees it.
fn fkey_code(key: egui::Key) -> Option<FKey> {
    use egui::Key;
    Some(match key {
        Key::F1 => FKey::Letter(b'P'),
        Key::F2 => FKey::Letter(b'Q'),
        Key::F3 => FKey::Letter(b'R'),
        Key::F4 => FKey::Letter(b'S'),
        Key::F5 => FKey::Tilde(15),
        Key::F6 => FKey::Tilde(17),
        Key::F7 => FKey::Tilde(18),
        Key::F8 => FKey::Tilde(19),
        Key::F9 => FKey::Tilde(20),
        Key::F10 => FKey::Tilde(21),
        Key::F11 => FKey::Tilde(23),
        Key::F12 => FKey::Tilde(24),
        _ => return None,
    })
}

/// Terminfo `kf1..kf12` plus the modified `kf13..` forms. Modifier parameter is
/// 1 + shift + 2*alt + 4*ctrl; a bare press (parameter 1) omits it.
fn fkey_bytes(code: FKey, mods: egui::Modifiers) -> Vec<u8> {
    let param = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    match (code, param) {
        (FKey::Letter(l), 1) => vec![0x1b, b'O', l],
        (FKey::Letter(l), p) => format!("\x1b[1;{p}{}", l as char).into_bytes(),
        (FKey::Tilde(n), 1) => format!("\x1b[{n}~").into_bytes(),
        (FKey::Tilde(n), p) => format!("\x1b[{n};{p}~").into_bytes(),
    }
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
pub fn ctrl_letter(key: egui::Key) -> Option<u8> {
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
pub fn alt_scroll_bytes(lines: i32) -> Vec<u8> {
    let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
    seq.repeat(lines.unsigned_abs() as usize)
}

#[cfg(test)]
mod tests {
    use egui::{Key, Modifiers};

    use super::{alt_scroll_bytes, ctrl_letter, key_to_bytes};

    fn mods(ctrl: bool, alt: bool, command: bool) -> Modifiers {
        Modifiers { alt, ctrl, shift: false, mac_cmd: command, command }
    }

    /// Expected bytes are the `xterm-256color` terminfo `kf1..kf12` (plain) and the shifted /
    /// ctrl / alt variants (`kf13..`), since that is the TERM the pty advertises.
    #[test]
    fn function_keys_match_xterm_terminfo() {
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let alt = Modifiers { alt: true, ..Modifiers::default() };
        let ctrl = Modifiers { ctrl: true, ..Modifiers::default() };
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::default() };
        let ctrl_alt_shift =
            Modifiers { ctrl: true, alt: true, shift: true, ..Modifiers::default() };
        let shift_alt = Modifiers { shift: true, alt: true, ..Modifiers::default() };
        let ctrl_alt = Modifiers { ctrl: true, alt: true, ..Modifiers::default() };
        let none = Modifiers::default();
        let cases: [(Key, Modifiers, &[u8]); 23] = [
            (Key::F1, none, b"\x1bOP"),
            (Key::F2, none, b"\x1bOQ"),
            (Key::F3, none, b"\x1bOR"),
            (Key::F4, none, b"\x1bOS"),
            (Key::F5, none, b"\x1b[15~"),
            (Key::F6, none, b"\x1b[17~"),
            (Key::F7, none, b"\x1b[18~"),
            (Key::F8, none, b"\x1b[19~"),
            (Key::F9, none, b"\x1b[20~"),
            (Key::F10, none, b"\x1b[21~"), // htop "Quit"
            (Key::F12, none, b"\x1b[24~"),
            (Key::F1, shift, b"\x1b[1;2P"),
            (Key::F5, shift, b"\x1b[15;2~"),
            (Key::F5, ctrl, b"\x1b[15;5~"),
            (Key::F10, alt, b"\x1b[21;3~"),
            (Key::F11, none, b"\x1b[23~"),
            (Key::F1, ctrl, b"\x1b[1;5P"),        // kf25
            (Key::F4, ctrl, b"\x1b[1;5S"),        // kf28
            (Key::F1, ctrl_shift, b"\x1b[1;6P"),  // kf37
            (Key::F5, ctrl_shift, b"\x1b[15;6~"), // kf41
            (Key::F5, ctrl_alt_shift, b"\x1b[15;8~"),
            (Key::F2, shift_alt, b"\x1b[1;4Q"),
            (Key::F11, ctrl_alt, b"\x1b[23;7~"),
        ];
        for (key, m, want) in cases {
            assert_eq!(key_to_bytes(key, m, false, false), Some(want.to_vec()), "{key:?} {m:?}");
        }
        // Cmd+F-key (with or without other modifiers) is left for app bindings, not forwarded.
        assert_eq!(key_to_bytes(Key::F10, mods(false, false, true), false, false), None);
        let cmd_shift = Modifiers { shift: true, ..mods(false, false, true) };
        assert_eq!(key_to_bytes(Key::F10, cmd_shift, false, false), None);
        assert_eq!(key_to_bytes(Key::F10, mods(true, false, true), false, false), None);
        // altIsMeta and DECCKM (arrow-only) must not change F-key bytes.
        assert_eq!(key_to_bytes(Key::F1, none, true, true), Some(b"\x1bOP".to_vec()));
        assert_eq!(key_to_bytes(Key::F10, alt, true, false), Some(b"\x1b[21;3~".to_vec()));
    }

    #[test]
    fn key_to_bytes_plain_and_ctrl() {
        assert_eq!(
            key_to_bytes(Key::Enter, mods(false, false, false), false, false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, false, false), false, false),
            Some(vec![0x7f])
        );
        assert_eq!(key_to_bytes(Key::C, mods(true, false, false), false, false), Some(vec![3])); // Ctrl-C SIGINT
        assert_eq!(key_to_bytes(Key::Enter, mods(true, false, false), false, false), None); // Ctrl+non-letter
        assert_eq!(key_to_bytes(Key::F13, mods(false, false, false), false, false), None); // unmapped
        // Option+Enter -> meta+Return (ESC+CR): apps read it as "insert newline", not "submit".
        assert_eq!(
            key_to_bytes(Key::Enter, mods(false, true, false), false, false),
            Some(vec![0x1b, b'\r'])
        );
        // Cmd+Alt+Enter stays an app pane binding (maximize), not forwarded.
        assert_eq!(key_to_bytes(Key::Enter, mods(false, true, true), false, false), None);
        // Shift+Enter -> bare LF ("insert newline"); distinct from plain Enter's CR ("submit")
        // and Option+Enter's ESC+CR. All three coexist.
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::Enter, shift, false, false), Some(vec![b'\n']));
        assert_eq!(key_to_bytes(Key::Enter, Modifiers::default(), false, false), Some(vec![b'\r']));
    }

    #[test]
    fn key_to_bytes_natural_editing() {
        // Option (alt) + arrows -> word motion; Cmd + arrows -> line ends.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, true, false), false, false),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, true, false), false, false),
            Some(b"\x1bf".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, false, true), false, false),
            Some(vec![0x01])
        );
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, false, true), false, false),
            Some(vec![0x05])
        );
        // Plain arrows keep the CSI sequences.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, false, false), false, false),
            Some(b"\x1b[D".to_vec())
        );
        // Backspace variants.
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, true, false), false, false),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::Backspace, mods(false, false, true), false, false),
            Some(vec![0x15])
        );
    }

    #[test]
    fn arrows_and_home_end_follow_application_cursor_mode() {
        let none = mods(false, false, false);
        // DECCKM off -> CSI (ESC [ x); on -> SS3 (ESC O x). Covers Up/Down/Left/Right + Home/End.
        for (key, tail) in [
            (Key::ArrowUp, b'A'),
            (Key::ArrowDown, b'B'),
            (Key::ArrowRight, b'C'),
            (Key::ArrowLeft, b'D'),
            (Key::Home, b'H'),
            (Key::End, b'F'),
        ] {
            assert_eq!(
                key_to_bytes(key, none, false, false),
                Some(vec![0x1b, b'[', tail]),
                "{key:?} CSI"
            );
            assert_eq!(
                key_to_bytes(key, none, false, true),
                Some(vec![0x1b, b'O', tail]),
                "{key:?} SS3"
            );
        }
        // Modified arrows (readline word/line motions) are mode-independent - never SS3.
        assert_eq!(
            key_to_bytes(Key::ArrowLeft, mods(false, true, false), false, true),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::ArrowRight, mods(false, false, true), false, true),
            Some(vec![0x05])
        );
    }

    #[test]
    fn alt_is_meta_sends_esc_prefixed_letters() {
        let alt = mods(false, true, false);
        assert_eq!(key_to_bytes(Key::B, alt, true, false), Some(vec![0x1b, b'b']));
        assert_eq!(key_to_bytes(Key::Num3, alt, true, false), Some(vec![0x1b, b'3'])); // digits too
        // Off: unmapped (macOS composes a Text event instead).
        assert_eq!(key_to_bytes(Key::B, alt, false, false), None);
        // Word-motion arrows unchanged even with altIsMeta.
        assert_eq!(key_to_bytes(Key::ArrowLeft, alt, true, false), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn cmd_shift_arrows_are_reserved_for_move_tab() {
        let m = egui::Modifiers { shift: true, command: true, mac_cmd: true, ..Default::default() };
        assert_eq!(key_to_bytes(Key::ArrowLeft, m, false, false), None);
        assert_eq!(key_to_bytes(Key::ArrowRight, m, false, false), None);
    }

    #[test]
    fn alt_scroll_sends_ss3_arrows_per_line() {
        assert_eq!(alt_scroll_bytes(1), b"\x1bOA".to_vec()); // wheel up = up arrow
        assert_eq!(alt_scroll_bytes(-1), b"\x1bOB".to_vec());
        assert_eq!(alt_scroll_bytes(3), b"\x1bOA\x1bOA\x1bOA".to_vec());
        assert_eq!(alt_scroll_bytes(-2), b"\x1bOB\x1bOB".to_vec());
        assert_eq!(alt_scroll_bytes(0), [] as [u8; 0]);
    }

    #[test]
    fn ctrl_shift_arrows_send_nothing_to_the_pty() {
        // Ctrl+Shift+Up/Down are the line-step scroll hotkeys (Tabby default binding) -
        // the ctrl branch maps arrows to None, so no reservation is needed.
        let m = Modifiers { ctrl: true, shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::ArrowUp, m, false, false), None);
        assert_eq!(key_to_bytes(Key::ArrowDown, m, false, false), None);
    }

    #[test]
    fn shift_tab_is_back_tab() {
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(key_to_bytes(Key::Tab, shift, false, false), Some(b"\x1b[Z".to_vec())); // apps cycle on this
        assert_eq!(key_to_bytes(Key::Tab, Modifiers::default(), false, false), Some(vec![b'\t'])); // plain tab
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
        assert_eq!(key_to_bytes(Key::Delete, none, false, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_to_bytes(Key::Insert, none, false, false), Some(b"\x1b[2~".to_vec()));
        assert_eq!(key_to_bytes(Key::Home, none, false, false), Some(b"\x1b[H".to_vec()));
        assert_eq!(key_to_bytes(Key::End, none, false, false), Some(b"\x1b[F".to_vec()));
        assert_eq!(key_to_bytes(Key::PageUp, none, false, false), Some(b"\x1b[5~".to_vec()));
        assert_eq!(key_to_bytes(Key::PageDown, none, false, false), Some(b"\x1b[6~".to_vec()));
        // Shift variants are app scrollback bindings - reserved.
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        for k in [Key::Home, Key::End, Key::PageUp, Key::PageDown] {
            assert_eq!(key_to_bytes(k, shift, false, false), None, "{k:?} must stay an app bind");
        }
    }

    #[test]
    fn cmd_alt_combos_are_reserved_for_panes() {
        // Cmd+Alt+arrows/Enter are app pane bindings; they must not reach the pty.
        assert_eq!(key_to_bytes(Key::ArrowLeft, mods(false, true, true), false, false), None);
        assert_eq!(key_to_bytes(Key::ArrowRight, mods(false, true, true), false, false), None);
        assert_eq!(key_to_bytes(Key::ArrowUp, mods(false, true, true), false, false), None);
        assert_eq!(key_to_bytes(Key::ArrowDown, mods(false, true, true), false, false), None);
        assert_eq!(key_to_bytes(Key::Enter, mods(false, true, true), false, false), None);
    }
}
