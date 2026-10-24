//! What the mouse buttons do on a terminal pane besides select and report: a middle click pastes,
//! and a right click opens the menu, pastes, or copies, by `terminal.right_click`.

use stdusk_core::paste::{self, PasteRules};
use stdusk_core::terminal::PtyTerm;

/// The raw byte a Ctrl+V keypress sends (matches `keys::ctrl_letter(Key::V)`). Forwarding it makes
/// an app that reads the system clipboard on ^V - Claude Code's image ingestion - pick up a
/// pasted image without us encoding anything.
pub const CTRL_V: u8 = 0x16;

/// A resolved mouse-paste payload. Text pastes normally; `Image` means the clipboard held an
/// image and no usable text, so we forward `CTRL_V` for app-native image ingestion.
///
/// NOTE: Cmd+V can NOT reach the image path. egui-winit 0.35 (`lib.rs` ~1007-1015) folds Cmd+V
/// into `Event::Paste` reading clipboard TEXT only, pushes no event when that text is empty, and
/// always swallows the key event - so an image-only clipboard on Cmd+V yields no egui event at
/// all. Ctrl+V already works (it forwards `CTRL_V` via `key_to_bytes`); these mouse paths add a
/// pointer route with the same behavior.
#[derive(Debug, PartialEq)]
pub enum ClipboardPaste {
    Text(String),
    Image,
}

/// What a mouse paste put into the pane, for the host's toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MousePasted {
    Text,
    Image,
}

/// Pure paste-target decision: prefer text, fall back to an image only when there's no usable
/// text (a screenshot copy carries none). Split from clipboard IO so it's unit-testable.
pub fn resolve_clipboard_paste(text: Option<String>, has_image: bool) -> Option<ClipboardPaste> {
    match text {
        Some(t) => Some(ClipboardPaste::Text(t)),
        None if has_image => Some(ClipboardPaste::Image),
        None => None,
    }
}

/// Read the system clipboard for a mouse paste. Probes the image only when there's no text (the
/// image decode is wasted work otherwise).
pub fn read_clipboard_paste() -> Option<ClipboardPaste> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let text = cb.get_text().ok().filter(|t| !t.is_empty());
    let has_image = text.is_none() && cb.get_image().is_ok();
    resolve_clipboard_paste(text, has_image)
}

/// Put a mouse paste into the pane through the same normalize/trim pipeline as a keyboard paste;
/// skips the multiline confirmation like Tabby (both are immediate mouse actions).
pub fn paste_clipboard(term: &mut PtyTerm, clip: ClipboardPaste, rules: PasteRules) -> MousePasted {
    let pasted = match clip {
        ClipboardPaste::Text(text) => {
            let flat = paste::normalize(&text, rules.newlines_to_spaces);
            term.paste(&paste::trim(&flat, rules.trim));
            MousePasted::Text
        }
        ClipboardPaste::Image => {
            term.send(&[CTRL_V]);
            MousePasted::Image
        }
    };
    term.scroll_to_bottom();
    pasted
}

/// `terminal.right_click` parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightClickMode {
    Menu,
    Paste,
    Clipboard,
}

/// Parse the config `right_click` string; unknown values fall back to `Menu` (the default).
pub fn right_click_mode(s: &str) -> RightClickMode {
    match s.to_ascii_lowercase().as_str() {
        "paste" => RightClickMode::Paste,
        "clipboard" => RightClickMode::Clipboard,
        _ => RightClickMode::Menu,
    }
}

/// What a right-click release does on a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightClickAction {
    Menu,
    Paste,
    Copy,
}

/// Tabby's exact right-click semantics (`baseTerminalTab` handleRightMouseUp): paste/clipboard
/// act only on a quick tap (held < 250ms); a hold falls back to the context menu. Clipboard
/// copies when a selection exists, else pastes.
pub fn right_click_action(
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn text_wins_even_when_an_image_is_also_present() {
        assert_eq!(
            resolve_clipboard_paste(Some("hi".into()), true),
            Some(ClipboardPaste::Text("hi".into())),
        );
    }

    #[test]
    fn image_used_only_when_there_is_no_text() {
        assert_eq!(resolve_clipboard_paste(None, true), Some(ClipboardPaste::Image));
    }

    #[test]
    fn empty_clipboard_pastes_nothing() {
        assert_eq!(resolve_clipboard_paste(None, false), None);
    }
}
