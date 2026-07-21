//! Session restore: remember each open tab (cwd, rename, color) in
//! `~/.config/stdusk/session.toml` and reopen them on launch (Tabby's `recoverTabs`).
//! The encode/decode is pure and unit-tested; saving is throttled by the caller.
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

// No `Eq`: `window` carries f32 geometry. Only `PartialEq` is needed (skip-identical-write guard).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct SavedSession {
    #[serde(default)]
    pub(crate) tabs: Vec<SavedTab>,
    #[serde(default)]
    pub(crate) active: usize,
    /// Remembered window geometry, restored on next launch in window mode. Written only in window
    /// mode; dropdown mode leaves it None (it uses the fixed top-edge quake geometry instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<WindowGeom>,
}

/// A window's outer position + inner (content) size, in logical points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowGeom {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

// No `Eq`: `pane` carries an f32 split ratio. Only `PartialEq` is needed (skip-identical-write).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct SavedTab {
    /// Custom title, only present when the user renamed the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// Tab color as `#rrggbb`, only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    /// Pinned flag, only written when set (pinned tabs sort first and guard close).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) pinned: bool,
    /// The tab's split layout at save time (Tabby-style pane tree). Absent -> a single pane (old
    /// sessions predating split-restore, decoded via the flat `cwd`). serde-default keeps old
    /// session files loading unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane: Option<SavedPane>,
}

/// A tab's split layout, persisted so re-open restores every pane (not just the first). Mirrors
/// `pane::Pane`: a `Leaf` (one terminal's cwd) or a `Split` of two children. Backward-compatible
/// via `SavedTab.pane: Option<_>` (absent -> single pane).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum SavedPane {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Split {
        dir: SavedSplitDir,
        ratio: f32,
        a: Box<SavedPane>,
        b: Box<SavedPane>,
    },
}

/// Serializable mirror of `pane::SplitDir` (kept local so `pane.rs` needn't derive serde).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SavedSplitDir {
    Row,
    Column,
}

impl From<crate::pane::SplitDir> for SavedSplitDir {
    fn from(d: crate::pane::SplitDir) -> Self {
        match d {
            crate::pane::SplitDir::Row => SavedSplitDir::Row,
            crate::pane::SplitDir::Column => SavedSplitDir::Column,
        }
    }
}

impl From<SavedSplitDir> for crate::pane::SplitDir {
    fn from(d: SavedSplitDir) -> Self {
        match d {
            SavedSplitDir::Row => crate::pane::SplitDir::Row,
            SavedSplitDir::Column => crate::pane::SplitDir::Column,
        }
    }
}

impl SavedPane {
    /// Build a saved tree from a live pane tree, extracting each leaf's persisted fields via `leaf`
    /// (which returns a `SavedPane::Leaf`). Pure + generic so it round-trips in tests with `T`=cwd.
    pub(crate) fn from_tree<T>(
        tree: &crate::pane::Pane<T>,
        leaf: &impl Fn(&T) -> SavedPane,
    ) -> SavedPane {
        match tree {
            crate::pane::Pane::Leaf(t) => leaf(t),
            crate::pane::Pane::Split { dir, ratio, a, b } => SavedPane::Split {
                dir: (*dir).into(),
                ratio: *ratio,
                a: Box::new(Self::from_tree(a, leaf)),
                b: Box::new(Self::from_tree(b, leaf)),
            },
        }
    }

    /// Rebuild a live pane tree, constructing each leaf's payload from its saved `Leaf` node via
    /// `make`. The recursion order (A then B) matches `Pane::leaf_paths`, so a rebuilt tree's
    /// `leaf_paths()` lines up 1:1 with the saved tree's leaves left-to-right.
    pub(crate) fn rebuild<T>(&self, make: &impl Fn(&SavedPane) -> T) -> crate::pane::Pane<T> {
        match self {
            SavedPane::Leaf { .. } => crate::pane::Pane::Leaf(make(self)),
            SavedPane::Split { dir, ratio, a, b } => crate::pane::Pane::Split {
                dir: (*dir).into(),
                ratio: *ratio,
                a: Box::new(a.rebuild(make)),
                b: Box::new(b.rebuild(make)),
            },
        }
    }
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
/// Atomic: write a temp file then rename, so a crash mid-write can't truncate the session.
pub(crate) fn save(s: &SavedSession) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = toml::to_string(s) {
        let tmp = p.with_extension("toml.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
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
                    pinned: true,
                    pane: Some(SavedPane::Leaf { cwd: Some("/tmp".into()) }),
                },
                SavedTab {
                    title: None,
                    color: None,
                    cwd: Some("/home/x".into()),
                    pinned: false,
                    pane: None,
                },
            ],
            active: 1,
            window: None,
        };
        let body = toml::to_string(&s).unwrap();
        let back: SavedSession = toml::from_str(&body).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn window_geometry_round_trips_and_is_absent_by_default() {
        // Dropdown sessions never write geometry; window mode's rect survives the round-trip.
        let plain = SavedSession::default();
        assert!(plain.window.is_none());
        assert!(!toml::to_string(&plain).unwrap().contains("window"));
        let s = SavedSession {
            window: Some(WindowGeom { x: 120.0, y: 64.0, w: 1024.0, h: 640.0 }),
            ..Default::default()
        };
        let back: SavedSession = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.window, Some(WindowGeom { x: 120.0, y: 64.0, w: 1024.0, h: 640.0 }));
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

    // --- Split-layout (pane tree) persistence -------------------------------------------------

    /// A saved leaf carrying just a cwd, for the round-trip shape tests.
    fn leaf(cwd: &str) -> SavedPane {
        SavedPane::Leaf { cwd: Some(cwd.into()) }
    }

    #[test]
    fn pane_tree_round_trips_through_toml() {
        use crate::pane::{Pane, SplitDir};
        // A horizontal (Row) split of two cwds, like a user's left/right terminal panes.
        let tree = Pane::Split {
            dir: SplitDir::Row,
            ratio: 0.5,
            a: Box::new(Pane::leaf("/proj/left".to_owned())),
            b: Box::new(Pane::leaf("/proj/right".to_owned())),
        };
        let saved =
            SavedPane::from_tree(&tree, &|cwd: &String| SavedPane::Leaf { cwd: Some(cwd.clone()) });
        // Serializes inside a SavedTab (the real embedding) and comes back identical.
        let tab = SavedTab { pane: Some(saved.clone()), ..Default::default() };
        let back: SavedTab = toml::from_str(&toml::to_string(&tab).unwrap()).unwrap();
        assert_eq!(back.pane, Some(saved.clone()));
        // ...and rebuilds to the same shape: two leaves in order, Row split, ratio preserved.
        let rebuilt = back.pane.unwrap().rebuild(&|sp| match sp {
            SavedPane::Leaf { cwd } => cwd.clone().unwrap_or_default(),
            SavedPane::Split { .. } => unreachable!(),
        });
        assert_eq!(rebuilt.leaf_count(), 2);
        assert_eq!(rebuilt.leaf_at(&[crate::pane::Side::A]), Some(&"/proj/left".to_owned()));
        assert_eq!(rebuilt.leaf_at(&[crate::pane::Side::B]), Some(&"/proj/right".to_owned()));
    }

    #[test]
    fn nested_pane_tree_rebuilds_leaves_in_order() {
        use crate::pane::Pane;
        // Row split, then split B into a column: three leaves left-to-right (a, b, c).
        let (tree, _) = Pane::leaf("a".to_owned()).split(
            &[],
            crate::pane::SplitDir::Row,
            "b".to_owned(),
            false,
        );
        let (tree, _) = tree.split(
            &[crate::pane::Side::B],
            crate::pane::SplitDir::Column,
            "c".to_owned(),
            false,
        );
        let saved = SavedPane::from_tree(&tree, &|cwd: &String| leaf(cwd));
        // Round-trips through TOML (nested externally-tagged enum) and rebuilds A-before-B order.
        let back: SavedPane = toml::from_str(&toml::to_string(&saved).unwrap()).unwrap();
        assert_eq!(back, saved);
        let rebuilt = back.rebuild(&|sp| match sp {
            SavedPane::Leaf { cwd } => cwd.clone().unwrap_or_default(),
            SavedPane::Split { .. } => unreachable!(),
        });
        let by_path: Vec<String> =
            rebuilt.leaf_paths().iter().map(|p| rebuilt.leaf_at(p).unwrap().clone()).collect();
        assert_eq!(by_path, vec!["a", "b", "c"]);
    }

    #[test]
    fn old_session_without_pane_tree_still_loads() {
        // Backward-compat: a session file written before split-restore (no `pane` key) decodes,
        // leaving `pane` None so the tab restores as a single pane.
        let body = "active = 0\n\n[[tabs]]\ncwd = \"/tmp\"\n";
        let back: SavedSession = toml::from_str(body).unwrap();
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.tabs[0].cwd.as_deref(), Some("/tmp"));
        assert!(back.tabs[0].pane.is_none());
    }
}
