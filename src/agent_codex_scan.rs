//! The Codex rollout scan and the memory that goes with it. [`CodexScanner`] reads the rollout files
//! under `$CODEX_HOME/sessions` on the procwatch scan thread, asks [`judge`] who owns each thread,
//! and keeps what it learned: the verdicts, the processes that left, and the files it read. The
//! matching rules and the module overview are in `agent_codex`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::agent_codex::{
    CodexProc, Gone, Thread, Verdict, blind_rival_causes_doubt, judge, rivals_no_live_process,
    same_process,
};
use crate::agents::{self, AgentKind, AgentSession, SessionId};

/// Largest first line of a rollout that is read. Real ones are about 20 KB (they hold the base
/// instructions). A longer line is not a rollout stdusk can use.
const MAX_META_BYTES: u64 = 1 << 20;

/// Reads of a rollout that did not parse before it is given up. A rollout may be seen half written.
const META_ATTEMPTS: u8 = 5;

/// Most rejected rollout paths that are remembered. Past it, a rejected file is read again on each
/// scan, which costs time and never gives a wrong answer.
const REJECTED_CAP: usize = 4096;

/// Most gone processes that are remembered as rivals. Past it, the oldest is forgotten.
const GONE_CAP: usize = 64;

/// What the scanner remembers of a verdict. `Nobody` is never kept: a TUI may still appear. A doubt
/// that a blind rival causes is not kept either (see [`blind_rival_causes_doubt`]). `Closed` is
/// kept, or the loss of the gone rival (see [`GONE_CAP`]) would hand the thread to a live pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settled {
    /// The process with this pid and start time.
    Owner {
        pid: u32,
        start_secs: u64,
    },
    Ambiguous,
    Closed,
}

/// The rollout scan and the memory that goes with it. It lives on the scan thread.
///
/// A verdict is kept from the first scan that gave one. Without that, the answer could change
/// later: when one of two TUIs in a cwd closes, the one that stays would take the `/new` thread of
/// the one that left.
pub(crate) struct CodexScanner {
    home: PathBuf,
    /// Rollouts read, by file path. Only a success is kept.
    threads: HashMap<PathBuf, Arc<Thread>>,
    /// Failed reads by path. At [`META_ATTEMPTS`] the file is ignored.
    failures: HashMap<PathBuf, u8>,
    /// Rollouts that read fine and are not a user thread of the TUI. Each is read once.
    rejected: HashSet<PathBuf>,
    /// The live processes of the last scan, to see which ones left.
    seen: Vec<CodexProc>,
    /// Processes that left, oldest first. Bounded by [`GONE_CAP`], and by [`rivals_no_live_process`].
    gone: Vec<Gone>,
    /// The latest `ended_ms` of a gone process that the cap dropped. A live process could compete
    /// with it, so an unsettled thread made up to this time may be its thread: it is in doubt.
    forgot_until_ms: u64,
    settled: HashMap<SessionId, Settled>,
    /// Rollout paths found by a search of the whole tree, for a TUI that resumes an old thread.
    /// A miss is kept too.
    found: HashMap<SessionId, Option<PathBuf>>,
}

/// What one scan found in the newest day directories.
struct Listing {
    /// The threads that are not older than every live process and that a TUI made for the user.
    threads: Vec<Arc<Thread>>,
    /// Every rollout file name, whatever its content.
    rollouts: HashMap<SessionId, PathBuf>,
}

/// The id in a rollout file name, `rollout-<time>-<id>.jsonl`.
fn rollout_id(name: &str) -> Option<SessionId> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    SessionId::parse(stem.get(stem.len().checked_sub(36)?..)?)
}

/// The children of `dir` named by exactly `digits` ASCII digits, newest (largest) first.
fn numbered_children(dir: &Path, digits: usize) -> Vec<PathBuf> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (name.len() == digits && name.bytes().all(|b| b.is_ascii_digit()))
                .then(|| (name, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// Days before the UTC date of the oldest live start that a day folder name may still lie. Codex
/// names the folders by local date. The local date is at most one day from the UTC date, and the
/// second day is spare.
const NAME_MARGIN_DAYS: i64 = 2;

/// The day number (days since 1970-01-01) of a civil date. Pure arithmetic: no time zone, no
/// calendar library. Days after the 28th may run into the next month, which keeps it an upper bound.
fn day_number(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The number in a folder name that [`numbered_children`] found.
fn folder_number(dir: &Path) -> Option<i64> {
    dir.file_name()?.to_str()?.parse().ok()
}

/// The day directories `sessions/YYYY/MM/DD` that a live process may have written to, newest
/// first. Two tests keep a folder out:
/// - The name. A rollout of a live process is made after the process starts, so its folder is not
///   more than [`NAME_MARGIN_DAYS`] older than the UTC date of `oldest_start`. Names sort as dates,
///   so the first folder that is too old ends the walk, and the older years and months are not
///   listed. This bounds the cost by the age of the oldest live process and not by the age of the
///   tree.
/// - The modification time. Making a file updates the mtime of its directory, so a folder that is
///   older than `oldest_start` is skipped. It is skipped and does not end the walk, because names
///   and mtimes need not agree (a clock reset, a copied tree).
fn day_dirs_since(sessions: &Path, oldest_start: SystemTime) -> Vec<PathBuf> {
    let start_secs =
        oldest_start.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    let floor = i64::try_from(start_secs / 86_400).unwrap_or(i64::MAX) - NAME_MARGIN_DAYS;
    let mut out = Vec::new();
    'walk: for year_dir in numbered_children(sessions, 4) {
        let Some(year) = folder_number(&year_dir) else { continue };
        if day_number(year, 12, 31) < floor {
            break 'walk;
        }
        for month_dir in numbered_children(&year_dir, 2) {
            let Some(month) = folder_number(&month_dir).filter(|m| (1..=12).contains(m)) else {
                continue;
            };
            if day_number(year, month, 31) < floor {
                break 'walk;
            }
            for day_dir in numbered_children(&month_dir, 2) {
                let Some(day) = folder_number(&day_dir).filter(|d| (1..=31).contains(d)) else {
                    continue;
                };
                if day_number(year, month, day) < floor {
                    break 'walk;
                }
                let modified = std::fs::metadata(&day_dir).and_then(|m| m.modified());
                if modified.is_ok_and(|t| t < oldest_start) {
                    continue;
                }
                out.push(day_dir);
            }
        }
    }
    out
}

/// The rollout files in one day directory, with the id from each name.
fn rollouts_in(day: &Path) -> Vec<(SessionId, PathBuf)> {
    std::fs::read_dir(day)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((rollout_id(e.file_name().to_str()?)?, e.path())))
        .collect()
}

/// The cwd of a thread that the Codex TUI made for the user, from the `payload` of a rollout's
/// first line. Any other thread gives `None`, so it can never become a pane's session:
/// - `originator` is `codex-tui` for the TUI. `codex exec` writes `codex_exec`, the IDE extension
///   and the desktop app write their own.
/// - `thread_source` is `user` for a top-level thread. A sub-agent and a `/review` are made by the
///   TUI too, but they write `subagent`.
///
/// A missing or unknown value is refused. A wrong pick is worse than no id, so a Codex update that
/// renames a value costs the capture and never the correctness.
fn tui_user_thread_cwd(payload: &serde_json::Value) -> Option<&str> {
    let text = |key: &str| payload.get(key)?.as_str();
    (text("originator")? == "codex-tui" && text("thread_source")? == "user").then_some(())?;
    text("cwd")
}

/// What the first line of a rollout says.
enum Meta {
    Thread(String),
    /// A complete line that is not a user thread of the TUI, or names another id. It stays so.
    Foreign,
    /// No complete line yet (a half-written file), or the file cannot be read.
    Unreadable,
}

/// The first line of a rollout, as JSON.
fn first_line(path: &Path) -> Option<serde_json::Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    std::io::BufReader::new(file.take(MAX_META_BYTES)).read_until(b'\n', &mut line).ok()?;
    serde_json::from_slice(&line).ok()
}

/// The cwd of the thread `id`, from its `session_meta` line. `None` when the line is not the
/// meta of `id` or is not a thread of the TUI (see [`tui_user_thread_cwd`]).
fn thread_cwd(meta: &serde_json::Value, id: &SessionId) -> Option<String> {
    let payload = meta.get("payload")?;
    let ours = meta.get("type")?.as_str()? == "session_meta"
        && payload.get("id")?.as_str()? == id.as_str();
    let cwd = tui_user_thread_cwd(payload).filter(|_| ours)?;
    // Codex records the path the shell reported. The OS reports the real one for a process.
    let real = std::fs::canonicalize(cwd).ok().and_then(|p| p.into_os_string().into_string().ok());
    agents::plain_abs_path(real.as_deref().unwrap_or(cwd))
}

fn read_meta(path: &Path, id: &SessionId) -> Meta {
    match first_line(path) {
        None => Meta::Unreadable,
        Some(line) => thread_cwd(&line, id).map_or(Meta::Foreign, Meta::Thread),
    }
}

fn session_of(thread: &Thread) -> AgentSession {
    AgentSession {
        kind: AgentKind::Codex,
        id: thread.id.clone(),
        cwd: thread.cwd.clone(),
        transcript: agents::plain_abs_path(&thread.rollout),
    }
}

impl CodexScanner {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self {
            home,
            threads: HashMap::new(),
            failures: HashMap::new(),
            rejected: HashSet::new(),
            seen: Vec::new(),
            gone: Vec::new(),
            forgot_until_ms: 0,
            settled: HashMap::new(),
            found: HashMap::new(),
        }
    }

    /// The Codex session of each live process, by pid. A process gets none when nothing is known
    /// or when the match is in doubt.
    pub(crate) fn sessions(&mut self, procs: &[CodexProc]) -> HashMap<u32, AgentSession> {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        self.sessions_at(procs, u64::try_from(now.as_millis()).unwrap_or(u64::MAX))
    }

    /// [`Self::sessions`] with the clock passed in, so a test controls when a process left.
    fn sessions_at(&mut self, procs: &[CodexProc], now_ms: u64) -> HashMap<u32, AgentSession> {
        self.note_departures(procs, now_ms);
        if procs.is_empty() {
            // Nothing lives, so the reads and verdicts go. What left stays a rival: a TUI may
            // start in the second in which another one closed after a /new.
            *self = Self {
                gone: std::mem::take(&mut self.gone),
                forgot_until_ms: self.forgot_until_ms,
                ..Self::new(std::mem::take(&mut self.home))
            };
            return HashMap::new();
        }
        let listing = self.list(procs);
        let mut newest: HashMap<u32, &Arc<Thread>> = HashMap::new();
        for thread in &listing.threads {
            if let Verdict::Owner(pid) = self.verdict(thread, procs)
                && newest.get(&pid).is_none_or(|old| old.time_ms < thread.time_ms)
            {
                newest.insert(pid, thread);
            }
        }
        let listed: HashSet<&SessionId> = listing.threads.iter().map(|t| &t.id).collect();
        self.settled.retain(|id, _| listed.contains(id));
        let mut out = HashMap::new();
        for proc in procs {
            let session = match (newest.get(&proc.pid), &proc.resumes) {
                (Some(thread), _) => Some(session_of(thread)),
                (None, Some(id)) => self.resumed_session(proc, id, &listing.rollouts),
                (None, None) => None,
            };
            if let Some(session) = session {
                out.insert(proc.pid, session);
            }
        }
        out
    }

    /// Remember the processes of the last scan that are not live now, and forget the ones that are
    /// live again. A gone process that no live process can compete with is forgotten (see
    /// [`rivals_no_live_process`]). Past [`GONE_CAP`] the oldest is dropped, and `forgot_until_ms`
    /// records that.
    fn note_departures(&mut self, procs: &[CodexProc], now_ms: u64) {
        let live = |old: &CodexProc| procs.iter().any(|p| same_process(p, old));
        let before = std::mem::take(&mut self.seen);
        let left = before.iter().filter(|old| !live(old)).cloned();
        self.gone.extend(left.map(|proc| Gone { proc, ended_ms: now_ms }));
        // A cwd that one scan could not read is kept from an earlier scan. A gone process with
        // no cwd would rival the threads of every cwd.
        self.seen = procs
            .iter()
            .map(|p| {
                let known =
                    || before.iter().find(|o| same_process(o, p)).and_then(|o| o.cwd.clone());
                CodexProc { cwd: p.cwd.clone().or_else(known), ..p.clone() }
            })
            .collect();
        // A scan may miss a process once. It is back, so it is no longer a rival of itself.
        self.gone.retain(|g| !live(&g.proc));
        self.gone.retain(|g| !rivals_no_live_process(g, procs, now_ms));
        let excess = self.gone.len().saturating_sub(GONE_CAP);
        for dropped in self.gone.drain(..excess) {
            self.forgot_until_ms = self.forgot_until_ms.max(dropped.ended_ms);
        }
    }

    /// The rollouts of the day directories that changed since the oldest live process started.
    /// Only the threads that are not older than every live process are opened, and each only once.
    /// The cost is linear in the number of rollouts and of day folders that [`day_dirs_since`] keeps.
    fn list(&mut self, procs: &[CodexProc]) -> Listing {
        let oldest_secs = procs.iter().map(|p| p.start_secs).min().unwrap_or(0);
        let oldest_ms = oldest_secs.saturating_mul(1000);
        let since = SystemTime::UNIX_EPOCH + Duration::from_secs(oldest_secs);
        let mut threads = Vec::new();
        let mut rollouts = HashMap::new();
        for day in day_dirs_since(&self.home.join("sessions"), since) {
            for (id, path) in rollouts_in(&day) {
                if let Some(time_ms) = id.v7_time_ms().filter(|t| *t >= oldest_ms)
                    && let Some(thread) = self.thread(&path, &id, time_ms)
                {
                    threads.push(thread);
                }
                rollouts.insert(id, path);
            }
        }
        let present: HashSet<&Path> = rollouts.values().map(PathBuf::as_path).collect();
        self.threads.retain(|path, _| present.contains(path.as_path()));
        self.failures.retain(|path, _| present.contains(path.as_path()));
        self.rejected.retain(|path| present.contains(path.as_path()));
        Listing { threads, rollouts }
    }

    fn thread(&mut self, path: &Path, id: &SessionId, time_ms: u64) -> Option<Arc<Thread>> {
        if let Some(thread) = self.threads.get(path) {
            return Some(Arc::clone(thread));
        }
        if self.rejected.contains(path)
            || self.failures.get(path).is_some_and(|n| *n >= META_ATTEMPTS)
        {
            return None;
        }
        let cwd = match read_meta(path, id) {
            Meta::Thread(cwd) => cwd,
            Meta::Foreign => {
                if self.rejected.len() < REJECTED_CAP {
                    self.rejected.insert(path.to_owned());
                }
                return None;
            }
            Meta::Unreadable => {
                *self.failures.entry(path.to_owned()).or_default() += 1;
                return None;
            }
        };
        let rollout = path.to_string_lossy().into_owned();
        let thread = Arc::new(Thread { id: id.clone(), cwd, time_ms, rollout });
        self.threads.insert(path.to_owned(), Arc::clone(&thread));
        Some(thread)
    }

    /// The verdict of one thread, kept from the first scan that gave one.
    fn verdict(&mut self, thread: &Thread, procs: &[CodexProc]) -> Verdict {
        match self.settled.get(&thread.id) {
            Some(Settled::Ambiguous) => Verdict::Ambiguous,
            Some(Settled::Closed) => Verdict::Closed,
            Some(&Settled::Owner { pid, start_secs }) => {
                let live = procs.iter().any(|p| p.pid == pid && p.start_secs == start_secs);
                if live { Verdict::Owner(pid) } else { Verdict::Nobody }
            }
            None => {
                let judged = judge(thread, procs, &self.gone);
                // A rival that was forgotten in a hurry may own it: nobody gets it.
                let verdict = match judged {
                    Verdict::Owner(_) if thread.time_ms <= self.forgot_until_ms => {
                        Verdict::Ambiguous
                    }
                    other => other,
                };
                let kept = match verdict {
                    Verdict::Owner(pid) => procs
                        .iter()
                        .find(|p| p.pid == pid)
                        .map(|p| Settled::Owner { pid, start_secs: p.start_secs }),
                    Verdict::Ambiguous if !blind_rival_causes_doubt(thread, procs, &self.gone) => {
                        Some(Settled::Ambiguous)
                    }
                    Verdict::Closed => Some(Settled::Closed),
                    Verdict::Ambiguous | Verdict::Nobody => None,
                };
                if let Some(kept) = kept {
                    self.settled.insert(thread.id.clone(), kept);
                }
                verdict
            }
        }
    }

    /// The session of a TUI that runs `codex resume <id>`: exact. It carries the rollout path when
    /// the rollout is in the tree, and restore needs the path.
    fn resumed_session(
        &mut self,
        proc: &CodexProc,
        id: &SessionId,
        rollouts: &HashMap<SessionId, PathBuf>,
    ) -> Option<AgentSession> {
        let rollout = rollouts.get(id).cloned().or_else(|| self.find_rollout(id));
        Some(AgentSession {
            kind: AgentKind::Codex,
            id: id.clone(),
            cwd: proc.cwd.clone()?,
            transcript: rollout.and_then(|p| agents::plain_abs_path(p.to_str()?)),
        })
    }

    /// Search the whole tree for the rollout of an old thread. Once per id, and the answer is kept.
    fn find_rollout(&mut self, id: &SessionId) -> Option<PathBuf> {
        if let Some(known) = self.found.get(id) {
            return known.clone();
        }
        let found = numbered_children(&self.home.join("sessions"), 4)
            .into_iter()
            .flat_map(|y| numbered_children(&y, 2))
            .flat_map(|m| numbered_children(&m, 2))
            .flat_map(|d| rollouts_in(&d))
            .find_map(|(found, path)| (&found == id).then_some(path));
        self.found.insert(id.clone(), found.clone());
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_codex::fixtures::*;

    #[test]
    fn a_rollout_name_holds_its_id_and_nothing_else_counts() {
        let id = id_at(0, 7);
        let name = |n: &str| format!("rollout-2026-09-30T12-46-59-{n}.jsonl");
        assert_eq!(rollout_id(&name(id.as_str())), Some(id.clone()));
        for bad in [
            format!("{}.jsonl", id.as_str()),
            format!("rollout-{}.txt", id.as_str()),
            format!("rollout-x-{}", id.as_str()),
            name("not-an-id"),
            "rollout-.jsonl".to_owned(),
            String::new(),
        ] {
            assert_eq!(rollout_id(&bad), None, "{bad}");
        }
    }

    // The first lines of real rollouts (Codex 0.159.2, live probes). `base_instructions` (about
    // 22 KB), the git block and other fields that the check ignores are cut out. The cwd is renamed.
    const FIRST_TUI_THREAD: &str = r#"{"type":"session_meta","payload":{"id":"01a0f1e4-08db-7b90-874b-f9ed5a620212","cwd":"/work/a","originator":"codex-tui","cli_version":"0.159.2","source":"vscode","thread_source":"user"}}"#;
    const CODEX_EXEC: &str = r#"{"type":"session_meta","payload":{"id":"01a0f1eb-4d99-7fa3-b8ba-cb59312864b3","cwd":"/work/a","originator":"codex_exec","cli_version":"0.159.2","source":"exec","thread_source":"user"}}"#;
    const SUB_AGENT: &str = r#"{"type":"session_meta","payload":{"id":"01a0f1e5-9cc1-7431-8f3d-1cd5d5337b87","parent_thread_id":"01a0f1e5-7998-7472-b4d3-6d09f8f4acb0","cwd":"/work/a","originator":"codex-tui","source":{"subagent":{"thread_spawn":{"parent_thread_id":"01a0f1e5-7998-7472-b4d3-6d09f8f4acb0","depth":1,"agent_path":null,"agent_nickname":"Dewey","agent_role":null}}},"thread_source":"subagent"}}"#;
    const REVIEW: &str = r#"{"type":"session_meta","payload":{"id":"01a0f1fc-264f-7930-8906-fef939e54b61","parent_thread_id":"01a0f1fb-ecad-7c30-835c-7107da442649","cwd":"/work/a","originator":"codex-tui","source":{"subagent":"review"},"thread_source":"subagent"}}"#;

    #[test]
    fn only_a_user_thread_of_the_tui_has_a_cwd() {
        let cwd = |line: &str| {
            let meta: serde_json::Value = serde_json::from_str(line).unwrap();
            tui_user_thread_cwd(&meta["payload"]).map(str::to_owned)
        };
        let mine = Some("/work/a".to_owned());
        let cases = [
            ("first thread of a TUI, and its /new", FIRST_TUI_THREAD, mine.clone()),
            ("codex exec", CODEX_EXEC, None),
            ("sub-agent of the TUI", SUB_AGENT, None),
            ("/review of the TUI", REVIEW, None),
            // A value that a later Codex renames or drops is refused, never guessed.
            ("no originator", r#"{"payload":{"cwd":"/w","thread_source":"user"}}"#, None),
            ("no thread_source", r#"{"payload":{"cwd":"/w","originator":"codex-tui"}}"#, None),
            (
                "another thread_source",
                r#"{"payload":{"cwd":"/w","originator":"codex-tui","thread_source":"unknown"}}"#,
                None,
            ),
            (
                "IDE extension",
                r#"{"payload":{"cwd":"/w","originator":"codex_vscode","thread_source":"user"}}"#,
                None,
            ),
            (
                "not a string",
                r#"{"payload":{"cwd":"/w","originator":["codex-tui"],"thread_source":"user"}}"#,
                None,
            ),
            ("no cwd", r#"{"payload":{"originator":"codex-tui","thread_source":"user"}}"#, None),
        ];
        for (why, line, want) in cases {
            assert_eq!(cwd(line), want, "{why}");
        }
    }

    // --- The scanner, on a temp tree ------------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("stdusk-codex-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        // The OS gives real paths for a process cwd, and the scanner compares against them.
        std::fs::canonicalize(&p).unwrap()
    }

    /// Write a rollout for `id` under `sessions/<day>/`. The first line is the `session_meta` of a
    /// user thread of the TUI, with `extra` merged into its payload.
    fn rollout_with(
        home: &Path,
        day: &str,
        id: &SessionId,
        cwd: &str,
        extra: &serde_json::Value,
    ) -> String {
        let dir = home.join("sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", id.as_str()));
        let mut payload = serde_json::json!({
            "session_id": id.as_str(), "id": id.as_str(), "cwd": cwd,
            "originator": "codex-tui", "source": "vscode", "thread_source": "user",
            "base_instructions": { "text": "x".repeat(20_000) }
        });
        payload.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let line = serde_json::json!({
            "timestamp": "2026-09-30T09:46:59.404Z", "ordinal": 0, "type": "session_meta",
            "payload": payload
        });
        std::fs::write(&path, format!("{line}\n{{\"type\":\"event\"}}\n")).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn rollout(home: &Path, day: &str, id: &SessionId, cwd: &str) -> String {
        rollout_with(home, day, id, cwd, &serde_json::json!({}))
    }

    fn ids_of(got: &HashMap<u32, AgentSession>) -> Vec<(u32, String)> {
        let mut v: Vec<_> = got.iter().map(|(p, s)| (*p, s.id.as_str().to_owned())).collect();
        v.sort();
        v
    }

    #[test]
    fn the_scan_gives_each_tui_the_thread_its_rollout_and_timing_name() {
        let home = scratch("scan");
        let (a, b) = (id_at(5_320, 1), id_at(15_310, 2));
        let path_a = rollout(&home, "2026/09/30", &a, "/work/a");
        rollout(&home, "2026/09/30", &b, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let procs = [tui(10, 5, "/work/a"), tui(11, 15, "/work/a")];
        let got = scanner.sessions(&procs);
        assert_eq!(ids_of(&got), vec![(10, a.as_str().to_owned()), (11, b.as_str().to_owned())]);
        let s = &got[&10];
        assert_eq!((s.kind, s.cwd.as_str()), (AgentKind::Codex, "/work/a"));
        assert_eq!(s.transcript.as_deref(), Some(path_a.as_str()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_rollout_cwd_that_is_a_symlink_matches_the_real_cwd_of_the_process() {
        let home = scratch("symlink");
        let real = home.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = home.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let id = id_at(5_320, 1);
        rollout(&home, "2026/09/30", &id, link.to_str().unwrap());
        let mut scanner = CodexScanner::new(home.clone());
        let got = scanner.sessions(&[tui(10, 5, real.to_str().unwrap())]);
        assert_eq!(ids_of(&got), vec![(10, id.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_scan_ignores_threads_of_no_live_tui_and_unusable_files() {
        let home = scratch("ignore");
        let old = id_at(1_000, 1); // before the TUI
        let mine = id_at(5_320, 2);
        rollout(&home, "2026/09/30", &old, "/work/a");
        rollout(&home, "2026/09/30", &mine, "/work/a");
        // Junk that must not break the scan.
        let dir = home.join("sessions/2026/09/30");
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join("rollout-bad.jsonl"), "x").unwrap();
        let broken = id_at(6_000, 3);
        std::fs::write(
            dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", broken.as_str())),
            "{ nope",
        )
        .unwrap();
        // A rollout whose first line names another id is not this thread.
        let liar = id_at(7_000, 4);
        let other = id_at(7_000, 5);
        let text =
            std::fs::read_to_string(rollout(&home, "2026/09/30", &other, "/work/a")).unwrap();
        std::fs::remove_file(
            dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", other.as_str())),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", liar.as_str())),
            text,
        )
        .unwrap();
        let mut scanner = CodexScanner::new(home.clone());
        let got = scanner.sessions(&[tui(10, 5, "/work/a")]);
        // `mine` is the first thread. `liar` is refused, so it cannot be a later one.
        assert_eq!(ids_of(&got), vec![(10, mine.as_str().to_owned())]);
        // With no TUI at all nothing is assigned.
        assert!(scanner.sessions(&[]).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_newest_thread_of_a_tui_is_its_session() {
        let home = scratch("newest");
        let (first, second) = (id_at(5_320, 1), id_at(600_000, 2));
        rollout(&home, "2026/09/30", &first, "/work/a");
        rollout(&home, "2026/09/30", &second, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let got = scanner.sessions(&[tui(10, 5, "/work/a")]);
        assert_eq!(ids_of(&got), vec![(10, second.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_thread_in_doubt_gives_the_pane_no_session() {
        let home = scratch("doubt");
        rollout(&home, "2026/09/30", &id_at(6_320, 1), "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let got = scanner.sessions(&[tui(10, 5, "/work/a"), tui(11, 6, "/work/a")]);
        assert!(got.is_empty(), "{got:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_verdict_is_kept_so_a_closed_tui_cannot_hand_its_thread_to_the_one_that_stays() {
        let home = scratch("settled");
        let (first_a, first_b, new_a) = (id_at(5_320, 1), id_at(105_320, 2), id_at(300_000, 3));
        rollout(&home, "2026/09/30", &first_a, "/work/a");
        rollout(&home, "2026/09/30", &first_b, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let both = [tui(10, 5, "/work/a"), tui(11, 105, "/work/a")];
        assert_eq!(scanner.sessions(&both).len(), 2);
        // TUI 10 does /new: two TUIs could own it, so it is in doubt.
        rollout(&home, "2026/09/30", &new_a, "/work/a");
        let got = scanner.sessions(&both);
        assert_eq!(ids_of(&got)[0], (10, first_a.as_str().to_owned()), "10 keeps its old thread");
        // TUI 10 closes. TUI 11 is now the only one in the cwd, but the thread stays in doubt.
        let got = scanner.sessions(&both[1..]);
        assert_eq!(ids_of(&got), vec![(11, first_b.as_str().to_owned())]);
        // A fresh scanner has no memory and would have made that mistake. That is why one lives on.
        let got = CodexScanner::new(home.clone()).sessions(&both[1..]);
        assert_eq!(ids_of(&got), vec![(11, new_a.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_thread_that_is_not_a_user_thread_of_the_tui_never_takes_over_the_pane() {
        use serde_json::json;
        let home = scratch("foreign");
        let mine = id_at(5_320, 1);
        rollout(&home, "2026/09/30", &mine, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let procs = [tui(10, 5, "/work/a")];
        assert_eq!(ids_of(&scanner.sessions(&procs)), vec![(10, mine.as_str().to_owned())]);
        // Each of these is newer than the TUI and in its cwd, and no live process explains it.
        let foreign = [
            json!({ "originator": "codex_exec", "source": "exec" }),
            json!({ "source": { "subagent": "review" }, "thread_source": "subagent" }),
            json!({ "source": { "subagent": { "thread_spawn": {} } }, "thread_source": "subagent" }),
        ];
        for (n, extra) in (2..).zip(foreign) {
            rollout_with(&home, "2026/09/30", &id_at(60_000 * n, n), "/work/a", &extra);
            let got = scanner.sessions(&procs);
            assert_eq!(ids_of(&got), vec![(10, mine.as_str().to_owned())], "foreign thread {n}");
        }
        // A user thread of the TUI in the same place is still a /new.
        let new = id_at(600_000, 9);
        rollout(&home, "2026/09/30", &new, "/work/a");
        assert_eq!(ids_of(&scanner.sessions(&procs)), vec![(10, new.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Cost of a steady-state scan with many rollouts and many old day folders, made by the
    /// procwatch scan thread. The scan is linear. The spec (section 4.3) gives the measured cost.
    /// Slow, so it is run by hand:
    /// `cargo +1.98.1 test --release scan_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand in a release build"]
    fn scan_cost_is_linear_in_the_number_of_rollouts() {
        let home = scratch("cost");
        for n in 0..3000 {
            rollout(&home, "2026/09/30", &id_at(1_000 + n, n), "/work/b");
        }
        // Three years of day folders, all older than the name bound of the process below.
        for year in 2021..2024 {
            for month in 1..=12 {
                for day in 1..=28 {
                    std::fs::create_dir_all(
                        home.join(format!("sessions/{year}/{month:02}/{day:02}")),
                    )
                    .unwrap();
                }
            }
        }
        let mut scanner = CodexScanner::new(home.clone());
        let procs = [tui(10, 0, "/work/a")];
        scanner.sessions(&procs); // reads every first line, once
        let started = std::time::Instant::now();
        scanner.sessions(&procs);
        let each = started.elapsed();
        eprintln!("steady-state scan of 3000 rollouts: {each:?}");
        assert!(each < std::time::Duration::from_millis(100), "{each:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_tui_that_resumes_a_thread_gets_it_exactly_with_its_rollout_from_any_day() {
        let home = scratch("resume");
        let old = id_at(0, 50);
        let old_path = rollout(&home, "2026/08/01", &old, "/work/a");
        // Two newer day dirs push the old one out of the scan window.
        rollout(&home, "2026/09/29", &id_at(1, 51), "/work/z");
        rollout(&home, "2026/09/30", &id_at(2, 52), "/work/z");
        let mut scanner = CodexScanner::new(home.clone());
        let mut t = tui(10, 5, "/work/a");
        t.resumes = Some(old.clone());
        let got = scanner.sessions(&[t.clone()]);
        assert_eq!(ids_of(&got), vec![(10, old.as_str().to_owned())]);
        assert_eq!(got[&10].transcript.as_deref(), Some(old_path.as_str()));
        // A resumed thread whose rollout is gone is still named, without a path.
        t.resumes = Some(id_at(0, 60));
        assert_eq!(scanner.sessions(&[t.clone()])[&10].transcript, None);
        // With no cwd the session has no directory to resume in.
        t.cwd = None;
        assert!(scanner.sessions(&[t]).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Noon UTC of a civil date, as a file time. The tests below use fixed dates, not the clock.
    fn noon(year: i64, month: i64, day: i64) -> SystemTime {
        let secs = u64::try_from(day_number(year, month, day) * 86_400 + 43_200).unwrap();
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Make the folder `day` under `sessions` with the given modification time.
    fn day_folder(sessions: &Path, day: &str, mtime: SystemTime) {
        let dir = sessions.join(day);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::open(&dir).unwrap().set_modified(mtime).unwrap();
    }

    fn walk_names(sessions: &Path, start: SystemTime) -> Vec<String> {
        day_dirs_since(sessions, start)
            .iter()
            .map(|p| p.strip_prefix(sessions).unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_civil_date_maps_to_its_day_number() {
        // 1970-01-01 is day 0. The other rows are checked against `date -u -d @<secs>`.
        let cases = [
            ((1970, 1, 1), 0),
            ((1970, 3, 1), 59),
            ((2000, 2, 29), 11_016),
            ((2024, 12, 31), 20_088),
            ((2026, 9, 30), 20_726),
            ((1969, 12, 31), -1),
        ];
        for ((y, m, d), want) in cases {
            assert_eq!(day_number(y, m, d), want, "{y}-{m}-{d}");
        }
    }

    #[test]
    fn day_directories_older_than_the_oldest_start_are_skipped() {
        let home = scratch("days");
        let sessions = home.join("sessions");
        let days = [
            ((2026, 9, 30), "2026/09/30"),
            ((2026, 9, 29), "2026/09/29"),
            ((2026, 9, 28), "2026/09/28"),
            ((2026, 9, 27), "2026/09/27"),
            ((2026, 8, 21), "2026/08/21"),
            ((2025, 12, 3), "2025/12/03"),
        ];
        for ((y, m, d), name) in days {
            day_folder(&sessions, name, noon(y, m, d));
        }
        std::fs::create_dir_all(sessions.join("2026/xx/01")).unwrap(); // not a number
        std::fs::create_dir_all(sessions.join("tmp/09/30")).unwrap();
        let hour = Duration::from_secs(3_600);
        let before = |y, m, d| noon(y, m, d) - hour;
        assert_eq!(walk_names(&sessions, before(2026, 9, 30)), ["2026/09/30"], "started today");
        assert_eq!(
            walk_names(&sessions, before(2026, 9, 29)),
            ["2026/09/30", "2026/09/29"],
            "started yesterday"
        );
        let three = walk_names(&sessions, before(2026, 9, 27));
        assert_eq!((three.len(), three[3].as_str()), (4, "2026/09/27"), "started three days ago");
        assert_eq!(walk_names(&sessions, before(2025, 1, 1)).len(), 6, "an old process reads all");
        assert_eq!(
            day_dirs_since(&home.join("missing"), noon(2026, 9, 30)),
            [] as [std::path::PathBuf; 0]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_walk_ends_at_the_first_folder_name_that_is_too_old_whatever_its_mtime() {
        let home = scratch("name-bound");
        let sessions = home.join("sessions");
        let fresh = SystemTime::now(); // an mtime that the mtime test alone would keep
        let start = noon(2026, 9, 30);
        // The bound is two days before the UTC date of the start: 09/28 stays and 09/27 goes.
        for name in
            ["2026/09/30", "2026/09/28", "2026/09/27", "2026/09/01", "2026/08/31", "2025/12/31"]
        {
            day_folder(&sessions, name, fresh);
        }
        assert_eq!(walk_names(&sessions, start), ["2026/09/30", "2026/09/28"]);
        // A start just after midnight UTC has the same bound, with the margin for a local date.
        let after_midnight = noon(2026, 9, 30) - Duration::from_hours(11);
        assert_eq!(walk_names(&sessions, after_midnight), ["2026/09/30", "2026/09/28"]);
        // Across a month and a year edge.
        let january = noon(2026, 1, 1);
        day_folder(&sessions, "2025/12/30", fresh);
        day_folder(&sessions, "2025/12/29", fresh);
        let names = walk_names(&sessions, january);
        assert!(names.iter().any(|n| n == "2025/12/30"), "{names:?}");
        assert!(names.iter().all(|n| n != "2025/12/29"), "{names:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_day_directory_with_a_reset_mtime_does_not_hide_the_newer_names_after_it() {
        let home = scratch("mtime-reset");
        let sessions = home.join("sessions");
        // The middle folder has an old mtime (a copied tree, a clock reset). The folders on both
        // sides are recent. Names sort 30, 29, 28, and the walk must reach 28.
        day_folder(&sessions, "2026/09/30", noon(2026, 9, 30));
        day_folder(&sessions, "2026/09/29", noon(2025, 11, 1));
        day_folder(&sessions, "2026/09/28", noon(2026, 9, 29));
        let names = walk_names(&sessions, noon(2026, 9, 29) - Duration::from_secs(3_600));
        assert_eq!(names, ["2026/09/30", "2026/09/28"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_rejected_rollout_is_read_once() {
        let home = scratch("rejected");
        let exec = id_at(6_000, 1);
        let exec_path = rollout_with(
            &home,
            "2026/09/30",
            &exec,
            "/work/a",
            &serde_json::json!({ "originator": "codex_exec" }),
        );
        let mut scanner = CodexScanner::new(home.clone());
        let procs = [tui(10, 5, "/work/a")];
        assert!(scanner.sessions(&procs).is_empty());
        assert!(scanner.rejected.contains(Path::new(&exec_path)));
        // Proof of a single read: the file now holds a valid thread, and the scan does not look.
        rollout(&home, "2026/09/30", &exec, "/work/a");
        assert!(scanner.sessions(&procs).is_empty());
        // The memory is bounded by the files present.
        std::fs::remove_file(&exec_path).unwrap();
        scanner.sessions(&procs);
        assert!(scanner.rejected.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_doubt_from_a_blind_rival_is_decided_by_a_later_scan() {
        let home = scratch("blind");
        let id = id_at(5_320, 1);
        rollout(&home, "2026/09/30", &id, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let blind_first = [tui(10, 5, "/work/a"), blind(11, 5)];
        assert!(scanner.sessions(&blind_first).is_empty());
        // The OS now gives the cwd of process 11, and it is elsewhere.
        let sighted = [tui(10, 5, "/work/a"), tui(11, 5, "/work/b")];
        assert_eq!(ids_of(&scanner.sessions(&sighted)), vec![(10, id.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_doubt_between_two_sighted_tuis_still_settles() {
        let home = scratch("settled-doubt");
        let id = id_at(6_320, 1);
        rollout(&home, "2026/09/30", &id, "/work/a");
        let mut scanner = CodexScanner::new(home.clone());
        let both = [tui(10, 5, "/work/a"), tui(11, 6, "/work/a"), blind(12, 6)];
        assert!(scanner.sessions(&both).is_empty());
        assert_eq!(scanner.settled.get(&id), Some(&Settled::Ambiguous));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A TUI that closed, and a thread it made that the daemon writes afterwards.
    #[test]
    fn a_thread_of_a_closed_tui_does_not_go_to_the_live_pane_in_the_same_cwd() {
        let home = scratch("closed");
        let (first_a, first_b) = (id_at(5_320, 1), id_at(105_320, 2));
        rollout(&home, "2026/09/30", &first_a, "/work/a");
        rollout(&home, "2026/09/30", &first_b, "/work/a");
        let (a, b) = (tui(10, 5, "/work/a"), tui(11, 105, "/work/a"));
        let mut scanner = CodexScanner::new(home.clone());
        let ms = |s: u64| T0 * 1000 + s;
        assert_eq!(scanner.sessions_at(&[a.clone(), b.clone()], ms(200_000)).len(), 2);
        // TUI 10 closes at about 250 s. It had made a /new at 240 s, which the daemon writes now.
        scanner.sessions_at(std::slice::from_ref(&b), ms(250_000));
        let late = id_at(240_000, 3);
        rollout(&home, "2026/09/30", &late, "/work/a");
        let got = scanner.sessions_at(std::slice::from_ref(&b), ms(260_000));
        assert_eq!(ids_of(&got), vec![(11, first_b.as_str().to_owned())]);
        // A /new that TUI 11 makes after that is its own.
        let mine = id_at(300_000, 4);
        rollout(&home, "2026/09/30", &mine, "/work/a");
        let got = scanner.sessions_at(&[b], ms(310_000));
        assert_eq!(ids_of(&got), vec![(11, mine.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn gone_processes_are_forgotten_when_they_cannot_rival_a_thread_or_past_the_cap() {
        let home = scratch("gone-bound");
        let mut scanner = CodexScanner::new(home.clone());
        let ms = |s: u64| T0 * 1000 + s;
        scanner.sessions_at(&[tui(10, 5, "/work/a")], ms(40_000));
        scanner.sessions_at(&[tui(11, 60, "/work/a")], ms(70_000));
        assert_eq!(scanner.gone.len(), 1, "10 left at 70 s and 11 started at 60 s");
        scanner.sessions_at(&[tui(12, 100, "/work/a")], ms(110_000));
        let pids: Vec<u32> = scanner.gone.iter().map(|g| g.proc.pid).collect();
        assert_eq!(pids, [11], "10 left before the oldest live start, so it is dropped");
        for pid in 20..20 + u32::try_from(GONE_CAP).unwrap() + 10 {
            scanner.sessions_at(&[tui(pid, 100, "/work/a")], ms(120_000));
        }
        assert_eq!(scanner.gone.len(), GONE_CAP);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_tui_that_one_scan_missed_is_not_a_rival_of_its_own_thread() {
        let home = scratch("flap");
        let (a, b) = (tui(10, 5, "/work/a"), tui(11, 105, "/work/a"));
        let ms = |s: u64| T0 * 1000 + s;
        let mut scanner = CodexScanner::new(home.clone());
        scanner.sessions_at(&[a.clone(), b.clone()], ms(200_000));
        // The scan misses TUI 11 once, then sees it again. Its /new comes after that.
        scanner.sessions_at(std::slice::from_ref(&a), ms(201_000));
        scanner.sessions_at(&[a.clone(), b.clone()], ms(202_000));
        assert!(scanner.gone.is_empty(), "{:?}", scanner.gone);
        // The pure rule too: a gone entry equal to a live process is skipped.
        let gone = Gone { proc: b.clone(), ended_ms: ms(201_000) };
        assert_eq!(
            judge(&thread(150_000, 1, W), std::slice::from_ref(&b), &[gone]),
            Verdict::Owner(11)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// TUI 10 stays. TUI 11 starts at 100 s in the same cwd and leaves at 150 s. Then more than
    /// [`GONE_CAP`] other TUIs of the cwd come and go, so TUI 11 is dropped from the memory.
    fn scanner_after_a_rival_left_and_was_dropped(
        home: &Path,
        before_drop: impl FnOnce(&mut CodexScanner, &CodexProc),
    ) -> (CodexScanner, CodexProc) {
        let ms = |s: u64| T0 * 1000 + s;
        let (b, a) = (tui(10, 5, "/work/a"), tui(11, 100, "/work/a"));
        let mut scanner = CodexScanner::new(home.to_path_buf());
        scanner.sessions_at(&[b.clone(), a], ms(120_000));
        scanner.sessions_at(std::slice::from_ref(&b), ms(150_000));
        before_drop(&mut scanner, &b);
        for i in 0..GONE_CAP + 5 {
            let pid = 100 + u32::try_from(i).unwrap();
            let step = 151_000 + 1_000 * u64::try_from(i).unwrap();
            scanner.sessions_at(&[b.clone(), tui(pid, 140, "/work/a")], ms(step));
        }
        assert!(scanner.gone.iter().all(|g| g.proc.pid != 11), "TUI 11 was dropped");
        (scanner, b)
    }

    #[test]
    fn a_thread_that_only_a_gone_tui_could_own_stays_closed_after_that_tui_is_dropped() {
        let home = scratch("closed-final");
        let first_a = id_at(100_400, 1); // the first thread of TUI 11, written late
        let (mut scanner, b) = scanner_after_a_rival_left_and_was_dropped(&home, |scanner, b| {
            rollout(&home, "2026/09/30", &first_a, "/work/a");
            let got = scanner.sessions_at(std::slice::from_ref(b), T0 * 1000 + 151_000);
            assert!(got.is_empty(), "the thread is closed: {got:?}");
            assert_eq!(scanner.settled.get(&first_a), Some(&Settled::Closed), "and it is kept");
        });
        let got = scanner.sessions_at(&[b], T0 * 1000 + 300_000);
        assert!(got.is_empty(), "the live pane must not take it: {got:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_thread_that_appears_after_its_rival_was_dropped_is_in_doubt_but_a_later_one_is_not() {
        let home = scratch("horizon");
        let (mut scanner, b) = scanner_after_a_rival_left_and_was_dropped(&home, |_, _| {});
        // The daemon writes the first thread of TUI 11 only now.
        rollout(&home, "2026/09/30", &id_at(100_400, 1), "/work/a");
        let got = scanner.sessions_at(std::slice::from_ref(&b), T0 * 1000 + 300_000);
        assert!(got.is_empty(), "TUI 11 may own it: {got:?}");
        // A /new after everything that was dropped is TUI 10's.
        let mine = id_at(900_000, 2);
        rollout(&home, "2026/09/30", &mine, "/work/a");
        let got = scanner.sessions_at(&[b], T0 * 1000 + 950_000);
        assert_eq!(ids_of(&got), vec![(10, mine.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_tui_that_closed_before_an_empty_scan_is_still_a_rival() {
        let home = scratch("empty-scan");
        let ms = |s: u64| T0 * 1000 + s;
        let mut scanner = CodexScanner::new(home.clone());
        scanner.sessions_at(&[tui(10, 5, W)], ms(251_100));
        // TUI 10 made a /new at 251.2 s and closed. The scan finds nothing.
        assert!(scanner.sessions_at(&[], ms(251_800)).is_empty());
        // A resumed TUI starts in the same second, so it makes no first thread. The daemon now
        // writes the /new of TUI 10.
        let late = id_at(251_200, 1);
        rollout(&home, "2026/09/30", &late, W);
        let got = scanner.sessions_at(&[resumed(11, 251, W)], ms(252_000));
        assert_eq!(ids_of(&got), vec![(11, id_at(0, 99).as_str().to_owned())], "not {late:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_gone_tui_keeps_the_last_cwd_that_a_scan_could_read() {
        let home = scratch("gone-cwd");
        let ms = |s: u64| T0 * 1000 + s;
        let (a, b) = (tui(10, 5, "/work/a"), tui(11, 6, "/work/b"));
        let mut scanner = CodexScanner::new(home.clone());
        scanner.sessions_at(&[a.clone(), b.clone()], ms(100_000));
        // The last scan that saw TUI 10 could not read its cwd.
        let blind_a = CodexProc { cwd: None, ..a };
        scanner.sessions_at(&[blind_a, b.clone()], ms(101_000));
        scanner.sessions_at(std::slice::from_ref(&b), ms(200_000));
        assert_eq!(scanner.gone[0].proc.cwd.as_deref(), Some("/work/a"));
        // A thread in another cwd is TUI 11's, not in doubt because of TUI 10.
        let mine = id_at(150_000, 1);
        rollout(&home, "2026/09/30", &mine, "/work/b");
        let got = scanner.sessions_at(std::slice::from_ref(&b), ms(201_000));
        assert_eq!(ids_of(&got), vec![(11, mine.as_str().to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_rollout_seen_half_written_is_read_again_and_a_hopeless_one_is_given_up() {
        let home = scratch("retry");
        let id = id_at(5_320, 1);
        let dir = home.join("sessions/2026/09/30");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", id.as_str()));
        std::fs::write(&path, "{\"type\":\"session_m").unwrap();
        let mut scanner = CodexScanner::new(home.clone());
        let procs = [tui(10, 5, "/work/a")];
        assert!(scanner.sessions(&procs).is_empty());
        rollout(&home, "2026/09/30", &id, "/work/a");
        assert_eq!(ids_of(&scanner.sessions(&procs)), vec![(10, id.as_str().to_owned())]);
        // A file that never parses stops being read after a few tries.
        let bad = id_at(6_000, 2);
        let bad_path = dir.join(format!("rollout-2026-09-30T12-00-00-{}.jsonl", bad.as_str()));
        std::fs::write(&bad_path, "junk").unwrap();
        for _ in 0..META_ATTEMPTS {
            scanner.sessions(&procs);
        }
        assert_eq!(scanner.failures.get(&bad_path), Some(&META_ATTEMPTS));
        let _ = std::fs::remove_dir_all(&home);
    }
}
