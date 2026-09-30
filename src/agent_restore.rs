//! Restore plan: which saved agent sessions a relaunch reopens, how (typed with or without Enter),
//! and what to tell the user about the ones it does not. `agents` owns the session types, the
//! parsers and the file checks this module asks. `tabs` runs the plan leaf by leaf.
//!
//! Rules that hold everywhere here:
//! - A session resumes only when its id parses, its directory exists, its transcript exists, and
//!   no earlier leaf claimed the same `(agent, id)`.
//! - A session kept after a crash (a crash hint) is typed without Enter, whatever the setting says.
//!   A hint claims nothing, so a healthy record of the same session in another pane still resumes.
//! - A failure is a plain shell, one grouped toast, and a notice in the pane.

use std::collections::HashSet;
use std::path::Path;

use crate::agents::{AgentKind, AgentSession, ResumeFs, ResumeMode, SessionId};
use crate::session::SavedAgent;

/// Why a saved session did not resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkipReason {
    BadId,
    DirectoryMissing,
    TranscriptMissing,
    Duplicate,
    /// A live handoff could not hand this pane's shell over, so the pane got a fresh shell.
    NotHandedOver,
    /// The fallback found the tty busy, so the resume command was not typed.
    ShellBusy,
}

impl SkipReason {
    /// The reasons in the order a count list names them.
    const ALL: [Self; 6] = [
        Self::TranscriptMissing,
        Self::DirectoryMissing,
        Self::Duplicate,
        Self::NotHandedOver,
        Self::ShellBusy,
        Self::BadId,
    ];

    /// The reason for one session, and for several.
    fn forms(self) -> (&'static str, &'static str) {
        match self {
            Self::BadId => ("invalid session id", "invalid session ids"),
            Self::DirectoryMissing => ("directory missing", "directories missing"),
            Self::TranscriptMissing => ("transcript missing", "transcripts missing"),
            // True in every mode: with "type only" the other pane has the command typed, not run.
            Self::Duplicate => ("duplicate of another pane", "duplicates of another pane"),
            Self::NotHandedOver => ("shell not handed over", "shells not handed over"),
            Self::ShellBusy => ("shell busy at start", "shells busy at start"),
        }
    }

    pub(crate) fn text(self) -> &'static str {
        self.forms().0
    }

    /// `n` sessions with this reason, as "3 transcripts missing".
    fn counted(self, n: usize) -> String {
        let (one, many) = self.forms();
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// What restore tells the user inside a pane, until the user acts in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneNotice {
    /// The saved session did not resume, and the pane is a plain shell.
    NotResumed { kind: AgentKind, reason: SkipReason },
    /// The session ended by a crash signal. Its resume command is typed at the first prompt and
    /// waits for Enter.
    CrashHint { kind: AgentKind },
}

/// A saved session that did not resume, named by its directory for the toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skip {
    pub(crate) label: String,
    pub(crate) reason: SkipReason,
}

/// A saved session that restore reopens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resume {
    pub(crate) session: AgentSession,
    /// The saved record was kept only because its agent crashed (a crash hint). The pane keeps
    /// that mark until an agent binds to it.
    pub(crate) crash_hint: bool,
    /// The user's `resume_agents` setting.
    setting: ResumeMode,
}

impl Resume {
    pub(crate) fn new(session: AgentSession, crash_hint: bool, setting: ResumeMode) -> Self {
        Self { session, crash_hint, setting }
    }

    /// How to reopen: a crash hint is typed without Enter whatever the setting says, because a
    /// status of 137 cannot prove an out-of-memory kill (the user's own `kill -9` looks the same).
    pub(crate) fn mode(&self) -> ResumeMode {
        if self.crash_hint { ResumeMode::Prefill } else { self.setting }
    }

    /// The command that reopens the session, typed at the first prompt.
    pub(crate) fn input(&self) -> Vec<u8> {
        let line = match self.session.kind {
            AgentKind::Claude => format!("claude --resume {}", self.session.id.as_str()),
            AgentKind::Codex => format!("codex resume {}", self.session.id.as_str()),
        };
        match self.mode() {
            ResumeMode::Auto => format!("{line}\r").into_bytes(),
            ResumeMode::Prefill => line.into_bytes(),
        }
    }
}

/// The restore decisions of one session file. Leaves are decided in the order restore builds
/// them, so a conversation that two saved panes claim resumes in the first one only.
#[derive(Default)]
pub(crate) struct RestorePlan {
    claimed: HashSet<(AgentKind, SessionId)>,
    skips: Vec<Skip>,
    /// Sessions that come back as a typed hint after a crash.
    crash_hints: usize,
}

impl RestorePlan {
    /// The session to resume for one saved leaf. `Err` means a plain shell, and the reason is
    /// kept for the toast too. A leaf resumes only when its id parses, its directory exists, its
    /// transcript exists, and no earlier leaf claimed the same `(agent, id)`. A crash hint does
    /// not claim: it only types a command, and a healthy record of the session must not lose to it.
    pub(crate) fn resume(
        &mut self,
        saved: &SavedAgent,
        setting: ResumeMode,
        fs: &dyn ResumeFs,
    ) -> Result<Resume, SkipReason> {
        let session = self.check(saved, fs).inspect_err(|&reason| self.skipped(saved, reason))?;
        if saved.crashed {
            self.crash_hints += 1;
        } else {
            self.claimed.insert((session.kind, session.id.clone()));
        }
        Ok(Resume::new(session, saved.crashed, setting))
    }

    /// Record that a saved leaf did not resume, for the toast. `resume` does this for its own
    /// failures. A caller that decides a skip itself (a failed handoff) calls it directly.
    pub(crate) fn skipped(&mut self, saved: &SavedAgent, reason: SkipReason) {
        let label = Path::new(&saved.cwd)
            .file_name()
            .map_or_else(|| saved.cwd.clone(), |n| n.to_string_lossy().into_owned());
        self.skips.push(Skip { label, reason });
    }

    fn check(&self, saved: &SavedAgent, fs: &dyn ResumeFs) -> Result<AgentSession, SkipReason> {
        let session = AgentSession::from_saved(saved).ok_or(SkipReason::BadId)?;
        if !fs.is_dir(&session.cwd) {
            return Err(SkipReason::DirectoryMissing);
        }
        let transcript_ok = match (&session.transcript, session.kind) {
            (Some(path), _) => fs.is_file(path),
            (None, AgentKind::Claude) => fs.claude_transcript_exists(&session.id),
            // Codex is captured by the rollout scan, which names the rollout file.
            (None, AgentKind::Codex) => false,
        };
        if !transcript_ok {
            return Err(SkipReason::TranscriptMissing);
        }
        if self.claimed.contains(&(session.kind, session.id.clone())) {
            return Err(SkipReason::Duplicate);
        }
        Ok(session)
    }

    /// The one toast after restore, or `None` when nothing needs saying.
    pub(crate) fn into_toast(self) -> Option<String> {
        restore_toast(&self.skips, self.crash_hints)
    }
}

/// How many saved sessions the next launch reopens without a keypress: the ones restore would
/// resume, minus the crashed ones (typed as a hint) and all of them in "type only" mode.
pub(crate) fn auto_resumable(
    saved: &[SavedAgent],
    setting: ResumeMode,
    fs: &dyn ResumeFs,
) -> usize {
    let mut plan = RestorePlan::default();
    saved
        .iter()
        .filter(|s| plan.resume(s, setting, fs).is_ok_and(|r| r.mode() == ResumeMode::Auto))
        .count()
}

/// Skipped sessions that the toast names one by one. More than this, and it gives counts.
const MAX_NAMED_SKIPS: usize = 3;

/// The toast text: the sessions that did not resume, and the ones that only got a typed hint.
fn restore_toast(skips: &[Skip], crash_hints: usize) -> Option<String> {
    let skipped = (!skips.is_empty()).then(|| {
        let noun = if skips.len() == 1 { "session" } else { "sessions" };
        let detail: Vec<String> = if skips.len() <= MAX_NAMED_SKIPS {
            skips.iter().map(|s| format!("{} ({})", s.label, s.reason.text())).collect()
        } else {
            SkipReason::ALL
                .into_iter()
                .filter_map(|reason| {
                    let n = skips.iter().filter(|s| s.reason == reason).count();
                    (n > 0).then(|| reason.counted(n))
                })
                .collect()
        };
        format!("Could not resume {} {noun}: {}", skips.len(), detail.join(", "))
    });
    // Said at startup, before any prompt: the command is typed later, and a pane whose shell is
    // busy then says so itself (`PaneNotice`).
    let hint = match crash_hints {
        0 => None,
        1 => Some(
            "1 crashed session was not reopened. Its resume command will be typed at the prompt. Press Enter to run it."
                .to_owned(),
        ),
        n => Some(format!(
            "{n} crashed sessions were not reopened. Their resume commands will be typed at the prompts. Press Enter to run each one."
        )),
    };
    let parts: Vec<String> = [skipped, hint].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(". "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "0c2cbc96-1111-4222-8333-444455556666";
    const ID_B: &str = "d8b21abe-aaaa-4bbb-8ccc-ddddeeeeffff";

    fn sid(s: &str) -> SessionId {
        SessionId::parse(s).expect("test id is canonical")
    }

    fn session(kind: AgentKind, id: &str, cwd: &str) -> AgentSession {
        AgentSession { kind, id: sid(id), cwd: cwd.into(), transcript: None }
    }

    #[test]
    fn resume_input_matches_each_agent_and_mode() {
        let cases = [
            (AgentKind::Claude, ResumeMode::Auto, format!("claude --resume {ID_A}\r")),
            (AgentKind::Claude, ResumeMode::Prefill, format!("claude --resume {ID_A}")),
            (AgentKind::Codex, ResumeMode::Auto, format!("codex resume {ID_A}\r")),
            (AgentKind::Codex, ResumeMode::Prefill, format!("codex resume {ID_A}")),
        ];
        for (kind, mode, want) in cases {
            let resume = Resume::new(session(kind, ID_A, "/w"), false, mode);
            assert_eq!(resume.input(), want.into_bytes());
        }
    }

    /// A fake file system: which dirs, files and Claude transcripts exist.
    #[derive(Default)]
    struct FakeFs {
        dirs: Vec<&'static str>,
        files: Vec<&'static str>,
        claude_ids: Vec<&'static str>,
    }

    impl ResumeFs for FakeFs {
        fn is_dir(&self, path: &str) -> bool {
            self.dirs.contains(&path)
        }
        fn is_file(&self, path: &str) -> bool {
            self.files.contains(&path)
        }
        fn claude_transcript_exists(&self, id: &SessionId) -> bool {
            self.claude_ids.contains(&id.as_str())
        }
    }

    fn saved(kind: AgentKind, id: &str, cwd: &str, transcript: Option<&str>) -> SavedAgent {
        SavedAgent {
            kind,
            session: id.into(),
            cwd: cwd.into(),
            transcript: transcript.map(Into::into),
            crashed: false,
        }
    }

    fn fs() -> FakeFs {
        FakeFs {
            dirs: vec!["/work/stdusk", "/work/api"],
            files: vec!["/t/codex.jsonl", "/t/claude.jsonl"],
            claude_ids: vec![ID_A],
        }
    }

    /// Decide each leaf in order, as restore does.
    fn plan(leaves: &[SavedAgent], fs: &FakeFs) -> (Vec<Option<AgentSession>>, Vec<Skip>) {
        let mut plan = RestorePlan::default();
        let out = leaves
            .iter()
            .map(|l| plan.resume(l, ResumeMode::Auto, fs).ok().map(|r| r.session))
            .collect();
        (out, plan.skips)
    }

    #[test]
    fn restore_resumes_mixed_agents_by_transcript_path_or_registry_lookup() {
        let leaves = [
            saved(AgentKind::Claude, ID_A, "/work/stdusk", None), // registry record: home lookup
            saved(AgentKind::Codex, ID_B, "/work/api", Some("/t/codex.jsonl")),
        ];
        let (got, skips) = plan(&leaves, &fs());
        assert!(skips.is_empty(), "{skips:?}");
        assert_eq!(got[0], Some(session(AgentKind::Claude, ID_A, "/work/stdusk")));
        let codex = got[1].as_ref().unwrap();
        assert_eq!(codex.transcript.as_deref(), Some("/t/codex.jsonl"));
    }

    #[test]
    fn restore_trusts_the_recorded_transcript_path_over_the_home_lookup() {
        // The agent ran with its own config dir: the home lookup would miss, the path is right.
        let leaves = [saved(AgentKind::Claude, ID_B, "/work/api", Some("/t/claude.jsonl"))];
        assert!(plan(&leaves, &fs()).0[0].is_some());
        // A recorded path that is gone is a missing transcript, even when the home has the id.
        let leaves = [saved(AgentKind::Claude, ID_A, "/work/api", Some("/t/gone.jsonl"))];
        assert_eq!(plan(&leaves, &fs()).1[0].reason, SkipReason::TranscriptMissing);
    }

    #[test]
    fn restore_lets_only_the_first_leaf_claim_a_session() {
        let leaves = [
            saved(AgentKind::Claude, ID_A, "/work/stdusk", None),
            saved(AgentKind::Claude, ID_A, "/work/api", None),
        ];
        let (got, skips) = plan(&leaves, &fs());
        assert!(got[0].is_some() && got[1].is_none());
        assert_eq!(skips, vec![Skip { label: "api".into(), reason: SkipReason::Duplicate }]);
    }

    #[test]
    fn restore_treats_the_same_id_under_two_agents_as_two_sessions() {
        let leaves = [
            saved(AgentKind::Claude, ID_A, "/work/stdusk", None),
            saved(AgentKind::Codex, ID_A, "/work/api", Some("/t/codex.jsonl")),
        ];
        assert!(plan(&leaves, &fs()).0.iter().all(Option::is_some));
    }

    #[test]
    fn restore_names_each_failure() {
        let leaves = [
            saved(AgentKind::Claude, "not-a-uuid", "/work/stdusk", None),
            saved(AgentKind::Claude, ID_A, "/gone/dir", None),
            saved(AgentKind::Claude, ID_B, "/work/api", None), // no claude transcript for ID_B
            saved(AgentKind::Codex, ID_A, "/work/api", None),  // codex needs a recorded path
        ];
        let (got, skips) = plan(&leaves, &fs());
        assert!(got.iter().all(Option::is_none));
        let reasons: Vec<_> = skips.iter().map(|s| (s.label.as_str(), s.reason)).collect();
        assert_eq!(
            reasons,
            vec![
                ("stdusk", SkipReason::BadId),
                ("dir", SkipReason::DirectoryMissing),
                ("api", SkipReason::TranscriptMissing),
                ("api", SkipReason::TranscriptMissing),
            ]
        );
    }

    #[test]
    fn a_failed_claim_does_not_block_a_later_valid_leaf() {
        // The first leaf has the id but no directory, so it must not claim the session.
        let leaves = [
            saved(AgentKind::Claude, ID_A, "/gone", None),
            saved(AgentKind::Claude, ID_A, "/work/stdusk", None),
        ];
        let (got, _) = plan(&leaves, &fs());
        assert!(got[0].is_none() && got[1].is_some());
    }

    #[test]
    fn a_session_kept_after_a_crash_is_typed_without_enter_whatever_the_mode() {
        let mut leaf = saved(AgentKind::Claude, ID_A, "/work/stdusk", None);
        for setting in [ResumeMode::Auto, ResumeMode::Prefill] {
            let mut plan = RestorePlan::default();
            let normal = plan.resume(&leaf, setting, &fs()).unwrap();
            assert_eq!((normal.mode(), normal.crash_hint), (setting, false));
            assert_eq!(plan.crash_hints, 0);

            leaf.crashed = true;
            let mut plan = RestorePlan::default();
            let hinted = plan.resume(&leaf, setting, &fs()).unwrap();
            assert_eq!(
                (hinted.mode(), hinted.crash_hint),
                (ResumeMode::Prefill, true),
                "{setting:?}"
            );
            assert!(!hinted.input().ends_with(b"\r"), "no carriage return for the hint");
            assert_eq!(plan.crash_hints, 1);
            leaf.crashed = false;
        }
    }

    #[test]
    fn the_quit_sentence_counts_only_sessions_that_reopen_by_themselves() {
        let mut crashed = saved(AgentKind::Codex, ID_B, "/work/api", Some("/t/codex.jsonl"));
        crashed.crashed = true;
        let leaves = [
            saved(AgentKind::Claude, ID_A, "/work/stdusk", None), // counts
            crashed,                                              // typed as a hint only
            saved(AgentKind::Claude, ID_A, "/work/api", None),    // the same session twice
            saved(AgentKind::Claude, ID_B, "/gone", None),        // directory missing
            saved(AgentKind::Codex, ID_A, "/work/api", None),     // no transcript path
        ];
        assert_eq!(auto_resumable(&leaves, ResumeMode::Auto, &fs()), 1);
        // With "type only" nothing reopens by itself.
        assert_eq!(auto_resumable(&leaves, ResumeMode::Prefill, &fs()), 0);
        assert_eq!(auto_resumable(&[], ResumeMode::Auto, &fs()), 0);
    }

    #[test]
    fn a_crashed_session_that_cannot_resume_is_a_skip_not_a_hint() {
        let mut leaf = saved(AgentKind::Claude, ID_A, "/gone", None);
        leaf.crashed = true;
        let mut plan = RestorePlan::default();
        assert!(plan.resume(&leaf, ResumeMode::Auto, &fs()).is_err());
        assert_eq!((plan.skips.len(), plan.crash_hints), (1, 0));
    }

    #[test]
    fn a_failed_leaf_reports_its_reason_to_the_pane() {
        let mut plan = RestorePlan::default();
        let gone = saved(AgentKind::Claude, ID_A, "/gone", None);
        assert_eq!(
            plan.resume(&gone, ResumeMode::Auto, &fs()).err(),
            Some(SkipReason::DirectoryMissing)
        );
        let ok = saved(AgentKind::Claude, ID_A, "/work/stdusk", None);
        assert!(plan.resume(&ok, ResumeMode::Auto, &fs()).is_ok());
        assert_eq!(plan.resume(&ok, ResumeMode::Auto, &fs()).err(), Some(SkipReason::Duplicate));
    }

    #[test]
    fn restore_toast_groups_skips_by_reason_and_names_them_only_when_few() {
        use SkipReason::{BadId, DirectoryMissing, Duplicate, TranscriptMissing};
        let skips = |list: &[(&str, SkipReason)]| -> Vec<Skip> {
            list.iter().map(|&(l, reason)| Skip { label: l.into(), reason }).collect()
        };
        let hint1 = "1 crashed session was not reopened. Its resume command will be typed at the prompt. Press Enter to run it.";
        let hint2 = "2 crashed sessions were not reopened. Their resume commands will be typed at the prompts. Press Enter to run each one.";
        let cases: [(Vec<Skip>, usize, Option<&str>); 11] = [
            (vec![], 0, None),
            (
                skips(&[("stdusk", TranscriptMissing)]),
                0,
                Some("Could not resume 1 session: stdusk (transcript missing)"),
            ),
            // Up to 3 skips are named, whatever their reasons.
            (
                skips(&[
                    ("stdusk", TranscriptMissing),
                    ("api", DirectoryMissing),
                    ("web", Duplicate),
                ]),
                0,
                Some(
                    "Could not resume 3 sessions: stdusk (transcript missing), api (directory missing), web (duplicate of another pane)",
                ),
            ),
            // More than 3 give counts only, with no names.
            (
                skips(&[
                    ("a", TranscriptMissing),
                    ("b", TranscriptMissing),
                    ("c", TranscriptMissing),
                    ("d", DirectoryMissing),
                    ("e", DirectoryMissing),
                ]),
                0,
                Some("Could not resume 5 sessions: 3 transcripts missing, 2 directories missing"),
            ),
            (
                skips(&[
                    ("a", BadId),
                    ("b", Duplicate),
                    ("c", DirectoryMissing),
                    ("d", TranscriptMissing),
                ]),
                0,
                Some(
                    "Could not resume 4 sessions: 1 transcript missing, 1 directory missing, 1 duplicate of another pane, 1 invalid session id",
                ),
            ),
            (
                skips(&[("a", BadId), ("b", BadId), ("c", Duplicate), ("d", Duplicate)]),
                0,
                Some(
                    "Could not resume 4 sessions: 2 duplicates of another pane, 2 invalid session ids",
                ),
            ),
            (vec![], 1, Some(hint1)),
            (vec![], 2, Some(hint2)),
            (
                skips(&[("api", DirectoryMissing)]),
                1,
                Some(
                    "Could not resume 1 session: api (directory missing). 1 crashed session was not reopened. Its resume command will be typed at the prompt. Press Enter to run it.",
                ),
            ),
            (
                skips(&[
                    ("a", TranscriptMissing),
                    ("b", TranscriptMissing),
                    ("c", TranscriptMissing),
                    ("d", DirectoryMissing),
                    ("e", DirectoryMissing),
                ]),
                2,
                Some(
                    "Could not resume 5 sessions: 3 transcripts missing, 2 directories missing. 2 crashed sessions were not reopened. Their resume commands will be typed at the prompts. Press Enter to run each one.",
                ),
            ),
            // The boundary: 3 skips are still named, the 4th turns the list into counts.
            (
                skips(&[("a", DirectoryMissing), ("b", DirectoryMissing), ("c", DirectoryMissing)]),
                0,
                Some(
                    "Could not resume 3 sessions: a (directory missing), b (directory missing), c (directory missing)",
                ),
            ),
        ];
        for (skips, crash_hints, want) in cases {
            assert_eq!(
                restore_toast(&skips, crash_hints).as_deref(),
                want,
                "{skips:?} {crash_hints}"
            );
        }
    }

    #[test]
    fn a_crash_hint_does_not_block_a_healthy_record_of_the_same_session() {
        let healthy = saved(AgentKind::Claude, ID_A, "/work/api", None);
        let mut hint = saved(AgentKind::Claude, ID_A, "/work/stdusk", None);
        hint.crashed = true;
        let mut plan = RestorePlan::default();
        // The hint comes first and is typed. The healthy record after it still resumes.
        let first = plan.resume(&hint, ResumeMode::Auto, &fs()).unwrap();
        assert!(first.crash_hint);
        let second = plan.resume(&healthy, ResumeMode::Auto, &fs()).unwrap();
        assert_eq!((second.crash_hint, second.mode()), (false, ResumeMode::Auto));
        assert!(plan.skips.is_empty(), "{:?}", plan.skips);
        assert_eq!(auto_resumable(&[hint.clone(), healthy.clone()], ResumeMode::Auto, &fs()), 1);

        // The other order: the healthy record claims the session, and the hint is a duplicate.
        let mut plan = RestorePlan::default();
        assert!(plan.resume(&healthy, ResumeMode::Auto, &fs()).is_ok());
        assert_eq!(plan.resume(&hint, ResumeMode::Auto, &fs()).err(), Some(SkipReason::Duplicate));
    }

    #[test]
    fn a_skip_the_caller_decides_reaches_the_toast_like_any_other() {
        let leaf = saved(AgentKind::Claude, ID_A, "/work/stdusk", None);
        let mut plan = RestorePlan::default();
        plan.skipped(&leaf, SkipReason::NotHandedOver);
        assert_eq!(
            plan.into_toast().as_deref(),
            Some("Could not resume 1 session: stdusk (shell not handed over)")
        );
    }

    #[test]
    fn every_skip_reason_reads_as_one_phrase_and_is_counted_in_the_toast() {
        for reason in SkipReason::ALL {
            let (one, many) = reason.forms();
            assert!(!one.is_empty() && !many.is_empty(), "{reason:?}");
            assert_eq!(reason.counted(1), format!("1 {one}"));
            assert_eq!(reason.counted(2), format!("2 {many}"));
        }
        // The list holds every variant once, so a count list never drops a reason.
        let unique: HashSet<_> = SkipReason::ALL.iter().map(|r| r.text()).collect();
        assert_eq!(unique.len(), SkipReason::ALL.len());
    }
}
