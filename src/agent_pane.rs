//! The agent state of one pane, and the queue for its resume command.
//!
//! Who arms what, from a restore to a session on disk:
//! 1. `tabs` decides per pane. It fills an [`AgentSetup`]: the resume command from the restore
//!    plan.
//! 2. `terminal` builds the [`AgentPane`] from it. `shell::configure` reports back whether the
//!    shell marks its prompts, which decides if the fallback timer exists. The shell is any shell:
//!    nothing is injected for the agents.
//! 3. The scan thread of `procwatch` finds the agent under the pane's shell and names its session
//!    (the Claude registry, or the Codex rollout match of `agent_codex`). `main` feeds every scan
//!    to [`AgentPane::scan`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent_track::{Status, Track};
use crate::agents::{AgentSession, Seen};
use crate::session::SavedAgent;

/// The default wait for a prompt mark from a shell without integration (fish, or integration off).
pub(crate) const PROMPT_FALLBACK: Duration = Duration::from_secs(5);

/// What `tabs` decides for one agent-aware pane.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentSetup {
    /// Input typed at the first prompt (a resume command).
    pub(crate) pending_input: Option<Vec<u8>>,
    /// How long to wait for a prompt mark before the fallback may type the input. Only a shell
    /// that gets no integration waits on this. A shell with integration marks its prompt.
    pub(crate) fallback_delay: Duration,
}

/// Input that waits for the first prompt (a resume command). Whoever takes it types it, so it is
/// typed at most once. It leaves the queue in one of three ways:
/// - The reader thread types it at the first OSC 133 prompt mark. A shell with our integration
///   always marks its prompts, so this is the normal path.
/// - The fallback types it once the wait is over, for a shell that marks no prompt. It does so
///   only if the shell itself holds the tty, and drops it otherwise.
/// - Any input from the user drops it (`PtyTerm::send`), so it never lands in a line being typed.
#[derive(Clone, Default)]
pub(crate) struct PendingInput(Arc<Mutex<Option<Vec<u8>>>>);

impl PendingInput {
    fn new(bytes: Option<Vec<u8>>) -> Self {
        Self(Arc::new(Mutex::new(bytes)))
    }

    /// Take the input out of the queue, to type it or to drop it.
    pub(crate) fn take(&self) -> Option<Vec<u8>> {
        // grab-copy-drop: the guard ends with this statement, before the caller writes anything.
        self.0.lock().unwrap().take()
    }
}

/// What the fallback did with the queued input.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// The shell holds the tty: type these bytes.
    Type(Vec<u8>),
    /// Something else holds the tty: the input is gone and was not typed.
    Drop,
}

/// The agent state of one pane whose agents are tracked.
pub(crate) struct AgentPane {
    /// The agent session this pane runs, as the ~1 Hz scan last saw it.
    track: Track,
    pending: PendingInput,
    /// When the fallback may type `pending`. `None` = the shell marks its prompts, or no fallback.
    fallback_at: Option<Instant>,
}

impl AgentPane {
    /// A pane that spawns its shell.
    pub(crate) fn new(setup: AgentSetup) -> Self {
        Self {
            track: Track::default(),
            pending: PendingInput::new(setup.pending_input),
            fallback_at: Some(Instant::now() + setup.fallback_delay),
        }
    }

    /// A pane that adopts a live shell. It resumes nothing, because the agent already runs.
    pub(crate) fn adopted() -> Self {
        Self { track: Track::default(), pending: PendingInput::default(), fallback_at: None }
    }

    /// Say whether the shell marks its prompts. If it does, it releases the input itself and the
    /// fallback is not needed.
    pub(crate) fn shell_marks_prompts(mut self, marks: bool) -> Self {
        if marks {
            self.fallback_at = None;
        }
        self
    }

    /// The queue that the reader thread drains at the first prompt mark.
    pub(crate) fn pending(&self) -> PendingInput {
        self.pending.clone()
    }

    /// The user sent input first: drop the queued command, so it never lands in a line being typed.
    pub(crate) fn discard_pending(&self) {
        self.pending.take();
    }

    /// The record to save.
    pub(crate) fn saved(&self) -> Option<SavedAgent> {
        self.track.saved()
    }

    /// Advance the record by one scan, taken at `taken`, and the status the shell reported before it.
    pub(crate) fn scan(&mut self, status: Option<Status>, seen: Seen, taken: Instant) {
        self.track = std::mem::take(&mut self.track).scan(status, seen, taken);
    }

    /// Apply a status without a scan (every frame, and the final snapshot before quit).
    pub(crate) fn apply_status(&mut self, status: Option<Status>) {
        self.track = std::mem::take(&mut self.track).status(status);
    }

    /// Hold a session that stdusk reopened until an agent binds to it. `crash_hint` marks a
    /// session kept after a crash.
    pub(crate) fn restore(&mut self, session: AgentSession, crash_hint: bool) {
        self.track = Track::restored(session, crash_hint);
    }

    /// Hold the session of a live handoff, whose agent keeps running. `crash_hint` says the record
    /// was kept after a crash, so no agent runs.
    pub(crate) fn adopt(&mut self, session: AgentSession, crash_hint: bool) {
        self.track = Track::adopted(session, crash_hint);
    }

    /// The fallback for a shell that marks no prompt. Once the wait is over, it gives back the
    /// queued input if `at_prompt` says the shell itself holds the tty. Anything else on the
    /// tty (an editor, `ssh`, a blocked rc script) would receive the command, so the input is
    /// dropped instead. `None`: the wait is not over, or no input was queued.
    pub(crate) fn fallback_input(&mut self, at_prompt: impl FnOnce() -> bool) -> Option<Fallback> {
        if Instant::now() < self.fallback_at? {
            return None;
        }
        self.fallback_at = None;
        let bytes = self.pending.take()?;
        Some(if at_prompt() { Fallback::Type(bytes) } else { Fallback::Drop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(input: Option<&[u8]>, delay: Duration) -> AgentPane {
        AgentPane::new(AgentSetup {
            pending_input: input.map(<[u8]>::to_vec),
            fallback_delay: delay,
        })
    }

    #[test]
    fn an_adopted_pane_resumes_nothing() {
        assert!(AgentPane::adopted().pending().take().is_none(), "a handoff resumes nothing");
        assert!(AgentPane::adopted().saved().is_none(), "and holds no record until told");
    }

    #[test]
    fn the_input_leaves_the_queue_once_however_many_hands_reach_for_it() {
        let mut p = pane(Some(b"cmd"), Duration::ZERO);
        let reader = p.pending();
        assert_eq!(reader.take().as_deref(), Some(&b"cmd"[..]));
        assert_eq!(p.pending().take(), None);
        assert_eq!(p.fallback_input(|| true), None, "the reader typed it already");
    }

    #[test]
    fn the_fallback_waits_for_its_deadline_then_types_once_if_the_shell_holds_the_tty() {
        let mut early = pane(Some(b"cmd"), Duration::from_secs(3600));
        assert_eq!(early.fallback_input(|| true), None);
        assert!(early.pending().take().is_some(), "an early call leaves the input queued");

        let mut due = pane(Some(b"cmd"), Duration::ZERO);
        assert_eq!(due.fallback_input(|| true), Some(Fallback::Type(b"cmd".to_vec())));
        assert_eq!(due.fallback_input(|| true), None, "typed once");

        // Something else holds the tty: the input is dropped, not kept for later, and the caller
        // is told, so it can say the command was not typed.
        let mut busy = pane(Some(b"cmd"), Duration::ZERO);
        assert_eq!(busy.fallback_input(|| false), Some(Fallback::Drop));
        assert_eq!(busy.pending().take(), None);
        assert_eq!(busy.fallback_input(|| false), None, "told once");
    }

    #[test]
    fn a_shell_that_marks_prompts_needs_no_fallback() {
        let mut p = pane(Some(b"cmd"), Duration::ZERO).shell_marks_prompts(true);
        assert_eq!(p.fallback_input(|| true), None);
        assert!(p.pending().take().is_some(), "the reader thread still has it");
        let mut p = pane(Some(b"cmd"), Duration::ZERO).shell_marks_prompts(false);
        assert!(p.fallback_input(|| true).is_some());
        // With nothing queued there is nothing to type or to drop.
        let mut empty = pane(None, Duration::ZERO);
        assert_eq!(empty.fallback_input(|| false), None);
    }
}
