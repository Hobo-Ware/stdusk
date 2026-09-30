//! The per-pane agent record: which session the pane's agent runs, and how long to keep it while
//! the agent is out of sight. `Track::scan` holds every rule, as one pure function of the old
//! record, the shell's status and one scan result. `PtyTerm` only calls it. Every time is
//! passed in as data, so `Track` never reads a clock.
//!
//! Words: a *status* is the exit status the shell reports for a command (OSC 133 `D` from stdusk's
//! own hook). A *crash* is a status this run saw that says a fault signal killed the program. A
//! *crash hint* is a saved record marked `crashed`: its session is typed without Enter. Only a
//! crash of this run makes `Bind::Crashed`, and every record from disk or a handoff starts as a
//! hint or as `Adopted`, never as `Crashed`.
//!
//! Rules:
//! - A source that names a session always wins (the Claude registry, or the Codex rollout match). This covers `/clear` and `/new`.
//! - A silent agent (no source names its session) binds a waiting record only if its command line
//!   holds the record's session id (`claude --resume <id>`). Any other silent agent drops the
//!   record. An adopted record (a live handoff) binds to the first agent of its kind, because that
//!   agent is the same process and its command line may be a bare `claude`.
//! - A missing agent makes the record gone after two scans in a row. A shutdown may kill the agent a
//!   moment before stdusk saves, so one miss must not erase anything. An adopted record follows the
//!   same rule, so a handoff that lost its agent cannot wait forever.
//! - The shell reports the status of each command. The status is the shell's foreground job, so
//!   Ctrl+Z, `bg`, `wait` and an empty-prompt Ctrl+C in bash also produce one. A status alone
//!   therefore proves nothing. It stamps the record as ended, and the first scan taken after the
//!   status decides. A scan that still shows the agent means the agent lives and the status is
//!   ignored. Otherwise a crash keeps the session and any other status ends it. Only the first
//!   status after the agent counts. The pane keeps one status between two takes, by the egui frame
//!   in `main.rs` or by a procwatch scan step.
//! - A record that no agent ever bound (a restore, or an adopted pane) drops at the first status.
//!   This includes a crash hint, also one that a handoff carried, so a hint cannot outlive the
//!   pane's next command.
//! - A crash of this run survives later commands until an agent binds that names the session (a
//!   source, or its command line). Any other agent drops it.

use std::time::Instant;

use crate::agents::{AgentSession, Seen};
use crate::session::SavedAgent;

/// SIGBUS is the one crash signal whose number depends on the OS.
const SIGBUS: i32 = if cfg!(target_os = "macos") { 10 } else { 7 };

/// Signals that kill a program by a fault or by the out-of-memory killer: SIGILL, SIGTRAP,
/// SIGABRT, SIGBUS, SIGFPE, SIGKILL and SIGSEGV. A user exit, Ctrl+C (SIGINT), SIGTERM and SIGHUP
/// are not crashes, and neither is a stop signal.
const CRASH_SIGNALS: [i32; 7] = [4, 5, 6, SIGBUS, 8, 9, 11];

/// A shell reports a program that died of signal `n` as status `128 + n`.
fn is_crash(code: i32) -> bool {
    code > 128 && CRASH_SIGNALS.contains(&(code - 128))
}

/// Scans in a row without the agent before its record is gone.
const MISSES_BEFORE_GONE: u8 = 2;

/// Scans a gone record waits for the status of its agent. With the two that made it gone, this
/// makes five scans in all, then the record is given up.
const GONE_WAIT_SCANS: u8 = 3;

/// One status the shell reported, and when the reader thread got it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) code: i32,
    pub(crate) at: Instant,
}

/// How a session ties to a running agent process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bind {
    /// stdusk reopened the session and types the resume command. No agent has bound yet. Codex
    /// names its session only on the first turn, so an idle resumed pane would look unknown.
    /// `crash_hint` marks a session kept only because its agent crashed in an earlier run.
    Waiting { crash_hint: bool },
    /// A live handoff carried the record. Its agent runs already. `missed` counts the scans in a
    /// row without it.
    Adopted { missed: u8 },
    /// The agent process `pid` runs the session. `missed` counts the scans in a row without it.
    Bound { pid: u32, missed: u8 },
    /// A command ended in the shell at `at` while `pid` was bound. The first scan taken after `at`
    /// decides: it shows `pid` (the agent lives) or it does not. `crash` says how the command ended.
    Ended { pid: u32, at: Instant, crash: bool },
    /// The agent is gone and its status is on its way. The record is not saved. The status decides:
    /// a crash brings it back, anything else ends it.
    Gone { scans_left: u8 },
    /// The agent died of a crash signal in this run. The record waits for a new agent.
    Crashed,
}

impl Bind {
    /// The session is kept only because its agent crashed, so it is saved as a crash hint.
    fn kept_by_crash(self) -> bool {
        matches!(
            self,
            Self::Waiting { crash_hint: true } | Self::Ended { crash: true, .. } | Self::Crashed
        )
    }

    /// This bind after one scan without the agent. `None` gives the record up.
    fn missed(self) -> Option<Self> {
        match self {
            Self::Bound { pid, missed } if missed + 1 < MISSES_BEFORE_GONE => {
                Some(Self::Bound { pid, missed: missed + 1 })
            }
            Self::Adopted { missed } if missed + 1 < MISSES_BEFORE_GONE => {
                Some(Self::Adopted { missed: missed + 1 })
            }
            Self::Bound { .. } | Self::Adopted { .. } => {
                Some(Self::Gone { scans_left: GONE_WAIT_SCANS })
            }
            Self::Gone { scans_left: 1 } => None,
            Self::Gone { scans_left } => Some(Self::Gone { scans_left: scans_left - 1 }),
            // These wait for their agent however long that takes. `Ended` never gets here: `seen`
            // settles it before it counts a miss.
            Self::Waiting { .. } | Self::Ended { .. } | Self::Crashed => Some(self),
        }
    }
}

/// A session the pane holds, and its link to the agent process.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    session: AgentSession,
    bind: Bind,
}

/// What a pane holds about its agent session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Track(Option<Record>);

impl Track {
    /// A track that starts from a session stdusk reopened. `crash_hint` says the record was kept
    /// after a crash, so its command is typed and not run.
    pub(crate) fn restored(session: AgentSession, crash_hint: bool) -> Self {
        Self::with(session, Bind::Waiting { crash_hint })
    }

    /// A track that starts from the record of a live handoff. A crash hint has no live agent, so
    /// it stays a hint and drops at the first status, as a restored one does.
    pub(crate) fn adopted(session: AgentSession, crash_hint: bool) -> Self {
        let bind =
            if crash_hint { Bind::Waiting { crash_hint } } else { Bind::Adopted { missed: 0 } };
        Self::with(session, bind)
    }

    fn with(session: AgentSession, bind: Bind) -> Self {
        Self(Some(Record { session, bind }))
    }

    fn bound(session: AgentSession, pid: u32) -> Self {
        Self::with(session, Bind::Bound { pid, missed: 0 })
    }

    /// The record to save, with its crash mark. A record that is gone is not saved: without a
    /// crash status it is on its way out.
    pub(crate) fn saved(&self) -> Option<SavedAgent> {
        let record = self.visible()?;
        Some(record.session.to_saved(record.bind.kept_by_crash()))
    }

    fn visible(&self) -> Option<&Record> {
        self.0.as_ref().filter(|r| !matches!(r.bind, Bind::Gone { .. }))
    }

    /// One scan taken at `taken`: the status the shell reported since the last one comes first, so
    /// a crash is known before the scan that finds the agent gone.
    pub(crate) fn scan(self, status: Option<Status>, seen: Seen, taken: Instant) -> Self {
        self.status(status).seen(seen, taken)
    }

    /// Apply a status alone. The final snapshot before quit uses this, so a crash status that no
    /// scan has seen yet is still saved.
    pub(crate) fn status(self, status: Option<Status>) -> Self {
        match status {
            Some(status) => self.command_ended(status),
            None => self,
        }
    }

    /// The first status after the agent. Every state either ends here or ignores later statuses.
    fn command_ended(self, status: Status) -> Self {
        let Some(Record { session, bind }) = self.0 else { return self };
        let crash = is_crash(status.code);
        let bind = match bind {
            Bind::Bound { pid, .. } => Bind::Ended { pid, at: status.at, crash },
            Bind::Gone { .. } if crash => Bind::Crashed,
            // The user moved on, or the resume failed in under a scan.
            Bind::Gone { .. } | Bind::Waiting { .. } | Bind::Adopted { .. } => return Self(None),
            Bind::Ended { .. } | Bind::Crashed => bind,
        };
        Self::with(session, bind)
    }

    /// A scan when no record exists: only a named session starts one.
    fn started_by(seen: Seen) -> Self {
        match seen {
            Seen::Agent { pid, session: Some(session), .. } => Self::bound(session, pid),
            Seen::Agent { .. } | Seen::NoAgent => Self(None),
        }
    }

    /// One scan result. The order of the arms matters: a named session beats every other rule.
    fn seen(self, seen: Seen, taken: Instant) -> Self {
        let Some(Record { session, bind }) = self.0 else { return Self::started_by(seen) };
        let seen_pid = match &seen {
            Seen::Agent { pid, .. } => Some(*pid),
            Seen::NoAgent => None,
        };
        let bind = match bind {
            // A scan taken before the status is no evidence about it.
            Bind::Ended { at, .. } if taken <= at => return Self::with(session, bind),
            // The scan shows the process: the agent lives and the status was another command.
            Bind::Ended { pid, .. } if seen_pid == Some(pid) => Bind::Bound { pid, missed: 0 },
            Bind::Ended { crash: true, .. } => Bind::Crashed,
            Bind::Ended { .. } if seen_pid.is_none() => return Self(None),
            // The agent ended. A named session starts a record, and so does the same session again
            // by its command line (`codex resume <id>` typed within one scan). Judge it as waiting.
            Bind::Ended { .. } => Bind::Waiting { crash_hint: false },
            other => other,
        };
        let Seen::Agent { kind, pid, session: named, argv_session } = seen else {
            return bind.missed().map_or(Self(None), |bind| Self::with(session, bind));
        };
        let same_kind = session.kind == kind;
        match (bind, named) {
            // A source named the session: it replaces whatever was there (`/clear`, `/new`).
            (_, Some(named)) => Self::bound(named, pid),
            // The agent we already follow, and its source is silent (an unreadable file, or Codex
            // before its first turn). One silent scan says nothing against the session.
            (Bind::Bound { pid: followed, .. }, None) if followed == pid && same_kind => {
                Self::bound(session, pid)
            }
            // The agent of a live handoff is the first agent of its kind.
            (Bind::Adopted { .. }, None) if same_kind => Self::bound(session, pid),
            // The resume itself: its command line names the session.
            (Bind::Waiting { .. } | Bind::Crashed, None)
                if same_kind && argv_session.as_ref() == Some(&session.id) =>
            {
                Self::bound(session, pid)
            }
            // A silent agent that is not the followed one (a restart, another kind, a fresh
            // session): the record is not its.
            (_, None) => Self(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentKind, SessionId};
    use std::time::Duration;

    const ID_A: &str = "0c2cbc96-1111-4222-8333-444455556666";
    const ID_B: &str = "d8b21abe-aaaa-4bbb-8ccc-ddddeeeeffff";
    const BUS: i32 = if cfg!(target_os = "macos") { 138 } else { 135 };
    const CRASHES: [i32; 7] = [132, 133, 134, BUS, 136, 137, 139];
    /// Includes 146 (SIGTSTP on macOS, what Ctrl+Z prints) and 148 (SIGTSTP on Linux).
    const NOT_CRASHES: [i32; 8] = [0, 1, 127, 129, 130, 143, 146, 148];

    /// A fixed clock: `t(n)` is `n` seconds after an arbitrary start.
    fn t(secs: u64) -> Instant {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now) + Duration::from_secs(secs)
    }

    /// The tests bind at `t(0)`, the shell reports statuses at `EXIT_AT`, and later scans run at
    /// `LATER`. A scan at `BEFORE` predates the statuses.
    const BEFORE: u64 = 5;
    const EXIT_AT: u64 = 10;
    const LATER: u64 = 20;

    fn session(kind: AgentKind, id: &str) -> AgentSession {
        let id = SessionId::parse(id).expect("test id is canonical");
        AgentSession { kind, id, cwd: "/a".into(), transcript: None }
    }

    /// A scan where a source named session `id` for the Claude process `pid`.
    fn named(pid: u32, id: &str) -> Seen {
        Seen::Agent {
            kind: AgentKind::Claude,
            pid,
            session: Some(session(AgentKind::Claude, id)),
            argv_session: None,
        }
    }

    /// A scan where an agent runs but no source named its session.
    fn silent(kind: AgentKind, pid: u32) -> Seen {
        Seen::Agent { kind, pid, session: None, argv_session: None }
    }

    /// A silent agent whose command line names session `id` (`codex resume <id>`).
    fn resuming(kind: AgentKind, pid: u32, id: &str) -> Seen {
        Seen::Agent { kind, pid, session: None, argv_session: SessionId::parse(id) }
    }

    /// Scans taken after every status.
    fn scans(track: Track, seen: &[Seen]) -> Track {
        seen.iter().fold(track, |track, s| track.scan(None, s.clone(), t(LATER)))
    }

    fn misses(track: Track, n: usize) -> Track {
        scans(track, &vec![Seen::NoAgent; n])
    }

    /// The status the shell reported at `EXIT_AT`.
    fn status(code: i32) -> Status {
        Status { code, at: t(EXIT_AT) }
    }

    /// The shell reports `code` at `EXIT_AT`, and no scan has run since.
    fn ended(track: Track, code: i32) -> Track {
        track.status(Some(status(code)))
    }

    /// The saved session id, if the pane would save one.
    fn id(track: &Track) -> Option<&str> {
        track.visible().map(|r| r.session.id.as_str())
    }

    fn crashed(track: &Track) -> Option<bool> {
        track.saved().map(|a| a.crashed)
    }

    /// A pane whose Claude process 10 runs session A.
    fn running() -> Track {
        Track::default().scan(None, named(10, ID_A), t(0))
    }

    /// A restored Codex pane that waits for its agent.
    fn waiting() -> Track {
        Track::restored(session(AgentKind::Codex, ID_A), false)
    }

    /// A Codex pane whose process 50 runs session A but names no session (before its first turn).
    fn bound_silent() -> Track {
        waiting().scan(None, resuming(AgentKind::Codex, 50, ID_A), t(0))
    }

    // --- The status table -------------------------------------------------------------------------

    #[test]
    fn a_crash_status_is_128_plus_a_fault_signal() {
        for status in CRASHES {
            assert!(is_crash(status), "status {status}");
        }
        for status in NOT_CRASHES {
            assert!(!is_crash(status), "status {status}");
        }
        // SIGBUS is 10 on macOS and 7 on Linux. The other number is another signal there.
        let other_bus = if cfg!(target_os = "macos") { 135 } else { 138 };
        assert!(!is_crash(other_bus));
    }

    // --- Following a running agent ----------------------------------------------------------------

    #[test]
    fn a_named_session_starts_and_replaces_the_record() {
        assert_eq!(id(&running()), Some(ID_A));
        // The session changes under a running agent (`/clear`), and under a new process.
        assert_eq!(id(&scans(running(), &[named(10, ID_B)])), Some(ID_B));
        assert_eq!(id(&scans(running(), &[named(11, ID_B)])), Some(ID_B));
        // An agent with no session and no record gives nothing to keep.
        assert_eq!(id(&scans(Track::default(), &[silent(AgentKind::Claude, 10)])), None);
    }

    #[test]
    fn a_silent_scan_of_the_same_agent_keeps_the_record() {
        // One unreadable registry file must not erase a live session.
        let track = scans(running(), &vec![silent(AgentKind::Claude, 10); 5]);
        assert_eq!(id(&track), Some(ID_A));
        // A silent scan also resets the count of misses in a row.
        let track = scans(misses(running(), 1), &[silent(AgentKind::Claude, 10)]);
        assert_eq!(id(&misses(track, 1)), Some(ID_A));
    }

    #[test]
    fn a_silent_scan_of_another_process_or_kind_drops_the_record() {
        assert_eq!(id(&scans(running(), &[silent(AgentKind::Claude, 11)])), None);
        assert_eq!(id(&scans(running(), &[silent(AgentKind::Codex, 10)])), None);
    }

    #[test]
    fn two_misses_in_a_row_make_the_record_gone() {
        assert_eq!(id(&misses(running(), 1)), Some(ID_A));
        assert_eq!(id(&misses(running(), 2)), None);
        // A sighting between two misses resets the count.
        let track = scans(misses(running(), 1), &[named(10, ID_B)]);
        assert_eq!(id(&misses(track, 1)), Some(ID_B));
    }

    // --- A restored record ------------------------------------------------------------------------

    #[test]
    fn a_waiting_record_waits_for_its_agent_however_long_it_takes() {
        // Prefill, a slow start, or a failed resume: no agent runs for many scans.
        assert_eq!(id(&misses(waiting(), 50)), Some(ID_A));
    }

    #[test]
    fn a_waiting_record_binds_only_to_an_agent_that_names_its_session_in_argv() {
        let bound = bound_silent();
        assert_eq!(id(&bound), Some(ID_A));
        // The agent stays silent (Codex before its first turn): the record stays.
        assert_eq!(id(&scans(bound.clone(), &vec![silent(AgentKind::Codex, 50); 3])), Some(ID_A));
        // Two misses in a row make it gone, one does not.
        assert_eq!(id(&misses(bound.clone(), 1)), Some(ID_A));
        assert_eq!(id(&misses(bound.clone(), 2)), None);
        // A quick restart, another process of the same kind with no id yet: the old id must go.
        assert_eq!(id(&scans(bound, &[silent(AgentKind::Codex, 51)])), None);
    }

    #[test]
    fn a_waiting_record_drops_for_any_other_silent_agent() {
        let cases = [
            ("no session in argv", silent(AgentKind::Codex, 50)),
            ("another session in argv", resuming(AgentKind::Codex, 50, ID_B)),
            ("another kind", silent(AgentKind::Claude, 50)),
            ("another kind, right id", resuming(AgentKind::Claude, 50, ID_A)),
        ];
        for (why, seen) in cases {
            assert_eq!(id(&scans(waiting(), &[seen])), None, "{why}");
        }
    }

    #[test]
    fn a_named_session_replaces_a_waiting_record_and_then_follows_the_normal_rules() {
        let codex = Seen::Agent {
            kind: AgentKind::Codex,
            pid: 50,
            session: Some(session(AgentKind::Codex, ID_B)),
            argv_session: None,
        };
        for start in [waiting(), bound_silent()] {
            let track = scans(start, std::slice::from_ref(&codex));
            assert_eq!(id(&track), Some(ID_B));
            // Once followed, a silent scan of the same agent keeps it, and a miss pair makes it gone.
            let silent_scan = silent(AgentKind::Codex, 50);
            assert_eq!(id(&scans(track.clone(), &[silent_scan])), Some(ID_B));
            assert_eq!(id(&misses(track, 2)), None);
        }
    }

    #[test]
    fn an_adopted_record_binds_to_the_first_agent_of_its_kind() {
        let adopted = || Track::adopted(session(AgentKind::Codex, ID_A), false);
        // The agent runs already, so its command line may be a bare `codex`.
        assert_eq!(id(&scans(adopted(), &[silent(AgentKind::Codex, 50)])), Some(ID_A));
        assert_eq!(crashed(&scans(adopted(), &[silent(AgentKind::Codex, 50)])), Some(false));
        assert_eq!(id(&scans(adopted(), &[silent(AgentKind::Claude, 50)])), None);
    }

    #[test]
    fn an_adopted_record_follows_the_two_miss_rule_so_it_cannot_wait_forever() {
        // The agent of a live handoff is there at the first scan or it is not. One miss can be the
        // shutdown race, two in a row are not.
        let adopted = || Track::adopted(session(AgentKind::Codex, ID_A), false);
        assert_eq!(id(&misses(adopted(), 1)), Some(ID_A));
        assert_eq!(id(&misses(adopted(), 2)), None);
        // A sighting resets nothing to fear: it binds, and the bound rules apply from then on.
        let track = scans(misses(adopted(), 1), &[silent(AgentKind::Codex, 50)]);
        assert_eq!(id(&misses(track.clone(), 1)), Some(ID_A));
        assert_eq!(id(&misses(track, 2)), None);
        // A crash status still brings back a record that just went gone.
        let gone = misses(adopted(), 2);
        assert_eq!(id(&ended(gone, 137)), Some(ID_A));
    }

    #[test]
    fn a_record_no_agent_ever_bound_drops_at_the_first_status() {
        // A command ended in this shell and no agent was seen: the resume failed in under a scan,
        // or the user cleared the typed line and moved on. A crash hint expires the same way.
        let records = [
            ("waiting", waiting()),
            ("crash hint", Track::restored(session(AgentKind::Codex, ID_A), true)),
            ("adopted", Track::adopted(session(AgentKind::Codex, ID_A), false)),
            ("adopted crash hint", Track::adopted(session(AgentKind::Codex, ID_A), true)),
        ];
        for (name, record) in records {
            for status in CRASHES.into_iter().chain(NOT_CRASHES) {
                assert_eq!(id(&ended(record.clone(), status)), None, "{name}, status {status}");
            }
        }
        // A pane without integration reports no status, so a record that waits for its agent
        // waits as before. An adopted one has a running agent and follows the miss rule.
        assert!(id(&misses(waiting(), 10)).is_some());
        assert!(id(&misses(Track::adopted(session(AgentKind::Codex, ID_A), true), 10)).is_some());
    }

    // --- Statuses ----------------------------------------------------------------------------

    #[test]
    fn a_crash_status_keeps_the_record_through_any_number_of_scans() {
        for status in CRASHES {
            let track = misses(ended(running(), status), 20);
            assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)), "status {status}");
        }
        // From a bound record whose agent stayed silent, too.
        let track = misses(ended(bound_silent(), 137), 20);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)));
    }

    #[test]
    fn a_crash_status_after_the_record_was_gone_brings_it_back() {
        for status in CRASHES {
            let gone = misses(running(), 2);
            assert_eq!(id(&gone), None, "gone while the status is pending");
            let track = ended(gone, status);
            assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)), "status {status}");
            assert_eq!(id(&misses(track, 20)), Some(ID_A));
        }
    }

    #[test]
    fn any_other_status_ends_the_record_once_a_scan_finds_the_agent_gone() {
        for status in NOT_CRASHES {
            assert_eq!(id(&misses(ended(running(), status), 1)), None, "bound, status {status}");
            assert_eq!(id(&ended(misses(running(), 2), status)), None, "gone, status {status}");
        }
    }

    #[test]
    fn a_scan_that_still_shows_the_agent_outlives_every_status() {
        // Ctrl+Z prints 146 on macOS. `bg`, `wait` and `true` print their own. A crash status can
        // come from an unrelated command while the agent sleeps. The agent lives in all of them.
        let live = [
            ("named", running(), named(10, ID_A)),
            ("silent", bound_silent(), silent(AgentKind::Codex, 50)),
        ];
        for (name, record, seen) in live {
            for status in CRASHES.into_iter().chain(NOT_CRASHES) {
                let track = scans(ended(record.clone(), status), std::slice::from_ref(&seen));
                assert_eq!(
                    (id(&track), crashed(&track)),
                    (Some(ID_A), Some(false)),
                    "{name}, status {status}"
                );
                // It is a followed agent again: the normal rules apply from then on.
                assert_eq!(id(&misses(track.clone(), 1)), Some(ID_A));
                assert_eq!(id(&misses(track, 2)), None);
            }
        }
    }

    #[test]
    fn a_followed_agent_takes_a_crash_status_again_after_it_was_alive() {
        let track = scans(ended(running(), 139), &[named(10, ID_A)]);
        let track = misses(ended(track, 134), 1);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)));
    }

    #[test]
    fn a_scan_taken_before_the_status_is_no_evidence() {
        // The status arrives first. The scan of the same step was taken before it: it may show an
        // agent that has died since. Neither a live agent nor a gone one is decided by it.
        for at in [BEFORE, EXIT_AT] {
            let stale = |track: Track, seen| track.scan(Some(status(0)), seen, t(at));
            let track = stale(running(), named(10, ID_A));
            assert_eq!(id(&track), Some(ID_A), "kept until a scan after the status decides");
            assert_eq!(id(&misses(track.clone(), 1)), None, "the next scan finds it gone");
            assert_eq!(
                id(&scans(track, &[named(10, ID_A)])),
                Some(ID_A),
                "or finds it alive: the status was another command"
            );
            // A stale scan of a gone agent decides nothing either.
            let track = stale(running(), Seen::NoAgent);
            assert_eq!(id(&scans(track, &[named(10, ID_A)])), Some(ID_A));
        }
        // A crash status is held the same way.
        let track = running().scan(Some(status(137)), named(10, ID_A), t(BEFORE));
        assert_eq!(crashed(&track), Some(true), "a quit now saves the crash");
        assert_eq!(crashed(&misses(track.clone(), 1)), Some(true));
        assert_eq!(crashed(&scans(track, &[named(10, ID_A)])), Some(false));
    }

    #[test]
    fn a_status_that_never_arrives_gives_up_after_five_scans() {
        // fish, or integration off: two misses make the record gone, three more give it up. A much
        // later crash status cannot bring it back.
        let track = misses(running(), 4);
        assert_eq!(id(&ended(track.clone(), 137)), Some(ID_A), "still waiting at the fourth");
        assert_eq!(id(&ended(misses(running(), 5), 137)), None);
        assert_eq!(id(&ended(misses(track, 20), 137)), None);
    }

    #[test]
    fn only_the_first_status_after_the_agent_counts() {
        // The agent exits cleanly, then an unrelated command crashes in a later step: nothing
        // comes back. (Within one step the pane keeps only the first status.)
        for (first, later) in [(0, 134), (1, 139)] {
            let track = misses(ended(ended(running(), first), later), 20);
            assert_eq!(id(&track), None, "statuses {first}, {later}");
            let track = ended(ended(misses(running(), 2), first), later);
            assert_eq!(id(&track), None, "gone, statuses {first}, {later}");
        }
        // The agent crashes, then an unrelated command ends: the crash record stays.
        let track = misses(ended(ended(running(), 137), 0), 20);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)));
        // A later command in a later step does not touch it either.
        let track = ended(misses(ended(running(), 137), 2), 0);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)));
    }

    #[test]
    fn a_status_with_no_record_changes_nothing() {
        assert_eq!(id(&ended(Track::default(), 137)), None);
        assert_eq!(id(&Track::default().scan(Some(status(137)), Seen::NoAgent, t(LATER))), None);
    }

    #[test]
    fn the_scan_applies_the_status_before_it_looks_at_the_agent() {
        // Status and "agent gone" arrive in one step: the crash is known before the miss counts.
        let track = running().scan(Some(status(137)), Seen::NoAgent, t(LATER));
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(true)));
        // A clean exit in the same step ends the record.
        assert_eq!(id(&running().scan(Some(status(0)), Seen::NoAgent, t(LATER))), None);
    }

    #[test]
    fn the_same_session_started_again_within_one_scan_stays_tracked() {
        // A silent Codex that ended cleanly, and `codex resume <id>` typed before the next scan:
        // its command line names the session, so the record goes on with the new process.
        let ended_cleanly = || ended(bound_silent(), 0);
        let track = scans(ended_cleanly(), &[resuming(AgentKind::Codex, 51, ID_A)]);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(false)));
        assert_eq!(id(&misses(track, 2)), None, "and it follows the normal rules from then on");
        // Anything else that shows up after a clean end is not this session.
        let others = [
            silent(AgentKind::Codex, 51),
            resuming(AgentKind::Codex, 51, ID_B),
            resuming(AgentKind::Claude, 51, ID_A),
            Seen::NoAgent,
        ];
        for seen in others {
            assert_eq!(id(&scans(ended_cleanly(), std::slice::from_ref(&seen))), None, "{seen:?}");
        }
        // A named session still starts a record of its own.
        let named_codex = Seen::Agent {
            kind: AgentKind::Codex,
            pid: 51,
            session: Some(session(AgentKind::Codex, ID_B)),
            argv_session: None,
        };
        assert_eq!(id(&scans(ended_cleanly(), &[named_codex])), Some(ID_B));
    }

    // --- The crashed agent, seen again ------------------------------------------------------------

    #[test]
    fn a_new_agent_binds_a_kept_record_only_by_argv_and_clears_the_crash_mark() {
        let kept = || misses(ended(running(), 137), 1);
        assert_eq!(crashed(&kept()), Some(true));
        // A named session replaces it.
        let track = scans(kept(), &[named(11, ID_B)]);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_B), Some(false)));
        // The resume itself names the session in its command line, and the mark goes.
        let track = scans(kept(), &[resuming(AgentKind::Claude, 11, ID_A)]);
        assert_eq!((id(&track), crashed(&track)), (Some(ID_A), Some(false)));
        assert_eq!(id(&misses(track, 2)), None, "and it follows the normal rules from then on");
        // Any other silent agent is a fresh session, not the crashed one.
        let others = [
            silent(AgentKind::Claude, 11),
            resuming(AgentKind::Claude, 11, ID_B),
            resuming(AgentKind::Codex, 11, ID_A),
        ];
        for seen in others {
            assert_eq!(id(&scans(kept(), std::slice::from_ref(&seen))), None, "{seen:?}");
        }
    }

    // --- Saving -----------------------------------------------------------------------------------

    #[test]
    fn only_a_record_kept_by_a_crash_status_is_saved_as_crashed() {
        assert_eq!(crashed(&running()), Some(false));
        assert_eq!(crashed(&waiting()), Some(false));
        assert_eq!(crashed(&Track::default()), None);
        // Kept by a crash status, in either order of status and debounce. The mark survives the
        // waiting scans.
        let before = ended(running(), 137);
        let after = ended(misses(running(), 2), 137);
        for track in [before, after] {
            assert_eq!(crashed(&misses(track, 10)), Some(true));
        }
        // A restored hint keeps its mark across another restart while nothing has run.
        let hint = Track::restored(session(AgentKind::Claude, ID_A), true);
        assert_eq!(crashed(&misses(hint, 3)), Some(true));
        // A hint that a live handoff carried is still a hint, and it drops at the first status.
        let carried = || Track::adopted(session(AgentKind::Claude, ID_A), true);
        assert_eq!(crashed(&misses(carried(), 3)), Some(true));
        assert_eq!(crashed(&ended(carried(), 0)), None);
    }

    #[test]
    fn a_status_no_scan_has_judged_yet_is_saved_by_what_it_says() {
        // A quit right after a status has no later scan. A crash is saved as a crash. Any other
        // status may be Ctrl+Z, so the record stays, as it does for a live agent.
        assert_eq!(crashed(&ended(running(), 137)), Some(true));
        assert_eq!(crashed(&ended(running(), 146)), Some(false));
        assert_eq!(id(&ended(running(), 146)), Some(ID_A));
    }

    #[test]
    fn a_gone_record_is_not_saved() {
        assert_eq!(misses(running(), 2).saved(), None);
        assert_eq!(misses(running(), 1).saved().map(|a| a.session), Some(ID_A.to_owned()));
    }
}
