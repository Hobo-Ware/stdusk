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
    /// The repo root the tab is grouped under; absent for the Other group and older sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repo: Option<String>,
}

/// A tab's split layout, persisted so re-open restores every pane (not just the first). Mirrors
/// `pane::Pane`: a `Leaf` (one terminal's cwd) or a `Split` of two children. Backward-compatible
/// via `SavedTab.pane: Option<_>` (absent -> single pane).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum SavedPane {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// The agent session this pane ran at save time. Unknown content reads as `None`. The
        /// alias only lets a 1.4.0 file load: its `claude` table has another shape, so the lenient
        /// reader turns it into `None`. (The toml crate rejects unknown keys inside an enum
        /// variant, so without the alias such a file would lose the whole session.) It is never
        /// written back under that name.
        #[serde(
            default,
            alias = "claude",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::config::lenient"
        )]
        agent: Option<SavedAgent>,
    },
    Split {
        dir: SavedSplitDir,
        ratio: f32,
        a: Box<SavedPane>,
        b: Box<SavedPane>,
    },
}

/// The Claude Code or Codex session a pane ran. Holds a plain string id: it is validated when the
/// record is used (`agents::SessionId::parse`), never trusted from the file.
///
/// Downgrade note: a session file with an `agent` key fails to load in stdusk 1.8.0 and older
/// (toml rejects unknown keys inside an enum variant), and a live update handoff to such a build
/// aborts. Newer builds load older files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedAgent {
    pub(crate) kind: crate::agents::AgentKind,
    pub(crate) session: String,
    pub(crate) cwd: String,
    /// The transcript file (a Codex rollout). Absent for a Claude registry record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transcript: Option<String>,
    /// The record was kept only because the agent died of a crash signal (which statuses count is
    /// `agent_track::CRASH_SIGNALS`). This is a crash hint: restore types its resume command
    /// without Enter. Absent means a normal record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) crashed: bool,
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

    /// Leaves in this saved tree - how many terminals re-opening it needs. The handoff pairs one
    /// passed fd per leaf, so this is what its count check compares against.
    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            SavedPane::Leaf { .. } => 1,
            SavedPane::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
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

/// Write the session file so a power loss cannot leave it torn or lost: write a temp file, flush
/// it to the disk (`sync_all` is `F_FULLFSYNC` on macOS), rename it over the old file, then flush
/// the directory so the rename itself survives. The directory flush is best effort, because some
/// platforms refuse to open a directory for it. A failed write is not worth interrupting the user.
fn write_durably(path: &std::path::Path, body: &str) {
    use std::io::Write as _;
    let tmp = temp_path(path);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(body.as_bytes())?;
        f.sync_all()
    });
    if written.is_ok() && std::fs::rename(&tmp, path).is_ok() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
        }
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The temp file for `path`. The process id is in the name, so two stdusk processes that save at
/// once (a handoff successor and its predecessor) never write into one file.
fn temp_path(path: &std::path::Path) -> std::path::PathBuf {
    path.with_extension(format!("toml.{}.tmp", std::process::id()))
}

/// The pid in the name of a temp file (`session.toml.<pid>.tmp`), or `None` for any other name.
fn temp_file_pid(name: &str) -> Option<u32> {
    name.strip_prefix("session.toml.")?.strip_suffix(".tmp")?.parse().ok()
}

/// Delete the temp files in `dir` whose writer is not `alive`. A writer that quit mid-write (the
/// final save gives up after a wait) leaves its file behind. Best effort.
fn remove_dead_temp_files(dir: &std::path::Path, alive: impl Fn(u32) -> bool) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if name.to_str().and_then(temp_file_pid).is_some_and(|pid| !alive(pid)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Clean up at startup after earlier runs. A process that is still alive keeps its file: it may be
/// a handoff predecessor that still writes.
pub(crate) fn remove_stale_temp_files() {
    let Some(dir) = path().and_then(|p| p.parent().map(std::path::Path::to_path_buf)) else {
        return;
    };
    remove_dead_temp_files(&dir, crate::procwatch::process_alive);
}

fn write_session_file(s: &SavedSession) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = toml::to_string(s) {
        write_durably(&p, &body);
    }
}

/// A snapshot for the writer thread, and who to tell once it is on the disk.
struct Job {
    snapshot: SavedSession,
    done: Option<std::sync::mpsc::Sender<()>>,
}

/// The one thread that writes the session file. A full sync can take tens of milliseconds, so the
/// UI thread only hands snapshots over. Because one thread does all the writing, two saves never
/// interleave, and a queue that built up while a sync ran collapses to its newest snapshot.
struct SaveWriter {
    jobs: std::sync::mpsc::Sender<Job>,
}

impl SaveWriter {
    fn spawn(write: impl Fn(&SavedSession) + Send + 'static) -> Self {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
        std::thread::spawn(move || {
            while let Ok(mut latest) = queue.recv() {
                let mut waiting: Vec<_> = latest.done.take().into_iter().collect();
                while let Ok(mut newer) = queue.try_recv() {
                    waiting.extend(newer.done.take());
                    latest = newer;
                }
                write(&latest.snapshot);
                for done in waiting {
                    let _ = done.send(());
                }
            }
        });
        Self { jobs }
    }

    /// Queue a snapshot and return at once.
    fn save(&self, snapshot: SavedSession) {
        let _ = self.jobs.send(Job { snapshot, done: None });
    }

    /// Queue a snapshot and wait up to `limit` until it, or a newer one, is on the disk. Anything
    /// queued before it is written first, so this snapshot is the last word. Returns whether it
    /// got there in time.
    fn save_and_wait(&self, snapshot: SavedSession, limit: std::time::Duration) -> bool {
        let (done, written) = std::sync::mpsc::channel();
        self.jobs.send(Job { snapshot, done: Some(done) }).is_ok()
            && written.recv_timeout(limit).is_ok()
    }
}

fn writer() -> &'static SaveWriter {
    static WRITER: std::sync::OnceLock<SaveWriter> = std::sync::OnceLock::new();
    WRITER.get_or_init(|| SaveWriter::spawn(write_session_file))
}

/// Persist the session in the background (best-effort). The periodic save uses this, so a slow
/// disk never blocks a frame.
pub(crate) fn save(s: &SavedSession) {
    writer().save(s.clone());
}

/// How long quit waits for the final save. A hung home volume must not freeze Cmd+Q.
const FINAL_SAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Persist the session and return once it is on the disk, or after [`FINAL_SAVE_WAIT`]. The last
/// save before the panes are killed uses this: the kill ends the agents, and this snapshot must
/// be the one that stays.
pub(crate) fn save_and_wait(s: &SavedSession) {
    if !writer().save_and_wait(s.clone(), FINAL_SAVE_WAIT) {
        eprintln!("stdusk: the final session save did not finish in time; quitting anyway");
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
                    pane: Some(SavedPane::Leaf { cwd: Some("/tmp".into()), agent: None }),
                    repo: Some("/Users/x/Git/stdusk".into()),
                },
                SavedTab {
                    title: None,
                    color: None,
                    cwd: Some("/home/x".into()),
                    pinned: false,
                    pane: None,
                    repo: None,
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
        SavedPane::Leaf { cwd: Some(cwd.into()), agent: None }
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
            agent: None,
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
            SavedPane::Leaf { cwd, .. } => cwd.clone().unwrap_or_default(),
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

    // --- Agent session persistence -------------------------------------------------------------

    const SID: &str = "0c2cbc96-1111-4222-8333-444455556666";

    fn agent_leaf(kind: crate::agents::AgentKind, cwd: &str) -> SavedPane {
        SavedPane::Leaf {
            cwd: Some(cwd.into()),
            agent: Some(SavedAgent {
                kind,
                session: SID.into(),
                cwd: cwd.into(),
                transcript: None,
                crashed: false,
            }),
        }
    }

    #[test]
    fn a_leaf_with_an_agent_round_trips_for_both_agents() {
        use crate::agents::AgentKind::{Claude, Codex};
        let pane = SavedPane::Split {
            dir: SavedSplitDir::Row,
            ratio: 0.5,
            a: Box::new(agent_leaf(Claude, "/work/a")),
            b: Box::new(agent_leaf(Codex, "/work/b")),
        };
        let tab = SavedTab { pane: Some(pane.clone()), ..Default::default() };
        let body = toml::to_string(&tab).unwrap();
        assert!(body.contains("kind = \"claude\"") && body.contains("kind = \"codex\""), "{body}");
        let back: SavedTab = toml::from_str(&body).unwrap();
        assert_eq!(back.pane, Some(pane));
    }

    #[test]
    fn a_leaf_without_an_agent_writes_no_agent_key() {
        let body =
            toml::to_string(&SavedPane::Leaf { cwd: Some("/x".into()), agent: None }).unwrap();
        assert!(!body.contains("agent"), "{body}");
    }

    #[test]
    fn agent_records_load_from_the_written_shape() {
        let body = format!(
            "active = 0\n\n[[tabs]]\n[tabs.pane.Leaf]\ncwd = \"/w\"\n[tabs.pane.Leaf.agent]\nkind = \"codex\"\nsession = \"{SID}\"\ncwd = \"/w\"\n"
        );
        let s: SavedSession = toml::from_str(&body).unwrap();
        let Some(SavedPane::Leaf { agent: Some(a), .. }) = &s.tabs[0].pane else {
            panic!("expected a leaf with an agent");
        };
        assert_eq!(
            (a.kind, a.session.as_str(), a.cwd.as_str()),
            (crate::agents::AgentKind::Codex, SID, "/w")
        );
    }

    #[test]
    fn an_old_1_4_0_claude_key_still_loads_and_is_ignored() {
        // 1.4.0 wrote a `claude` key on the leaf. It must not fail the load, and must not resume.
        let body =
            format!("active = 0\n\n[[tabs]]\n[tabs.pane.Leaf]\ncwd = \"/w\"\nclaude = \"{SID}\"\n");
        let s: SavedSession = toml::from_str(&body).unwrap();
        assert_eq!(s.tabs[0].pane, Some(SavedPane::Leaf { cwd: Some("/w".into()), agent: None }));
    }

    #[test]
    fn an_unknown_agent_kind_or_shape_loads_the_leaf_without_an_agent() {
        let bad_agents = [
            "kind = \"gemini\"\nsession = \"x\"\ncwd = \"/w\"",
            "kind = \"claude\"\ncwd = \"/w\"",
            "kind = 3",
        ];
        for agent in bad_agents {
            let body = format!(
                "active = 0\n\n[[tabs]]\ncwd = \"/keep\"\n[tabs.pane.Leaf]\ncwd = \"/w\"\n[tabs.pane.Leaf.agent]\n{agent}\n"
            );
            let s: SavedSession = toml::from_str(&body).unwrap_or_else(|e| panic!("{agent}: {e}"));
            assert_eq!(s.tabs[0].cwd.as_deref(), Some("/keep"), "the rest of the file survives");
            assert_eq!(
                s.tabs[0].pane,
                Some(SavedPane::Leaf { cwd: Some("/w".into()), agent: None })
            );
        }
    }

    // --- Durable, single-writer saves -----------------------------------------------------------

    fn snap(title: &str) -> SavedSession {
        SavedSession {
            tabs: vec![SavedTab { title: Some(title.into()), ..Default::default() }],
            ..Default::default()
        }
    }

    /// Longer than any test write takes.
    const PATIENT: std::time::Duration = std::time::Duration::from_secs(10);

    fn titles(log: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    /// A writer that records what it writes, and holds its first write until `gate` opens.
    fn recording_writer(
        gate: std::sync::mpsc::Receiver<()>,
    ) -> (SaveWriter, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = log.clone();
        let gate = std::sync::Mutex::new(Some(gate));
        let writer = SaveWriter::spawn(move |s| {
            if let Some(g) = gate.lock().unwrap().take() {
                let _ = g.recv();
            }
            seen.lock().unwrap().push(s.tabs[0].title.clone().unwrap());
        });
        (writer, log)
    }

    #[test]
    fn a_save_returns_at_once_even_while_the_disk_is_slow() {
        let (open, gate) = std::sync::mpsc::channel();
        let (writer, log) = recording_writer(gate);
        let started = std::time::Instant::now();
        writer.save(snap("one")); // the writer is now stuck in its first write
        writer.save(snap("two"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "save must not block");
        assert!(titles(&log).is_empty(), "the slow write has not finished");
        open.send(()).unwrap();
        writer.save_and_wait(snap("last"), PATIENT);
        assert_eq!(titles(&log).last().map(String::as_str), Some("last"));
    }

    #[test]
    fn a_backlog_collapses_to_the_newest_snapshot_and_writes_never_overlap() {
        let (open, gate) = std::sync::mpsc::channel();
        let (writer, log) = recording_writer(gate);
        writer.save(snap("stuck"));
        std::thread::sleep(std::time::Duration::from_millis(150)); // the writer is inside its write
        for n in 0..20 {
            writer.save(snap(&format!("s{n}")));
        }
        open.send(()).unwrap();
        writer.save_and_wait(snap("final"), PATIENT);
        // The stuck write finished, the 20 queued ones collapsed, and the final one is last.
        assert_eq!(titles(&log), vec!["stuck".to_owned(), "final".to_owned()]);
    }

    #[test]
    fn the_final_save_is_written_after_everything_queued_before_it() {
        let (open, gate) = std::sync::mpsc::channel();
        let (writer, log) = recording_writer(gate);
        writer.save(snap("periodic"));
        open.send(()).unwrap();
        writer.save_and_wait(snap("final"), PATIENT);
        writer.save_and_wait(snap("final again"), PATIENT);
        let seen = titles(&log);
        assert_eq!(seen.last().map(String::as_str), Some("final again"));
        assert!(seen.iter().position(|t| t == "periodic") < seen.iter().position(|t| t == "final"));
    }

    #[test]
    fn a_save_that_the_disk_never_finishes_cannot_hold_quit_forever() {
        let (open, gate) = std::sync::mpsc::channel();
        let (writer, _log) = recording_writer(gate);
        let started = std::time::Instant::now();
        let written = writer.save_and_wait(snap("stuck"), std::time::Duration::from_millis(200));
        assert!(!written, "the wait must report that the snapshot is not on the disk");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "the wait must end");
        open.send(()).unwrap(); // let the writer thread finish
    }

    #[test]
    fn two_processes_never_share_a_temp_file() {
        let path = std::path::Path::new("/x/session.toml");
        let tmp = temp_path(path);
        assert_eq!(tmp.parent(), path.parent());
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains(&std::process::id().to_string()), "{name}");
        assert_ne!(tmp, path);
    }

    #[test]
    fn startup_removes_only_the_temp_files_of_dead_writers() {
        let dir = std::env::temp_dir().join(format!("stdusk-stale-tmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = [
            "session.toml.111.tmp",  // dead writer: removed
            "session.toml.222.tmp",  // live writer (a handoff predecessor): kept
            "session.toml",          // the session itself: kept
            "session.toml.abc.tmp",  // not a pid: kept
            "session.toml.111.tmp2", // not a temp name: kept
            "other.toml.111.tmp",    // not ours: kept
        ];
        for name in names {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        remove_dead_temp_files(&dir, |pid| pid == 222);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut want: Vec<String> =
            names.iter().filter(|n| **n != "session.toml.111.tmp").map(|n| (*n).into()).collect();
        want.sort();
        assert_eq!(left, want);
        // A directory that does not exist is fine.
        remove_dead_temp_files(&dir.join("gone"), |_| false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_durable_write_replaces_the_file_whole_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("stdusk-durable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("session.toml");
        write_durably(&file, "first = 1\n");
        write_durably(&file, "second = 2\n");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second = 2\n");
        let names: Vec<_> =
            std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // A missing directory: the write fails quietly, never panics.
        write_durably(&dir.join("gone/session.toml"), "x = 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_crashed_mark_is_written_only_when_set_and_an_absent_mark_means_normal() {
        let agent = |crashed| SavedAgent {
            kind: crate::agents::AgentKind::Claude,
            session: SID.into(),
            cwd: "/w".into(),
            transcript: None,
            crashed,
        };
        let leaf = |crashed| SavedPane::Leaf { cwd: None, agent: Some(agent(crashed)) };
        let normal = toml::to_string(&leaf(false)).unwrap();
        assert!(!normal.contains("crashed"), "{normal}");
        let marked = toml::to_string(&leaf(true)).unwrap();
        assert!(marked.contains("crashed = true"), "{marked}");
        for (body, want) in [(normal, false), (marked, true)] {
            let back: SavedPane = toml::from_str(&body).unwrap();
            assert_eq!(back, leaf(want));
        }
    }
}
