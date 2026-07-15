//! User config from `~/.config/stdusk/config.toml`. Missing file or fields fall back to
//! defaults (chosen to match Tabby where applicable). Also parses the quake hotkey string.
use std::str::FromStr;

use global_hotkey::hotkey::{Code, Modifiers};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub quake: Quake,
    pub terminal: Terminal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub opacity: f32,
    pub font_size: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Quake {
    pub hotkey: String,
    pub height_pct: f32,
    pub hide_on_focus_loss: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Terminal {
    pub detect_progress: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            quake: Quake::default(),
            terminal: Terminal::default(),
        }
    }
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "one-half-dark".into(),
            opacity: 0.85,
            font_size: 13.0,
        }
    }
}
impl Default for Quake {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Grave".into(),
            height_pct: 0.5,
            hide_on_focus_loss: true,
        }
    }
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            detect_progress: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
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
    std::env::var_os("HOME").map(|home| {
        std::path::Path::new(&home)
            .join(".config/stdusk/config.toml")
    })
}

/// Parse a hotkey string like "Ctrl+Grave", "F13", "Cmd+Grave", "Ctrl+Shift+T" into
/// (modifiers, key). Falls back to Ctrl+Grave on anything unparseable.
pub fn parse_hotkey(s: &str) -> (Option<Modifiers>, Code) {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
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
