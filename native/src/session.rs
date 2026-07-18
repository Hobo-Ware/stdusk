//! Session restore: remember each open tab (cwd, rename, color) in
//! `~/.config/stdusk/session.toml` and reopen them on launch (Tabby's `recoverTabs`).
//! The encode/decode is pure and unit-tested; saving is throttled by the caller.
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct SavedSession {
    #[serde(default)]
    pub(crate) tabs: Vec<SavedTab>,
    #[serde(default)]
    pub(crate) active: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct SavedTab {
    /// Custom title, only present when the user renamed the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// Tab color as `#rrggbb`, only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
}

pub(crate) fn color_to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub(crate) fn hex_to_color(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config/stdusk/session.toml"))
}

/// Load the saved session, or an empty one when absent/corrupt (never fails the launch).
pub(crate) fn load() -> SavedSession {
    let Some(p) = path() else { return SavedSession::default() };
    std::fs::read_to_string(p).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

/// Persist the session (best-effort; a failed write is not worth interrupting the user).
pub(crate) fn save(s: &SavedSession) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = toml::to_string(s) {
        let _ = std::fs::write(p, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_through_toml() {
        let s = SavedSession {
            tabs: vec![
                SavedTab {
                    title: Some("build".into()),
                    color: Some("#e06c75".into()),
                    cwd: Some("/tmp".into()),
                },
                SavedTab { title: None, color: None, cwd: Some("/home/x".into()) },
            ],
            active: 1,
        };
        let body = toml::to_string(&s).unwrap();
        let back: SavedSession = toml::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn corrupt_or_missing_session_is_empty() {
        let s: Result<SavedSession, _> = toml::from_str("not [valid");
        assert!(s.is_err()); // load() maps this to default
        assert_eq!(SavedSession::default().tabs.len(), 0);
    }

    #[test]
    fn color_hex_round_trip() {
        let c = Color32::from_rgb(0xe0, 0x6c, 0x75);
        assert_eq!(color_to_hex(c), "#e06c75");
        assert_eq!(hex_to_color("#e06c75"), Some(c));
        assert_eq!(hex_to_color("nope"), None);
        assert_eq!(hex_to_color("#fff"), None); // short form unsupported on purpose
    }
}
