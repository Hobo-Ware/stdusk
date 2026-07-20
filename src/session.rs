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
    /// Present when this tab was running Claude Code at save time (from CLI detection). Drives
    /// auto-resume on next launch. The inner `name` is Claude's session name (its OSC title).
    /// Legacy/single-pane path: for split tabs the per-leaf claude state lives in `pane` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claude: Option<ClaudeState>,
    /// The tab's split layout at save time (Tabby-style pane tree). Absent -> a single pane (old
    /// sessions predating split-restore, decoded via the flat `cwd`). serde-default keeps old
    /// session files loading unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane: Option<SavedPane>,
}

/// A tab's split layout, persisted so re-open restores every pane (not just the first). Mirrors
/// `pane::Pane`: a `Leaf` (one terminal's cwd + optional claude state) or a `Split` of two
/// children. Backward-compatible via `SavedTab.pane: Option<_>` (absent -> single pane).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum SavedPane {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Per-leaf claude state (presence marks a claude pane); drives per-pane auto-resume.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude: Option<ClaudeState>,
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
    /// `leaf_paths()` lines up 1:1 with `flat_leaves()`.
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

    /// Every leaf's `(cwd, claude)` left-to-right (A before B), parallel to a rebuilt tree's
    /// `leaf_paths()` - used to align each restored pane with its saved claude state.
    pub(crate) fn flat_leaves(&self) -> Vec<(&Option<String>, &Option<ClaudeState>)> {
        let mut out = Vec::new();
        self.flat_into(&mut out);
        out
    }

    fn flat_into<'a>(&'a self, out: &mut Vec<(&'a Option<String>, &'a Option<ClaudeState>)>) {
        match self {
            SavedPane::Leaf { cwd, claude } => out.push((cwd, claude)),
            SavedPane::Split { a, b, .. } => {
                a.flat_into(out);
                b.flat_into(out);
            }
        }
    }
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

/// Where a Claude session id's transcript lives, which decides HOW (or whether) to resume it.
/// `claude --resume <id>` resolves the transcript ONLY from the current cwd's project dir - verified
/// on macOS: resuming the same id from a different cwd errors "No conversation found". So it is NOT
/// a global by-id lookup, and a moved repo must `cd` back to the original cwd before resuming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resume {
    /// `<id>.jsonl` is in the tab's own cwd project dir -> `claude --resume <id>` works as-is.
    InCwd,
    /// Found only under a DIFFERENT project dir (repo moved). Carries that dir so the caller can
    /// read the original cwd back out of the transcript and `cd` there first.
    Moved(std::path::PathBuf),
    /// No `<id>.jsonl` under any project dir -> resume would error; degrade to a notice + bare claude.
    Missing,
}

/// Locate the transcript for `id`: first in `cwd`'s project dir, else a smart-scan of every
/// `projects_dir/*/` (the moved-repo case). Pure over the filesystem (a synthetic dir in tests).
pub(crate) fn locate_session(id: &str, cwd: &str, projects_dir: &std::path::Path) -> Resume {
    let file = format!("{id}.jsonl");
    if projects_dir.join(encode_cwd(cwd)).join(&file).is_file() {
        return Resume::InCwd;
    }
    if let Ok(rd) = std::fs::read_dir(projects_dir) {
        for entry in rd.flatten() {
            let dir = entry.path();
            if dir.is_dir() && dir.join(&file).is_file() {
                return Resume::Moved(dir);
            }
        }
    }
    Resume::Missing
}

/// The original working directory a transcript was recorded in, read from the first `"cwd":"..."`
/// on any line of `<id>.jsonl` under `project_dir`. Claude stamps the absolute cwd on every message
/// line; we need it to `cd` back before resuming a moved repo's session.
pub(crate) fn session_cwd(project_dir: &std::path::Path, id: &str) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(project_dir.join(format!("{id}.jsonl"))).ok()?;
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        if let Some(cwd) = extract_json_string(&line, "cwd") {
            return Some(cwd);
        }
    }
    None
}

/// Pull the value of a `"key":"value"` string field out of one JSON line without a JSON parser
/// (filesystem paths don't carry escaped quotes). `None` if the field is absent.
fn extract_json_string(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Single-quote a path for safe injection into the shell command line.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A friendly no-op comment line (a shell comment, so it just surfaces the reason) followed by a
/// bare `claude`. The embedded `\r` runs the comment before `claude`; the caller adds the final CR.
fn notice_bare(id: &str) -> String {
    format!("# stdusk: could not resume Claude session {id} (not found) - starting fresh\rclaude")
}

/// Turn a chosen session `id` into the exact command to inject, VERIFYING the transcript exists
/// first so a stale id degrades to a notice + fresh claude instead of Claude's ugly error.
fn resume_cmd_for(id: &str, cwd: &str, projects_dir: &std::path::Path) -> String {
    match locate_session(id, cwd, projects_dir) {
        Resume::InCwd => format!("claude --resume {id}"),
        // Repo moved: `cd` back to the transcript's original cwd so `--resume` resolves it there.
        Resume::Moved(dir) => match session_cwd(&dir, id) {
            Some(orig) if std::path::Path::new(&orig).is_dir() => {
                format!("cd {} && claude --resume {id}", sh_quote(&orig))
            }
            _ => notice_bare(id),
        },
        Resume::Missing => notice_bare(id),
    }
}

/// Decide the exact command to inject into each restored claude pane (in input order). Prefers each
/// pane's own captured session id; falls back to the cwd's most-recent transcript only for a lone
/// claude pane in that cwd; else bare `claude`. Never `--continue`, never resumes an id twice, and
/// verifies the transcript exists (smart-scanning for moved repos) before emitting a resume.
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
                    let cmd = resume_cmd_for(&id, &t.cwd, projects_dir);
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
                    pane: Some(SavedPane::Leaf {
                        cwd: Some("/tmp".into()),
                        claude: Some(ClaudeState {
                            resume_id: Some("3e58d6a4-cbcb-43e4-ae76-c56a48d0ffec".into()),
                        }),
                    }),
                },
                SavedTab {
                    title: None,
                    color: None,
                    cwd: Some("/home/x".into()),
                    pinned: false,
                    claude: None,
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
        // Primary path: the id captured from the tab's own claude argv wins - its transcript is in
        // the tab's own cwd project dir, so `--resume` resolves it there.
        let root = temp_projects();
        write_session(&root, "/proj/a", "uuid-1");
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
        write_session(&root, cwd, "uuid-a");
        write_session(&root, cwd, "uuid-b");
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
        write_session(&root, cwd, "uuid-only");
        let tabs = [
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-only".into()) },
            ResumeTab { cwd: cwd.into(), resume_id: Some("uuid-only".into()) },
        ];
        assert_eq!(resume_commands(&tabs, &root), vec!["claude --resume uuid-only", "claude"]);
        std::fs::remove_dir_all(&root).ok();
    }

    // --- Issue A: existence check + smart scan + graceful degradation -------------------------

    #[test]
    fn locate_session_finds_transcript_in_cwd_dir() {
        let root = temp_projects();
        write_session(&root, "/proj/here", "uuid-x");
        assert_eq!(locate_session("uuid-x", "/proj/here", &root), Resume::InCwd);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_session_smart_scans_other_dirs_for_moved_repo() {
        // Transcript lives under the OLD cwd's project dir; the tab now reports the NEW cwd.
        let root = temp_projects();
        write_session(&root, "/old/path", "uuid-moved");
        let found = locate_session("uuid-moved", "/new/path", &root);
        assert_eq!(found, Resume::Moved(root.join(encode_cwd("/old/path"))));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_session_missing_everywhere() {
        let root = temp_projects();
        write_session(&root, "/proj/other", "some-other-uuid");
        assert_eq!(locate_session("ghost", "/proj/here", &root), Resume::Missing);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_missing_transcript_degrades_to_notice_and_bare_claude() {
        // The bug being fixed: a captured id whose transcript is gone must NOT blindly run
        // `claude --resume` (ugly error) - it emits a comment notice then a fresh bare claude.
        let root = temp_projects();
        let tabs = [ResumeTab { cwd: "/proj/a".into(), resume_id: Some("dead-uuid".into()) }];
        let cmds = resume_commands(&tabs, &root);
        assert_eq!(
            cmds,
            vec![
                "# stdusk: could not resume Claude session dead-uuid (not found) - starting fresh\rclaude"
            ]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_commands_moved_repo_cds_back_then_resumes() {
        // Transcript found under a DIFFERENT project dir (repo moved). Since `--resume` is cwd-
        // scoped, the command `cd`s back to the transcript's original cwd (read from the jsonl)
        // before resuming. The original cwd must still exist on disk.
        let root = temp_projects();
        let orig = temp_projects(); // a real, existing dir standing in for the original cwd
        let dir = root.join(encode_cwd(orig.to_str().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("uuid-moved.jsonl"),
            format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", orig.to_str().unwrap()),
        )
        .unwrap();
        let tabs =
            [ResumeTab { cwd: "/somewhere/new".into(), resume_id: Some("uuid-moved".into()) }];
        assert_eq!(
            resume_commands(&tabs, &root),
            vec![format!("cd '{}' && claude --resume uuid-moved", orig.to_str().unwrap())]
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&orig).ok();
    }

    // --- Issue B: pane-tree persistence -------------------------------------------------------

    /// A saved leaf carrying just a cwd (no claude), for the round-trip shape tests.
    fn leaf(cwd: &str) -> SavedPane {
        SavedPane::Leaf { cwd: Some(cwd.into()), claude: None }
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
        let saved = SavedPane::from_tree(&tree, &|cwd: &String| SavedPane::Leaf {
            cwd: Some(cwd.clone()),
            claude: None,
        });
        // Serializes inside a SavedTab (the real embedding) and comes back identical.
        let tab = SavedTab { pane: Some(saved.clone()), ..Default::default() };
        let back: SavedTab = toml::from_str(&toml::to_string(&tab).unwrap()).unwrap();
        assert_eq!(back.pane, Some(saved.clone()));
        // ...and rebuilds to the same shape: two leaves in order, Row split, ratio preserved.
        let rebuilt = back.pane.unwrap().rebuild(&|sp| match sp {
            SavedPane::Leaf { cwd, .. } => cwd.clone().unwrap_or_default(),
            SavedPane::Split { .. } => unreachable!(),
        });
        assert_eq!(rebuilt.leaf_count(), 2);
        assert_eq!(rebuilt.leaf_at(&[crate::pane::Side::A]), Some(&"/proj/left".to_owned()));
        assert_eq!(rebuilt.leaf_at(&[crate::pane::Side::B]), Some(&"/proj/right".to_owned()));
    }

    #[test]
    fn nested_pane_tree_flat_leaves_align_with_leaf_paths() {
        use crate::pane::Pane;
        // Row split, then split B into a column: three leaves left-to-right.
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
        // flat_leaves order matches the rebuilt tree's leaf_paths order (A before B, recursively).
        let cwds: Vec<String> =
            saved.flat_leaves().into_iter().map(|(c, _)| c.clone().unwrap_or_default()).collect();
        assert_eq!(cwds, vec!["a", "b", "c"]);
        let rebuilt = saved.rebuild(&|sp| match sp {
            SavedPane::Leaf { cwd, .. } => cwd.clone().unwrap_or_default(),
            SavedPane::Split { .. } => unreachable!(),
        });
        let by_path: Vec<String> =
            rebuilt.leaf_paths().iter().map(|p| rebuilt.leaf_at(p).unwrap().clone()).collect();
        assert_eq!(cwds, by_path);
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
