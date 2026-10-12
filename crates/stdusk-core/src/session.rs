//! Persisted shape of a pane's agent session, shared by the agent tracker and the app's session file.
use serde::{Deserialize, Serialize};

/// The Claude Code or Codex session a pane ran. Holds a plain string id: it is validated when the
/// record is used (`agents::SessionId::parse`), never trusted from the file.
///
/// Downgrade note: a session file with an `agent` key fails to load in stdusk 1.8.0 and older
/// (toml rejects unknown keys inside an enum variant), and a live update handoff to such a build
/// aborts. Newer builds load older files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAgent {
    pub kind: crate::agents::AgentKind,
    pub session: String,
    pub cwd: String,
    /// The transcript file (a Codex rollout). Absent for a Claude registry record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// The record was kept only because the agent died of a crash signal (which statuses count is
    /// `agent_track::CRASH_SIGNALS`). This is a crash hint: restore types its resume command
    /// without Enter. Absent means a normal record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub crashed: bool,
}
