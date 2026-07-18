//! User config from `~/.config/stdusk/config.toml`. Missing file or fields fall back to
//! defaults (chosen to match Tabby where applicable). Also parses the quake hotkey string.
use std::str::FromStr;

use global_hotkey::hotkey::{Code, Modifiers};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) appearance: Appearance,
    pub(crate) quake: Quake,
    pub(crate) terminal: Terminal,
    pub(crate) session: Session,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Session {
    pub(crate) restore: bool, // reopen last session's tabs (cwd/title/color) on launch
}

impl Default for Session {
    fn default() -> Self {
        Self { restore: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Appearance {
    pub(crate) theme: String, // used when follow_system = false
    pub(crate) opacity: f32,
    pub(crate) font_size: f32,
    pub(crate) follow_system: bool, // pick theme_light/theme_dark by the OS appearance
    pub(crate) theme_light: String,
    pub(crate) theme_dark: String,
}

#[allow(clippy::struct_excessive_bools)] // independent quake toggles, not a mode
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Quake {
    pub(crate) hotkey: String,
    pub(crate) height_pct: f32,
    pub(crate) hide_on_focus_loss: bool,
    /// Run as a macOS accessory app: no Dock icon, no app-switcher/menu-bar entry - it just drops
    /// from the top on the hotkey (the quake default). Set false to appear as a normal Dock app.
    pub(crate) hide_from_dock: bool,
    /// Show a menu-bar (status) icon with a Show/Hide + Quit menu. The accessory app's main entry
    /// point + presence indicator; set false to hide it.
    pub(crate) menu_bar_icon: bool,
    /// (With hide_from_dock) show the Dock icon + a real menu bar *while the window is visible*,
    /// flipping back to accessory when it's hidden. Off by default (pure accessory - no Dock, and
    /// the menu bar belongs to whatever other app is frontmost).
    pub(crate) dock_when_visible: bool,
}

// Independent user toggles, not a mode - a state machine would be more code, not less.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Terminal {
    pub(crate) detect_progress: bool,
    pub(crate) cursor: String,          // "block" | "underline" | "beam"
    pub(crate) shell_integration: bool, // inject OSC 133 hooks into the spawned shell
    pub(crate) bell: String,            // "visual" | "off"
    pub(crate) detect_clis: bool,       // badge tabs running a known AI CLI (claude/gemini/...)
    pub(crate) clickable_links: bool,   // open URLs / file paths on click
    pub(crate) link_modifier: String,   // "none" (hover) | "cmd" | "ctrl" | "alt" | "shift"
    pub(crate) notify_on_done: bool, // desktop notification when a long (>10s) command finishes while hidden
    pub(crate) scrollback_lines: usize, // history size (Tabby default 25000)
    pub(crate) cursor_blink: bool,   // blink the focused pane's cursor
    pub(crate) alt_is_meta: bool,    // Option+key sends ESC+key instead of composed chars
    pub(crate) word_separators: String, // chars that end a double-click word selection
    pub(crate) copy_on_select: bool, // copy to clipboard whenever a selection finishes
    pub(crate) paste_on_middle_click: bool, // middle-click pastes the clipboard
    pub(crate) warn_on_multiline_paste: bool, // confirm before pasting multiple lines
    pub(crate) trim_whitespace_on_paste: bool, // strip leading/trailing whitespace from pastes
    pub(crate) replace_newlines_on_paste: bool, // newlines -> spaces on paste
    pub(crate) bold_bright: bool,    // draw bold text in the bright ANSI colors
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "one-half-dark".into(),
            opacity: 0.85,
            font_size: 13.0,
            follow_system: true,
            theme_light: "one-half-light".into(),
            theme_dark: "one-half-dark".into(),
        }
    }
}
impl Default for Quake {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Grave".into(),
            height_pct: 0.5,
            hide_on_focus_loss: true,
            hide_from_dock: true,
            menu_bar_icon: true,
            dock_when_visible: false,
        }
    }
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            detect_progress: true,
            cursor: "block".into(),
            shell_integration: true,
            bell: "visual".into(),
            detect_clis: true,
            clickable_links: true,
            link_modifier: "none".into(),
            notify_on_done: true,
            scrollback_lines: 25000,
            cursor_blink: true,
            alt_is_meta: false,
            word_separators: " ()[]{}'\"".into(),
            copy_on_select: false,
            paste_on_middle_click: true,
            warn_on_multiline_paste: true,
            trim_whitespace_on_paste: true,
            replace_newlines_on_paste: false,
            bold_bright: true,
        }
    }
}

impl Config {
    pub(crate) fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("stdusk: config parse error ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(), // no file - defaults
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|home| std::path::Path::new(&home).join(".config/stdusk/config.toml"))
}

/// Return the config path, creating it (with the example content) if it doesn't exist.
/// Used by the settings gear so "open config" always opens something.
pub(crate) fn ensure_and_path() -> Option<std::path::PathBuf> {
    let p = config_path()?;
    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, include_str!("../config.example.toml"));
    }
    Some(p)
}

/// Parse a hotkey string like "Ctrl+Grave", "F13", "Cmd+Grave", "Ctrl+Shift+T" into
/// (modifiers, key). Falls back to Ctrl+Grave on anything unparseable.
pub(crate) fn parse_hotkey(s: &str) -> (Option<Modifiers>, Code) {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return (Some(Modifiers::CONTROL), Code::Backquote);
    }
    let mut mods = Modifiers::empty();
    for m in &parts[..parts.len() - 1] {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "cmd" | "command" | "super" | "meta" | "win" => mods |= Modifiers::META,
            "alt" | "option" | "opt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            _ => {}
        }
    }
    let code = parse_code(parts[parts.len() - 1]).unwrap_or(Code::Backquote);
    let mods = if mods.is_empty() { None } else { Some(mods) };
    (mods, code)
}

/// Map a friendly key name to a W3C key `Code`.
fn parse_code(k: &str) -> Option<Code> {
    let lower = k.to_ascii_lowercase();
    let w3c = match lower.as_str() {
        "`" | "grave" | "backquote" => "Backquote".to_string(),
        "space" => "Space".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphabetic() => {
            format!("Key{}", s.to_ascii_uppercase())
        }
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_digit() => {
            format!("Digit{s}")
        }
        s if s.starts_with('f') && s[1..].parse::<u8>().is_ok() => {
            format!("F{}", &s[1..]) // f13 -> F13
        }
        _ => k.to_string(), // assume already a W3C code name
    };
    Code::from_str(&w3c).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_tabby_baseline() {
        let c = Config::default();
        assert_eq!(c.appearance.theme, "one-half-dark");
        assert_eq!(c.appearance.opacity, 0.85);
        assert_eq!(c.quake.hotkey, "Ctrl+Grave");
        assert!(c.quake.hide_on_focus_loss);
        assert!(c.terminal.detect_progress);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: Config = toml::from_str("[appearance]\ntheme = \"dracula\"\n").unwrap();
        assert_eq!(c.appearance.theme, "dracula");
        assert_eq!(c.appearance.opacity, 0.85); // default preserved
        assert_eq!(c.quake.hotkey, "Ctrl+Grave"); // default section
    }

    #[test]
    fn hotkey_parsing() {
        assert_eq!(parse_hotkey("Ctrl+Grave"), (Some(Modifiers::CONTROL), Code::Backquote));
        assert_eq!(parse_hotkey("F13"), (None, Code::F13));
        assert_eq!(parse_hotkey("Cmd+Grave"), (Some(Modifiers::META), Code::Backquote));
        assert_eq!(parse_hotkey("F12"), (None, Code::F12));
        assert_eq!(
            parse_hotkey("Ctrl+Shift+T"),
            (Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyT)
        );
    }

    #[test]
    fn garbage_hotkey_falls_back() {
        assert_eq!(parse_hotkey(""), (Some(Modifiers::CONTROL), Code::Backquote));
        assert_eq!(parse_hotkey("nonsense"), (None, Code::Backquote));
    }
}
