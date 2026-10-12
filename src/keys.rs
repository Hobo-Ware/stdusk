//! The `[hotkeys]` chord parser/matcher (`parse_hotkey_spec`, `key_from_name`, `hotkey_matches`)
//! and the per-frame hotkey dispatch. Pure functions, unit-testable without a window.
use eframe::egui;

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

/// F1-F20, the keys `[hotkeys]` may bind bare. egui-winit sends no `Event::Text` for any of them.
const FUNCTION_KEYS: [egui::Key; 20] = [
    egui::Key::F1,
    egui::Key::F2,
    egui::Key::F3,
    egui::Key::F4,
    egui::Key::F5,
    egui::Key::F6,
    egui::Key::F7,
    egui::Key::F8,
    egui::Key::F9,
    egui::Key::F10,
    egui::Key::F11,
    egui::Key::F12,
    egui::Key::F13,
    egui::Key::F14,
    egui::Key::F15,
    egui::Key::F16,
    egui::Key::F17,
    egui::Key::F18,
    egui::Key::F19,
    egui::Key::F20,
];

/// Friendly key name -> `egui::Key`. Case-insensitive; accepts punctuation literals.
#[allow(clippy::too_many_lines)] // a flat name table; splitting it would obscure it
fn key_from_name(name: &str) -> Option<egui::Key> {
    use egui::Key;
    let n = name.to_ascii_lowercase();
    // F-keys first so "f1" doesn't fall into the single-letter branch.
    if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
        return (1..=20).contains(&num).then(|| FUNCTION_KEYS[usize::from(num) - 1]);
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
        "[" | "openbracket" => Key::OpenBracket,
        "]" | "closebracket" => Key::CloseBracket,
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
/// where the same key reports either; likewise egui-winit reports Shift+[ as the logical `{`, so
/// curly brackets fold onto their square keys. NOTE (macOS): `mods.command` is Cmd, `mods.ctrl` is the
/// real Ctrl - the comparison relies on that split.
pub(crate) fn hotkey_matches(spec: &str, key: egui::Key, mods: egui::Modifiers) -> bool {
    let Some((want_mods, want_key)) = parse_hotkey_spec(spec) else {
        return false;
    };
    let key = match key {
        egui::Key::Plus => egui::Key::Equals,
        egui::Key::OpenCurlyBracket => egui::Key::OpenBracket,
        egui::Key::CloseCurlyBracket => egui::Key::CloseBracket,
        other => other,
    };
    key == want_key
        && mods.command == want_mods.command
        && mods.ctrl == want_mods.ctrl
        && mods.alt == want_mods.alt
        && mods.shift == want_mods.shift
}

/// Step direction of a pressed key against a next/prev chord pair: `+1` next, `-1` prev, `None`
/// for neither. When both specs are the same chord, next wins - one press never fires both.
pub(crate) fn cycle_dir(
    next_spec: &str,
    prev_spec: &str,
    key: egui::Key,
    mods: egui::Modifiers,
) -> Option<i32> {
    if hotkey_matches(next_spec, key, mods) {
        Some(1)
    } else if hotkey_matches(prev_spec, key, mods) {
        Some(-1)
    } else {
        None
    }
}

/// An app action a `[hotkeys]` chord fires. Some variants cover a pair or trio of binds
/// (`TabCycle`, `Split`, `Zoom`, `RepoCycle`) and carry which one fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HotkeyAction {
    Palette,
    Settings,
    TabCycle(i32),
    NewTab,
    Close,
    Reopen,
    ToggleLastTab,
    Find,
    Split(stdusk_core::pane::SplitDir),
    Broadcast,
    SelectAll,
    Clear,
    Zoom(i8),
    RepoCycle(i32),
}

/// Which modal state lets a chord dispatch. Sampled once per frame by the render loop.
// Four independent per-frame facts sampled from the app state, not one state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct HotkeyGuards {
    /// A text modal (rename, paste confirm, close/quit confirm) is up: no bind fires.
    pub(crate) text_modal: bool,
    /// Any modal or the settings view is up: only palette, settings and tab cycling fire.
    pub(crate) hard_modal: bool,
    /// Tab switching is allowed (no modal, or only the settings view is up).
    pub(crate) can_switch_tabs: bool,
    /// Repo grouping is on (`appearance.group_by_repo`). Off, the repo chords do nothing, so
    /// they stay with the shell.
    pub(crate) repo_grouping: bool,
}

/// One pressed key with the modifiers held at that moment.
pub(crate) type Chord = (egui::Key, egui::Modifiers);

/// The action a pressed chord fires under the current guards, or `None` when no bind matches
/// or a guard stops it. First match wins, so one chord bound twice fires only the earlier action.
pub(crate) fn hotkey_action(
    hk: &crate::config::Hotkeys,
    key: egui::Key,
    mods: egui::Modifiers,
    g: HotkeyGuards,
) -> Option<HotkeyAction> {
    use HotkeyAction as A;
    let is = |spec: &str| hotkey_matches(spec, key, mods);
    if !g.text_modal && is(&hk.palette) {
        return Some(A::Palette);
    }
    if !g.text_modal && is(&hk.settings) {
        return Some(A::Settings);
    }
    if g.can_switch_tabs
        && let Some(d) = cycle_dir(&hk.next_tab, &hk.prev_tab, key, mods)
    {
        return Some(A::TabCycle(d));
    }
    if g.hard_modal {
        return None;
    }
    let binds = [
        (&hk.new_tab, A::NewTab),
        (&hk.close, A::Close),
        (&hk.reopen, A::Reopen),
        (&hk.toggle_last_tab, A::ToggleLastTab),
        (&hk.find, A::Find),
        (&hk.split_right, A::Split(stdusk_core::pane::SplitDir::Row)),
        (&hk.split_down, A::Split(stdusk_core::pane::SplitDir::Column)),
        (&hk.broadcast, A::Broadcast),
        (&hk.select_all, A::SelectAll),
        (&hk.clear, A::Clear),
        (&hk.zoom_in, A::Zoom(1)),
        (&hk.zoom_out, A::Zoom(-1)),
        (&hk.zoom_reset, A::Zoom(0)),
    ];
    binds.into_iter().find_map(|(spec, action)| is(spec).then_some(action)).or_else(|| {
        // Grouping off: the cycle would do nothing, so leave the key to the shell.
        cycle_dir(&hk.next_repo, &hk.prev_repo, key, mods)
            .filter(|_| g.repo_grouping)
            .map(A::RepoCycle)
    })
}

/// Every key press in `events` that fires an app action, in event order, with its chord.
/// Read-only: the caller applies the actions, then hands the result to
/// `consume_hotkey_events`.
pub(crate) fn matched_hotkeys(
    events: &[egui::Event],
    hk: &crate::config::Hotkeys,
    g: HotkeyGuards,
) -> Vec<(HotkeyAction, Chord)> {
    events
        .iter()
        .filter_map(|ev| {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = ev else {
                return None;
            };
            let seen = stdusk_egui::keys::terminal_modifiers(*modifiers);
            hotkey_action(hk, *key, seen, g).map(|a| (a, (*key, *modifiers)))
        })
        .collect()
}

/// Remove the key presses that fired an app action, so `collect_input` never turns them into
/// pty bytes. Ghostty, kitty and Alacritty also keep a matched bind from the shell. Matches the
/// exact chord (egui's own `consume_key` ignores extra Shift and Alt, so a bare `F5` bind would
/// also eat Shift+F5). An Alt chord also loses the `Event::Text` that follows it: egui-winit
/// pushes the composed character (Option+K gives a symbol) right after the key.
pub(crate) fn consume_hotkey_events(
    events: &mut Vec<egui::Event>,
    matched: &[(HotkeyAction, Chord)],
) {
    // True right after a consumed Alt press that has composed text. egui-winit pushes a press's
    // `Key` event before its `Text` (lib.rs, key handler), so the next Text belongs to it.
    let mut drop_text = false;
    events.retain(|ev| match ev {
        egui::Event::Key { key, pressed: true, modifiers, .. }
            if matched.iter().any(|(_, chord)| *chord == (*key, *modifiers)) =>
        {
            // No Text exists under Ctrl or Cmd (egui-winit skips it), nor for F-keys. Arming the
            // flag there would eat an unrelated Text.
            drop_text = modifiers.alt
                && !modifiers.ctrl
                && !modifiers.command
                && !FUNCTION_KEYS.contains(key);
            false
        }
        egui::Event::Text(_) if drop_text => {
            drop_text = false;
            false
        }
        _ => {
            drop_text = false;
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui::{Key, Modifiers};

    use super::HotkeyAction as A;
    use super::{
        HotkeyAction, HotkeyGuards, consume_hotkey_events, cycle_dir, hotkey_matches,
        matched_hotkeys, parse_hotkey_spec,
    };
    use crate::config::Hotkeys;
    use stdusk_core::pane::SplitDir;
    use stdusk_egui::keys::key_to_bytes;

    fn mods(ctrl: bool, alt: bool, command: bool) -> Modifiers {
        Modifiers { alt, ctrl, shift: false, mac_cmd: command, command }
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
    fn repo_switch_binds_match_square_or_curly_brackets() {
        let cmd_shift = Modifiers { shift: true, ..mods(false, false, true) };
        let cases = [
            ("Cmd+Shift+]", Key::CloseBracket, true),
            ("Cmd+Shift+]", Key::CloseCurlyBracket, true),
            ("Cmd+Shift+[", Key::OpenBracket, true),
            ("Cmd+Shift+[", Key::OpenCurlyBracket, true),
            ("Cmd+Shift+[", Key::CloseCurlyBracket, false),
        ];
        for (spec, key, want) in cases {
            assert_eq!(hotkey_matches(spec, key, cmd_shift), want, "{spec} vs {key:?}");
        }
        assert!(!hotkey_matches("Cmd+Shift+]", Key::CloseBracket, mods(false, false, true)));
    }

    #[test]
    fn cycle_dir_reads_the_direction_from_the_next_and_prev_chords() {
        let cmd_shift = Modifiers { shift: true, ..mods(false, false, true) };
        let cases = [
            (Key::CloseBracket, Some(1)),
            (Key::CloseCurlyBracket, Some(1)), // egui-winit reports Shift+] as the logical `}`
            (Key::OpenBracket, Some(-1)),
            (Key::OpenCurlyBracket, Some(-1)),
            (Key::T, None),
        ];
        for (key, want) in cases {
            assert_eq!(cycle_dir("Cmd+Shift+]", "Cmd+Shift+[", key, cmd_shift), want, "{key:?}");
        }
    }

    #[test]
    fn cycle_dir_ignores_unbound_and_inexact_chords() {
        let cmd_shift = Modifiers { shift: true, ..mods(false, false, true) };
        assert_eq!(cycle_dir("", "", Key::CloseBracket, cmd_shift), None); // unbound
        assert_eq!(cycle_dir("Cmd+]", "Cmd+[", Key::CloseBracket, cmd_shift), None); // extra Shift
        // Two actions on one chord: next wins, never both.
        assert_eq!(cycle_dir("Cmd+Shift+]", "Cmd+Shift+]", Key::CloseBracket, cmd_shift), Some(1));
    }

    #[test]
    fn default_tab_and_repo_cycle_chords_never_fire_together() {
        let h = crate::config::Hotkeys::default();
        let cmd_shift = Modifiers { shift: true, ..mods(false, false, true) };
        let cmd_ctrl = mods(true, false, true);
        for (key, tab, repo) in
            [(Key::CloseBracket, Some(1), None), (Key::OpenBracket, Some(-1), None)]
        {
            assert_eq!(cycle_dir(&h.next_tab, &h.prev_tab, key, cmd_shift), tab, "{key:?}");
            assert_eq!(cycle_dir(&h.next_repo, &h.prev_repo, key, cmd_shift), repo, "{key:?}");
        }
        for (key, tab, repo) in
            [(Key::CloseBracket, None, Some(1)), (Key::OpenBracket, None, Some(-1))]
        {
            assert_eq!(cycle_dir(&h.next_tab, &h.prev_tab, key, cmd_ctrl), tab, "{key:?}");
            assert_eq!(cycle_dir(&h.next_repo, &h.prev_repo, key, cmd_ctrl), repo, "{key:?}");
        }
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
        // Without Cmd they're ordinary chords. A bind on Ctrl+C is consumed like any other
        // matched chord, so it no longer copies-or-interrupts.
        assert!(parse_hotkey_spec("Ctrl+C").is_some());
        assert!(parse_hotkey_spec("Ctrl+Shift+V").is_some());
    }

    fn key_ev(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// The guards of a plain terminal view: no modal, every bind live.
    fn open_guards() -> HotkeyGuards {
        HotkeyGuards {
            text_modal: false,
            hard_modal: false,
            can_switch_tabs: true,
            repo_grouping: true,
        }
    }

    /// Run the two steps the render loop runs: match the chords, then consume them.
    fn match_and_consume(
        events: Vec<egui::Event>,
        hk: &Hotkeys,
        g: HotkeyGuards,
    ) -> (Vec<HotkeyAction>, Vec<egui::Event>) {
        let matched = matched_hotkeys(&events, hk, g);
        let mut left = events;
        consume_hotkey_events(&mut left, &matched);
        (matched.into_iter().map(|(a, _)| a).collect(), left)
    }

    #[test]
    fn matched_hotkeys_are_consumed_not_sent_to_the_pty() {
        // Ghostty, kitty and Alacritty keep a matched bind from the shell. So does stdusk: the
        // key event leaves the frame, and `collect_input` finds nothing to encode.
        // The DEFAULT binds are Cmd-based, so key_to_bytes sends nothing for them anyway.
        let cmd = mods(false, false, true);
        for (spec, key) in [("Cmd+T", Key::T), ("Cmd+W", Key::W), ("Cmd+O", Key::O)] {
            assert!(hotkey_matches(spec, key, cmd));
            assert_eq!(
                key_to_bytes(key, cmd, false, false),
                None,
                "{spec} must not leak to the pty"
            );
        }
        // A rebind onto a chord the terminal encodes (Ctrl+K = 0x0b, F3 = ESC O R) would leak
        // without consumption. Each row: the bind, the key press, the action it fires.
        let hk = Hotkeys {
            clear: "Ctrl+K".into(),
            find: "F3".into(),
            zoom_in: "Alt+F3".into(),
            ..Hotkeys::default()
        };
        let ctrl = mods(true, false, false);
        let alt = mods(false, true, false);
        let cases = [
            (Key::K, ctrl, HotkeyAction::Clear, vec![11]),
            (Key::F3, Modifiers::default(), HotkeyAction::Find, b"\x1bOR".to_vec()),
            (Key::F3, alt, HotkeyAction::Zoom(1), b"\x1b[1;3R".to_vec()),
        ];
        for (key, m, want, raw) in cases {
            // Without consumption the shell would get these bytes.
            assert_eq!(key_to_bytes(key, m, false, false), Some(raw), "{key:?}");
            let (actions, left) = match_and_consume(vec![key_ev(key, m)], &hk, open_guards());
            assert_eq!(actions, vec![want], "{key:?}");
            assert!(left.is_empty(), "{key:?} must not reach collect_input");
        }
    }

    #[test]
    fn consumption_takes_the_exact_chord_only() {
        // egui's consume_key would treat a bare F5 bind as also matching Shift+F5.
        let hk = Hotkeys { find: "F5".into(), ..Hotkeys::default() };
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        let (actions, left) = match_and_consume(
            vec![key_ev(Key::F5, shift), key_ev(Key::F5, Modifiers::default())],
            &hk,
            open_guards(),
        );
        assert_eq!(actions, vec![HotkeyAction::Find]);
        assert_eq!(left, vec![key_ev(Key::F5, shift)]);
    }

    #[test]
    fn unbound_and_unmatched_keys_stay_in_the_frame() {
        let none = Modifiers::default();
        // Empty string = unbound: never matches, so the key belongs to the shell again.
        let hk = Hotkeys { find: String::new(), ..Hotkeys::default() };
        let events =
            vec![key_ev(Key::F3, none), egui::Event::Text("a".into()), key_ev(Key::A, none)];
        let (actions, left) = match_and_consume(events.clone(), &hk, open_guards());
        assert_eq!(actions, []);
        assert_eq!(left, events);
    }

    #[test]
    fn a_guarded_hotkey_is_neither_dispatched_nor_consumed() {
        // (guards, chord bound to, key press, dispatched?). A modal that stops the action
        // must leave the key alone: input_captured already gates the pty while it is up.
        let hk = Hotkeys {
            new_tab: "F6".into(),
            palette: "F7".into(),
            next_tab: "F8".into(),
            next_repo: "F9".into(),
            ..Hotkeys::default()
        };
        let none = Modifiers::default();
        let text = HotkeyGuards {
            text_modal: true,
            hard_modal: true,
            can_switch_tabs: false,
            repo_grouping: true,
        };
        let hard = HotkeyGuards { text_modal: false, hard_modal: true, ..text };
        let settings = HotkeyGuards { can_switch_tabs: true, ..hard };
        let no_repos = HotkeyGuards { repo_grouping: false, ..open_guards() };
        let cases = [
            (text, Key::F6, false),
            (text, Key::F7, false),
            (text, Key::F8, false),
            (hard, Key::F6, false), // new_tab is a hard-modal bind
            (hard, Key::F7, true),  // palette toggles stay live outside text modals
            (hard, Key::F8, false), // tab cycle needs can_switch_tabs
            (settings, Key::F6, false),
            (settings, Key::F8, true), // settings behaves like a tab
            (open_guards(), Key::F6, true),
            (open_guards(), Key::F9, true), // repo grouping on: the repo chord cycles
            (no_repos, Key::F9, false),     // grouping off: it would do nothing, so it stays
            (no_repos, Key::F6, true),      // other binds are unaffected
            (hard, Key::F9, false),         // repo cycling obeys hard_modal
        ];
        for (g, key, dispatched) in cases {
            let (actions, left) = match_and_consume(vec![key_ev(key, none)], &hk, g);
            assert_eq!(!actions.is_empty(), dispatched, "{g:?} {key:?}");
            assert_eq!(left.is_empty(), dispatched, "{g:?} {key:?}");
        }
    }

    #[test]
    fn every_remappable_action_is_matched_and_consumed() {
        // One distinct F-key per action; each fires its own action and is consumed.
        let hk = Hotkeys {
            new_tab: "F1".into(),
            close: "F2".into(),
            reopen: "F3".into(),
            toggle_last_tab: "F4".into(),
            find: "F5".into(),
            palette: "F6".into(),
            settings: "F7".into(),
            broadcast: "F8".into(),
            split_right: "F9".into(),
            split_down: "F10".into(),
            select_all: "F11".into(),
            clear: "F12".into(),
            zoom_in: "F13".into(),
            zoom_out: "F14".into(),
            zoom_reset: "F15".into(),
            next_tab: "F16".into(),
            prev_tab: "F17".into(),
            next_repo: "F18".into(),
            prev_repo: "F19".into(),
        };
        let cases = [
            (Key::F1, A::NewTab),
            (Key::F2, A::Close),
            (Key::F3, A::Reopen),
            (Key::F4, A::ToggleLastTab),
            (Key::F5, A::Find),
            (Key::F6, A::Palette),
            (Key::F7, A::Settings),
            (Key::F8, A::Broadcast),
            (Key::F9, A::Split(SplitDir::Row)),
            (Key::F10, A::Split(SplitDir::Column)),
            (Key::F11, A::SelectAll),
            (Key::F12, A::Clear),
            (Key::F13, A::Zoom(1)),
            (Key::F14, A::Zoom(-1)),
            (Key::F15, A::Zoom(0)),
            (Key::F16, A::TabCycle(1)),
            (Key::F17, A::TabCycle(-1)),
            (Key::F18, A::RepoCycle(1)),
            (Key::F19, A::RepoCycle(-1)),
        ];
        for (key, want) in cases {
            let (actions, left) =
                match_and_consume(vec![key_ev(key, Modifiers::default())], &hk, open_guards());
            assert_eq!(actions, vec![want], "{key:?}");
            assert!(left.is_empty(), "{key:?}");
        }
    }

    #[test]
    fn a_chord_with_no_composed_text_leaves_the_next_text_alone() {
        // egui-winit pushes no Text for F-keys (private-use chars are filtered) and none while
        // Ctrl or Cmd is held. The Text after such a chord is another key's, so it must stay.
        let alt = mods(false, true, false);
        let ctrl_alt = mods(true, true, false);
        let hk = Hotkeys {
            zoom_in: "Alt+F3".into(),
            zoom_out: "Alt+F14".into(),
            clear: "Ctrl+Alt+X".into(),
            find: "Cmd+Alt+J".into(),
            ..Hotkeys::default()
        };
        #[allow(unused_mut)]
        let mut cases = vec![
            (Key::F3, alt, HotkeyAction::Zoom(1)),
            (Key::F14, alt, HotkeyAction::Zoom(-1)),
            (Key::X, ctrl_alt, HotkeyAction::Clear),
        ];
        #[cfg(target_os = "macos")]
        cases.push((Key::J, mods(false, true, true), HotkeyAction::Find));
        for (key, m, want) in cases {
            let events = vec![key_ev(key, m), egui::Event::Text("z".into())];
            let (actions, left) = match_and_consume(events, &hk, open_guards());
            assert_eq!(actions, vec![want], "{key:?}");
            assert_eq!(left, vec![egui::Event::Text("z".into())], "{key:?}");
        }
    }

    #[test]
    fn two_matched_chords_in_one_frame_are_both_consumed() {
        let hk = Hotkeys { find: "F3".into(), clear: "Ctrl+K".into(), ..Hotkeys::default() };
        let events = vec![
            key_ev(Key::F3, Modifiers::default()),
            key_ev(Key::Backspace, Modifiers::default()),
            key_ev(Key::K, mods(true, false, false)),
        ];
        let (actions, left) = match_and_consume(events, &hk, open_guards());
        assert_eq!(actions, vec![HotkeyAction::Find, HotkeyAction::Clear]);
        assert_eq!(left, vec![key_ev(Key::Backspace, Modifiers::default())]);
    }

    #[test]
    fn an_alt_chord_takes_its_composed_text_with_it() {
        // egui-winit pushes Text("˚") right after Key(Alt+K) on macOS. Without dropping it the
        // shell would still see the symbol. Text from other keys stays.
        let alt = mods(false, true, false);
        let hk = Hotkeys { clear: "Alt+K".into(), ..Hotkeys::default() };
        let events = vec![
            key_ev(Key::K, alt),
            egui::Event::Text("\u{2da}".into()),
            key_ev(Key::A, Modifiers::default()),
            egui::Event::Text("a".into()),
        ];
        let (actions, left) = match_and_consume(events, &hk, open_guards());
        assert_eq!(actions, vec![HotkeyAction::Clear]);
        assert_eq!(left, vec![key_ev(Key::A, Modifiers::default()), egui::Event::Text("a".into())]);
    }
}
