//! Agent session resume: which Claude Code or Codex conversation runs in a pane, and how to
//! reopen it after a relaunch. This module owns the session types, the Claude registry parser, and
//! the file checks restore asks. `agent_codex` finds the Codex sessions. `agent_track` keeps the
//! per-pane record over time. `agent_restore` decides what a relaunch reopens. Other modules use it
//! only through this API.
//!
//! Rules that hold everywhere here:
//! - Only an exact session id resumes anything (`claude --resume <id>`, `codex resume <id>`).
//! - A [`SessionId`] is the only value that reaches a shell line or a file path.
//! - File contents never prove anything alone. A record must parse, and its id must validate.
//! - No hook and no shell wrapper feeds this. The sources are the Claude registry and the Codex
//!   rollout files, both read by the scan thread.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::SavedAgent;

/// Which agent CLI a pane ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    /// The name the user knows it by, for a notice.
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }
}

/// How restore reopens a session. "Off" is not a mode: `tabs::resume_mode` gives `None` for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeMode {
    /// Type the command and press Enter.
    Auto,
    /// Type the command and leave Enter to the user.
    Prefill,
}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// A canonical lowercase 8-4-4-4-12 hex UUID. The only constructor is [`SessionId::parse`], so a
/// value of this type is always safe to type into a shell line and to join into a file name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionId(String);

impl SessionId {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let ok = text.len() == 36
            && text.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => is_lower_hex(b),
            });
        ok.then(|| Self(text.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The creation time, in ms since the epoch, of a UUIDv7 id. Codex thread ids are v7: the first
    /// 48 bits are the time. Any other version has no time, so it gives `None`.
    pub(crate) fn v7_time_ms(&self) -> Option<u64> {
        let text = self.as_str();
        if text.as_bytes()[14] != b'7' {
            return None;
        }
        u64::from_str_radix(&format!("{}{}", &text[..8], &text[9..13]), 16).ok()
    }
}

/// One agent conversation: what runs and where its transcript lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSession {
    pub(crate) kind: AgentKind,
    pub(crate) id: SessionId,
    /// Absolute directory that owns the transcript. Resume runs here.
    pub(crate) cwd: String,
    /// The transcript file. A Codex rollout scan names it. A Claude registry record names none.
    pub(crate) transcript: Option<String>,
}

impl AgentSession {
    /// `crash_hint` marks a record kept only because its agent died of a crash signal.
    pub(crate) fn to_saved(&self, crash_hint: bool) -> SavedAgent {
        SavedAgent {
            crashed: crash_hint,
            kind: self.kind,
            session: self.id.as_str().to_owned(),
            cwd: self.cwd.clone(),
            transcript: self.transcript.clone(),
        }
    }

    /// The live form of a saved record, or `None` when its id is not a canonical UUID.
    pub(crate) fn from_saved(saved: &SavedAgent) -> Option<Self> {
        Some(Self {
            kind: saved.kind,
            id: SessionId::parse(&saved.session)?,
            cwd: saved.cwd.clone(),
            transcript: saved.transcript.clone(),
        })
    }
}

/// A path string that is safe to keep: absolute, non-empty, no control characters.
pub(crate) fn plain_abs_path(text: &str) -> Option<String> {
    (Path::new(text).is_absolute() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

// --- Capture: parsers ---------------------------------------------------------------------------

/// Claude writes `$CLAUDE_CONFIG_DIR/sessions/<pid>.json` (else `~/.claude/sessions`) for each live
/// interactive session, and it rewrites `sessionId` at once on `/clear` and `/resume`. The file
/// counts only for the process it names, and only when its `startedAt` is not older than the
/// process start. This rejects a stale file (a `kill -9` leaves one) and a reused pid.
///
/// A file of this process was written after it started, so its second is at least the floored
/// start second, and a stale file of an older process with the same pid is older.
pub(crate) fn parse_claude_registry(
    json: &str,
    pid: u32,
    proc_start_secs: u64,
) -> Option<AgentSession> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v.get("pid")?.as_u64()? != u64::from(pid) {
        return None;
    }
    if v.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k != "interactive") {
        return None;
    }
    let started_secs = v.get("startedAt")?.as_u64()? / 1000;
    if started_secs < proc_start_secs {
        return None;
    }
    Some(AgentSession {
        kind: AgentKind::Claude,
        id: SessionId::parse(v.get("sessionId")?.as_str()?)?,
        cwd: plain_abs_path(v.get("cwd")?.as_str()?)?,
        transcript: None,
    })
}

/// One process that may hold the pane's Claude session: the nearest agent, or its direct child of
/// the same kind. A launcher can run the native `claude` as a child, and the registry names it.
pub(crate) struct Candidate<'a> {
    pub(crate) pid: u32,
    pub(crate) start_secs: u64,
    /// The raw `~/.claude/sessions/<pid>.json`, if the scan found one.
    pub(crate) registry_json: Option<&'a str>,
}

/// The Claude session of a pane: the registry file of the first process in `family` that has a
/// valid one. The registry is the only source, so it names the session.
pub(crate) fn claude_session(family: &[Candidate]) -> Option<AgentSession> {
    family.iter().find_map(|c| parse_claude_registry(c.registry_json?, c.pid, c.start_secs))
}

// --- Capture: what a scan saw ----------------------------------------------------------------

/// What one scan saw under a pane's shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    NoAgent,
    /// The nearest agent process: its kind, its pid, and its session when its source named one
    /// (the Claude registry, or the Codex rollout match).
    Agent {
        kind: AgentKind,
        pid: u32,
        session: Option<AgentSession>,
        /// The session id in the process's own command line (`--resume <id>`, `resume <id>`).
        argv_session: Option<SessionId>,
    },
}

/// The first session id in a command line. `claude --resume <id>`, `codex resume <id>` and
/// `node codex.js resume <id>` all put it there. The form `--resume=<id>` counts too.
pub(crate) fn argv_session(cmd: &[String]) -> Option<SessionId> {
    cmd.iter().find_map(|arg| SessionId::parse(arg.rsplit('=').next().unwrap_or(arg)))
}

// --- Restore ------------------------------------------------------------------------------------

/// The file-system questions the restore plan asks. A trait so tests use a fake.
pub(crate) trait ResumeFs {
    fn is_dir(&self, path: &str) -> bool;
    fn is_file(&self, path: &str) -> bool;
    /// Claude's transcript for an id, found in `projects/*/`. Only for a record that names no path.
    fn claude_transcript_exists(&self, id: &SessionId) -> bool;
}

/// The real file system. `claude_home` is `$CLAUDE_CONFIG_DIR` or `~/.claude`, read from
/// stdusk's own environment. A field, so tests can point it at a scratch directory.
pub(crate) struct SystemFs {
    pub(crate) claude_home: PathBuf,
}

impl SystemFs {
    pub(crate) fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let claude_home = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".claude"), PathBuf::from);
        Self { claude_home }
    }
}

impl ResumeFs for SystemFs {
    fn is_dir(&self, path: &str) -> bool {
        Path::new(path).is_dir()
    }

    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    /// `projects/<encoded cwd>/<id>.jsonl`. The id is a UUID, so no path encoding is needed.
    fn claude_transcript_exists(&self, id: &SessionId) -> bool {
        let file = format!("{}.jsonl", id.as_str());
        std::fs::read_dir(self.claude_home.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|project| project.path().join(&file).is_file())
    }
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
    fn session_id_accepts_only_the_canonical_lowercase_uuid() {
        assert!(SessionId::parse(ID_A).is_some());
        assert!(SessionId::parse("01a0f0b0-21c4-7190-b46f-67a11c9255dd").is_some()); // codex v7
        let bad = [
            "",
            "0c2cbc96",
            "0C2CBC96-1111-4222-8333-444455556666", // uppercase is not canonical
            "0c2cbc96-1111-4222-8333-44445555666",  // one short
            "0c2cbc96-1111-4222-8333-4444555566667", // one long
            "0c2cbc96-1111-4222-8333-44445555666g", // non-hex
            "0c2cbc96_1111_4222_8333_444455556666", // wrong separators
            "0c2cbc96-1111-4222-8333-444455556666;rm -rf ~",
            "0c2cbc96-1111-4222-8333-444455556666\n",
            " 0c2cbc96-1111-4222-8333-444455556666",
            "0c2cbc96-1111-4222-8333-4444555566$(x)",
            "../../etc/passwd-1111-4222-8333-444455556666",
        ];
        for text in bad {
            assert!(SessionId::parse(text).is_none(), "input {text:?}");
        }
    }

    #[test]
    fn a_v7_id_carries_its_creation_time_and_other_versions_carry_none() {
        let cases = [
            // Real Codex 0.159.2 thread ids from the probe, with the time in ms they encode.
            ("01a0f1b5-cfca-7e30-a7aa-f5af4aa03f35", Some(1_790_761_619_402)),
            ("01a0f1b5-e1cc-70b3-9980-45a40737d002", Some(1_790_761_624_012)),
            (ID_A, None), // version 4
            ("00000000-0000-7000-8000-000000000000", Some(0)),
            ("ffffffff-ffff-7fff-bfff-ffffffffffff", Some(0xffff_ffff_ffff)),
        ];
        for (id, want) in cases {
            assert_eq!(sid(id).v7_time_ms(), want, "id {id}");
        }
    }

    const REGISTRY: &str = r#"{"pid":17858,"sessionId":"0c2cbc96-1111-4222-8333-444455556666",
        "cwd":"/Users/x/Repos/stdusk","startedAt":1790742451691,"kind":"interactive",
        "entrypoint":"cli","version":"2.1.285","status":"busy"}"#;

    #[test]
    fn claude_registry_maps_a_live_process_to_its_session() {
        let got = parse_claude_registry(REGISTRY, 17858, 1_790_742_449).unwrap();
        assert_eq!(got, session(AgentKind::Claude, ID_A, "/Users/x/Repos/stdusk"));
    }

    #[test]
    fn claude_registry_accepts_or_rejects_by_what_belongs_to_the_process() {
        let swap = |from: &str, to: &str| REGISTRY.replace(from, to);
        let cases: Vec<(String, u32, u64, bool, &str)> = vec![
            (REGISTRY.into(), 17858, 1_790_742_449, true, "the live process"),
            (swap(r#""kind":"interactive","#, ""), 17858, 1_790_742_449, true, "a missing kind"),
            (REGISTRY.into(), 999, 1_790_742_449, false, "wrong pid"),
            (REGISTRY.into(), 17858, 1_790_742_451, true, "same second as the process start"),
            (REGISTRY.into(), 17858, 1_790_742_449 - 200, true, "file written later"),
            (REGISTRY.into(), 17858, 1_790_742_452, false, "stale file, 1 s older"),
            (REGISTRY.into(), 17858, 1_790_742_449 + 200, false, "stale file, older process"),
            (swap("interactive", "print"), 17858, 1_790_742_449, false, "non-interactive kind"),
            (swap("0c2cbc96", "0C2CBC96"), 17858, 1_790_742_449, false, "non-canonical id"),
            (swap("/Users/x/Repos/stdusk", "relative/dir"), 17858, 1_790_742_449, false, "cwd"),
            (swap(r#""pid":17858,"#, ""), 17858, 1_790_742_449, false, "missing pid"),
            (swap(r#""startedAt":1790742451691,"#, ""), 17858, 1_790_742_449, false, "no time"),
            ("not json".into(), 17858, 1_790_742_449, false, "garbage"),
            ("[]".into(), 17858, 1_790_742_449, false, "wrong shape"),
        ];
        for (json, pid, start, want, why) in cases {
            assert_eq!(parse_claude_registry(&json, pid, start).is_some(), want, "{why}");
        }
    }

    fn family(pid: u32, start: u64, registry: Option<&str>) -> Vec<Candidate<'_>> {
        vec![Candidate { pid, start_secs: start, registry_json: registry }]
    }

    #[test]
    fn the_registry_names_the_claude_session_of_the_process_it_belongs_to() {
        let (pid, start) = (17858, 1_790_742_449);
        let want = Some(session(AgentKind::Claude, ID_A, "/Users/x/Repos/stdusk"));
        let cases = [
            ("the live process", family(pid, start, Some(REGISTRY)), want.clone()),
            ("another pid", family(pid + 1, start, Some(REGISTRY)), None),
            ("a stale file", family(pid, start + 500, Some(REGISTRY)), None),
            ("no file", family(pid, start, None), None),
            ("nothing", vec![], None),
        ];
        for (why, family, want) in cases {
            assert_eq!(claude_session(&family), want, "{why}");
        }
        // A launcher and its child: the child owns the file.
        let two = vec![
            Candidate { pid: 1, start_secs: start, registry_json: None },
            Candidate { pid, start_secs: start, registry_json: Some(REGISTRY) },
        ];
        assert_eq!(claude_session(&two), want);
    }

    #[test]
    fn the_session_id_in_a_command_line_is_found_wherever_the_agent_puts_it() {
        let argv = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let cases = [
            (format!("claude --resume {ID_A}"), Some(ID_A)),
            (format!("claude --resume={ID_A}"), Some(ID_A)),
            (format!("codex resume {ID_A}"), Some(ID_A)),
            (format!("node /x/bin/codex.js resume {ID_A}"), Some(ID_A)),
            ("claude".to_owned(), None),
            ("codex resume --last".to_owned(), None),
            (format!("claude --resume {}", ID_A.to_uppercase()), None),
            (format!("claude --resume {ID_A};rm"), None),
        ];
        for (line, want) in cases {
            let got = argv_session(&argv(&line));
            assert_eq!(got.as_ref().map(SessionId::as_str), want, "{line}");
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("stdusk-agents-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn system_fs_finds_a_claude_transcript_in_any_project_dir() {
        let base = scratch("claude-fs");
        let proj = base.join("projects/-Users-x-Repos-stdusk");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{ID_A}.jsonl")), "{}").unwrap();
        let fs = SystemFs { claude_home: base.clone() };
        assert!(fs.claude_transcript_exists(&sid(ID_A)));
        assert!(!fs.claude_transcript_exists(&sid(ID_B)));
        assert!(fs.is_dir(base.to_str().unwrap()));
        assert!(!fs.is_dir(base.join("missing").to_str().unwrap()));
        assert!(fs.is_file(proj.join(format!("{ID_A}.jsonl")).to_str().unwrap()));
        assert!(!fs.is_file(proj.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
