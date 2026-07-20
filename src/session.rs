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
    /// Pinned flag, only written when set (pinned tabs sort first and guard close).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) pinned: bool,
    /// Present when this tab was running Claude Code at save time (from CLI detection). Drives
    /// auto-resume on next launch. The inner `name` is Claude's session name (its OSC title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claude: Option<ClaudeState>,
}

/// Persisted Claude Code state for a tab. Its mere presence marks the tab as a claude tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct ClaudeState {
    /// The `--resume <uuid>` id parsed from the running claude process's argv at save time. It is
    /// per-process (parallel same-cwd tabs each carry their own), so it resumes the exact session.
    /// `None` for a bare/fresh `claude` whose id isn't in argv yet - restore then falls back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resume_id: Option<String>,
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

// --- Claude Code auto-resume ------------------------------------------------------------------
// On restore, each claude tab reattaches to its prior conversation. The session id is captured
// title-independently from the running claude process's argv (`--resume <uuid>`, parsed in
// `procwatch`) at save time, persisted per tab, and replayed as `claude --resume <uuid>`. Because
// each claude process carries its own id, parallel same-cwd tabs each resume their OWN session.
// Fallback (a fresh bare `claude` whose id isn't in argv yet): the most-recent transcript in the
// cwd's project dir, but ONLY when that cwd holds exactly one claude tab (else ambiguous - don't
// guess). Otherwise relaunch bare `claude`. Never `--continue` (it'd collide across same-cwd tabs).

/// A claude tab to resume, in the input's tab order.
pub(crate) struct ResumeTab {
    pub(crate) cwd: String,
    /// The `--resume <uuid>` id captured from the claude process's argv at save time, if any.
    pub(crate) resume_id: Option<String>,
}

/// `~/.claude/projects`, where Claude Code stores per-project session transcripts.
pub(crate) fn claude_projects_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    home.join(".claude/projects")
}

/// Encode a cwd the way Claude names its per-project session folder: every non-alphanumeric
/// character becomes `-` (so `/Users/x/y.z` -> `-Users-x-y-z`). Verified against the real
/// `~/.claude/projects` layout on disk (dots and underscores collapse to dashes too).
pub(crate) fn encode_cwd(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Decide the exact command to inject into each restored claude tab (in input order). Prefers each
/// tab's own captured session id; falls back to the cwd's most-recent transcript only for a lone
/// claude tab in that cwd; else bare `claude`. Never `--continue`, never resumes an id twice.
pub(crate) fn resume_commands(tabs: &[ResumeTab], projects_dir: &std::path::Path) -> Vec<String> {
    use std::collections::{HashMap, HashSet};
    let mut per_cwd: HashMap<&str, usize> = HashMap::new();
    for t in tabs {
        *per_cwd.entry(t.cwd.as_str()).or_default() += 1;
    }
    let mut used: HashSet<String> = HashSet::new();
    tabs.iter()
        .map(|t| {
            // Primary: the id parsed from this tab's own claude argv (per-process, collision-free).
            let id = t.resume_id.clone().filter(|id| !used.contains(id)).or_else(|| {
                // Fallback: guess the cwd's latest session, but only when this tab is the sole
                // claude tab there (otherwise we can't tell which tab owns which session).
                (per_cwd.get(t.cwd.as_str()).copied().unwrap_or(0) == 1)
                    .then(|| most_recent_session(&t.cwd, projects_dir))
                    .flatten()
                    .filter(|id| !used.contains(id))
            });
            match id {
                Some(id) => {
                    let cmd = format!("claude --resume {id}");
                    used.insert(id);
                    cmd
                }
                None => "claude".to_owned(),
            }
        })
        .collect()
}

/// The session id (jsonl file stem) of the most-recently-modified transcript in a cwd's project
/// folder, or `None` if the folder is absent/empty. Used only as a lone-tab fallback.
pub(crate) fn most_recent_session(cwd: &str, projects_dir: &std::path::Path) -> Option<String> {
    let dir = projects_dir.join(encode_cwd(cwd));
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue; // skip the sibling `<uuid>/` state dirs
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(bm, _)| mtime > *bm) {
            best = Some((mtime, stem.to_owned()));
        }
    }
    best.map(|(_, id)| id)
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
                    claude: Some(ClaudeState {
                        resume_id: Some("3e58d6a4-cbcb-43e4-ae76-c56a48d0ffec".into()),
                    }),
                },
                SavedTab {
                    title: None,
                    color: None,
                    cwd: Some("/home/x".into()),
                    pinned: false,
                    claude: None,
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

    // --- Claude auto-resume resolver ----------------------------------------------------------

    #[test]
    fn encode_cwd_matches_claude_project_folder_naming() {
        // Every non-alphanumeric char (slash, dot, underscore) collapses to a dash.
        assert_eq!(encode_cwd("/Users/x/Git/trakt-web"), "-Users-x-Git-trakt-web");
        assert_eq!(encode_cwd("/Users/x/feat-auth_locale"), "-Users-x-feat-auth-locale");
        assert_eq!(encode_cwd("/Users/x/trakt.tv"), "-Users-x-trakt-tv");
    }

    /// Drop a transcript file for `uuid` under a temp projects dir so the fallback resolver has
    /// something to find, without touching the real `~/.claude`.
    fn write_session(root: &std::path::Path, cwd: &str, uuid: &str) {
        let dir = root.join(encode_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{uuid}.jsonl")), "{}\n").unwrap();
    }

    fn temp_projects() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "stdusk-resume-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resume_commands_captured_id_resumes_that_uuid() {
        // Primary path: the id captured from the tab's own claude argv wins with no disk lookup.
        let root = temp_projects();
        let tabs = [ResumeTab { cwd: "/proj/a".into(), resume_id: Some("uuid-1".into()) }];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude --resume uuid-1"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_parallel_same_cwd_tabs_resume_their_own_distinct_sessions() {
        // Two claude tabs in ONE cwd, each with its own captured id -> two distinct resumes,
        // never a shared `--continue`. This is the correctness guarantee the argv approach buys.
        let root = temp_projects();
        let cwd = "/proj/shared";
        let tabs = [
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-a".into()) },
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-b".into()) },
        ];
        assert_eq!(
            resume_commands(&tabs, &root),
            vec!["claude --resume uuid-a", "claude --resume uuid-b"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_lone_tab_without_id_falls_back_to_most_recent() {
        // No captured id (fresh bare claude), sole claude tab in the cwd -> resume the cwd's
        // most-recently-modified transcript.
        let root = temp_projects();
        let cwd = "/proj/lonely";
        write_session(&root, cwd, "old-uuid");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_session(&root, cwd, "new-uuid"); // newer mtime wins
        let tabs = [ResumeTab { cwd: cwd.into(), resume_id: None }];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude --resume new-uuid"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_lone_tab_without_id_and_no_transcript_is_bare_claude() {
        // No id and nothing on disk -> a fresh bare claude, never a guess.
        let root = temp_projects();
        let tabs = [ResumeTab { cwd: "/proj/empty".into(), resume_id: None }];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_unresolved_among_shared_cwd_gets_bare_claude_never_guesses() {
        // The one with a captured id resumes it; its id-less sibling in the SAME cwd must NOT
        // fall back to the most-recent (ambiguous - could be the other tab's session): bare claude.
        let root = temp_projects();
        let cwd = "/proj/shared2";
        write_session(&root, cwd, "uuid-a");
        let tabs = [
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-a".into()) },
            ResumeTab { cwd: cwd.into(), resume_id: None },
        ];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude --resume uuid-a", "claude"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_never_resume_the_same_uuid_twice() {
        // Guard: if two tabs somehow carry the same id, only one resumes it; the other goes bare.
        let root = temp_projects();
        let cwd = "/proj/dup";
        let tabs = [
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-only".into()) },
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-only".into()) },
        ];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude --resume uuid-only", "claude"]);
        std::fs::remove_dir_all(&root).ok();
    }
}
