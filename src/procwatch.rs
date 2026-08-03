//! Ambient CLI awareness: figure out whether a known AI coding CLI (Claude, Codex, Gemini,
//! Copilot, ...) is running inside a tab, so the tab bar can show a small brand badge - "I've got
//! a claude going in tab 3". We look for a matching process among the *descendants* of the tab's
//! shell. The tree-walk + name matching is pure and unit-tested; `scan` is a thin sysinfo adapter
//! that runs on a ~1 Hz throttle from the UI thread.

use egui::Color32;

/// A recognized AI CLI. The enum order is the badge priority when a tab somehow hosts several.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cli {
    Claude,
    Codex,
    Gemini,
    Copilot,
    Aider,
    Cursor,
    Ollama,
}

/// `(kind, primary binary/dir name, extra aliases)`. A process matches a row when any path segment
/// of its name or argv equals the primary name, starts with `name-`/`name_` (so the `claude-code`
/// package dir counts as claude), or equals an alias.
const TABLE: &[(Cli, &str, &[&str])] = &[
    (Cli::Claude, "claude", &["claude-code"]),
    (Cli::Codex, "codex", &[]),
    (Cli::Gemini, "gemini", &["gemini-cli"]),
    (Cli::Copilot, "copilot", &["gh-copilot", "github-copilot"]),
    (Cli::Aider, "aider", &[]),
    (Cli::Cursor, "cursor", &["cursor-agent"]),
    (Cli::Ollama, "ollama", &[]),
];

impl Cli {
    /// Lowercase brand label shown in the tab badge.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
            Cli::Gemini => "gemini",
            Cli::Copilot => "copilot",
            Cli::Aider => "aider",
            Cli::Cursor => "cursor",
            Cli::Ollama => "ollama",
        }
    }

    /// Brand accent for the badge.
    pub(crate) fn color(self) -> Color32 {
        match self {
            Cli::Claude => Color32::from_rgb(0xD9, 0x77, 0x57), // Anthropic clay
            Cli::Codex => Color32::from_rgb(0x10, 0xA3, 0x7F),  // OpenAI green
            Cli::Gemini => Color32::from_rgb(0x4C, 0x8D, 0xF6), // Google blue
            Cli::Copilot => Color32::from_rgb(0x8A, 0x8A, 0x8A), // GitHub grey
            Cli::Aider => Color32::from_rgb(0xC2, 0x6B, 0xD1),  // aider magenta
            Cli::Cursor => Color32::from_rgb(0xE6, 0xB4, 0x50), // cursor amber
            Cli::Ollama => Color32::from_rgb(0xB8, 0xB8, 0xB8), // ollama light grey
        }
    }
}

/// A minimal process record - the pure `detect` works on these so it needs no sysinfo in tests.
pub(crate) struct Proc {
    pub(crate) pid: u32,
    pub(crate) parent: Option<u32>,
    pub(crate) name: String,
    pub(crate) cmd: Vec<String>,
}

/// The highest-priority known CLI running among the descendants of `root` (the tab's shell), or
/// `None`. `root` itself (the shell) is never classified - only its children and below.
pub(crate) fn detect(procs: &[Proc], root: u32) -> Option<Cli> {
    // Adjacency: parent pid -> indices of its children.
    let mut children: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(par) = p.parent {
            children.entry(par).or_default().push(i);
        }
    }
    let mut found = Vec::new();
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue; // guard against pid-reuse cycles
        }
        let Some(kids) = children.get(&pid) else { continue };
        for &i in kids {
            let p = &procs[i];
            if let Some(cli) = classify(&p.name, &p.cmd) {
                found.push(cli);
            }
            stack.push(p.pid);
        }
    }
    TABLE.iter().map(|t| t.0).find(|c| found.contains(c))
}

/// Classify one process by scanning the path segments of its name and each argv entry.
fn classify(name: &str, cmd: &[String]) -> Option<Cli> {
    let args = std::iter::once(name).chain(cmd.iter().map(String::as_str));
    for arg in args {
        for raw in arg.split(['/', '\\']) {
            let seg = strip_ext(raw).to_ascii_lowercase();
            if seg.is_empty() {
                continue;
            }
            for (cli, primary, aliases) in TABLE {
                if seg == *primary
                    || seg.starts_with(&format!("{primary}-"))
                    || seg.starts_with(&format!("{primary}_"))
                    || aliases.contains(&seg.as_str())
                {
                    return Some(*cli);
                }
            }
        }
    }
    None
}

/// Drop a single trailing extension (`cli.js` -> `cli`, `claude` -> `claude`).
fn strip_ext(seg: &str) -> &str {
    match seg.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => seg,
    }
}

/// The name of a process still running under `root` (the tab's shell), or `None` when the shell
/// is idle. Used by the close-tab confirmation. Prefers a recognized CLI's label; otherwise the
/// deepest descendant (the foreground-most program, e.g. `zsh -> ssh` names `ssh`).
pub(crate) fn busy_child(procs: &[Proc], root: u32) -> Option<String> {
    if let Some(cli) = detect(procs, root) {
        return Some(cli.label().to_owned());
    }
    let mut children: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(par) = p.parent {
            children.entry(par).or_default().push(i);
        }
    }
    let mut deepest: Option<(usize, String)> = None;
    let mut stack = vec![(root, 0usize)];
    let mut seen = std::collections::HashSet::new();
    while let Some((pid, depth)) = stack.pop() {
        if !seen.insert(pid) {
            continue; // guard against pid-reuse cycles
        }
        let Some(kids) = children.get(&pid) else { continue };
        for &i in kids {
            let p = &procs[i];
            if deepest.as_ref().is_none_or(|(d, _)| depth + 1 > *d) {
                deepest = Some((depth + 1, p.name.clone()));
            }
            stack.push((p.pid, depth + 1));
        }
    }
    deepest.map(|(_, name)| name)
}

/// Every process running under `root` (the tab's shell) - its descendants, NOT the shell itself.
/// Used to count + preview what a close/quit will terminate (the shell's process group). A
/// recognized AI CLI is surfaced by its brand label (e.g. `claude`), everything else by its raw
/// process name. Order is discovery order; the caller truncates for display.
pub(crate) fn running_children(procs: &[Proc], root: u32) -> Vec<String> {
    let mut children: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(par) = p.parent {
            children.entry(par).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue; // guard against pid-reuse cycles
        }
        let Some(kids) = children.get(&pid) else { continue };
        for &i in kids {
            let p = &procs[i];
            out.push(
                classify(&p.name, &p.cmd).map_or_else(|| p.name.clone(), |c| c.label().to_owned()),
            );
            stack.push(p.pid);
        }
    }
    out
}

/// Snapshot sysinfo's process table into plain `Proc`s (the pure fns work on these). The ~1 Hz
/// scan loop snapshots ONCE and runs `detect`/`busy_child` per tab on the same table.
pub(crate) fn snapshot(sys: &sysinfo::System) -> Vec<Proc> {
    sys.processes()
        .values()
        .map(|p| Proc {
            pid: p.pid().as_u32(),
            parent: p.parent().map(sysinfo::Pid::as_u32),
            name: p.name().to_string_lossy().into_owned(),
            cmd: p.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect(),
        })
        .collect()
}

/// Where a process is actually sitting, asked of the OS. `None` when the pid is gone, the OS won't
/// say, or the answer isn't a directory any more.
///
/// This is the fallback for a pane whose cwd we never learned: `TabState.cwd` is only ever filled by
/// OSC 7, and macOS zsh emits that from `/etc/zshrc_Apple_Terminal` - sourced ONLY when
/// `TERM_PROGRAM == Apple_Terminal`, which ours never is. So a shell whose own rc files don't emit
/// it stays cwd-less forever, and its tab keeps the bare "zsh" placeholder. Asking the OS costs one
/// targeted refresh (`PROC_PIDVNODEPATHINFO` on macOS), so keep it off per-frame paths - it exists
/// for the handoff, which runs it once per pane during a restart.
pub(crate) fn process_cwd(pid: u32) -> Option<String> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        false,
        sysinfo::ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always),
    );
    let cwd = sys.process(pid)?.cwd()?;
    cwd.is_dir().then(|| cwd.to_string_lossy().into_owned())
}

/// A process as TEARDOWN sees it: its parent link and the session it belongs to - the two facts that
/// decide whether closing a pane is responsible for killing it.
struct Member {
    pid: u32,
    parent: Option<u32>,
    session: Option<u32>,
}

/// Every live pid a pane's teardown must reap, given its shell's pid. TWO boundaries, because
/// neither alone is enough and both were measured on the user's live tree:
///
/// - the pty SESSION (`sid == leader`): an interactive shell puts every foreground job in its OWN
///   process group, so `killpg(shell)` reaches the shell by itself. The session is created per pty
///   (portable-pty `setsid`s the shell) and inherited by every job - `claude` sits here.
/// - the DESCENDANT closure: Claude Code runs each Bash tool call through a `/bin/zsh` in a NEW
///   session (`sid == its own pid`), so a backgrounded `deno task dev` is outside the pty session
///   and invisible to a session sweep. The parent chain is the only thing left that ties it to the
///   tab - which is why teardown must snapshot this BEFORE it signals anything: the first SIGTERM
///   kills the intermediate parent and the link is gone.
///
/// A job whose own parent already exited (a true daemonizing double-fork, e.g. the tmux server) is
/// in neither set and belongs to no tab any more - unreachable by design, not by oversight.
///
/// Costs one process-table refresh plus a `getsid` per process (macOS sysinfo answers `session_id`
/// with a live syscall), so this is teardown-only - never a per-frame path.
pub(crate) fn pty_victims(leader: u32) -> Vec<u32> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        false,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    let members: Vec<Member> = sys
        .processes()
        .values()
        .map(|p| Member {
            pid: p.pid().as_u32(),
            parent: p.parent().map(sysinfo::Pid::as_u32),
            session: p.session_id().map(sysinfo::Pid::as_u32),
        })
        .collect();
    victims_of(&members, leader)
}

/// The pure half of [`pty_victims`]: session members plus the descendant closure of `leader`,
/// deduplicated, never including `leader` itself (its own group kill covers it).
fn victims_of(procs: &[Member], leader: u32) -> Vec<u32> {
    let mut children: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(par) = p.parent {
            children.entry(par).or_default().push(i);
        }
    }
    let mut seen = std::collections::HashSet::from([leader]);
    let mut out = Vec::new();
    // Session members need no parent link: an orphaned job whose shell is already gone still carries
    // the sid, and that is exactly the case teardown exists for.
    for p in procs.iter().filter(|p| p.session == Some(leader)) {
        if seen.insert(p.pid) {
            out.push(p.pid);
        }
    }
    let mut stack = vec![leader];
    let mut walked = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !walked.insert(pid) {
            continue; // guard against pid-reuse cycles
        }
        for &i in children.get(&pid).into_iter().flatten() {
            let p = &procs[i];
            if seen.insert(p.pid) {
                out.push(p.pid);
            }
            stack.push(p.pid);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, name: &str, cmd: &[&str]) -> Proc {
        Proc {
            pid,
            parent: Some(parent),
            name: name.into(),
            cmd: cmd.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn classifies_direct_binary() {
        assert_eq!(classify("claude", &[]), Some(Cli::Claude));
        assert_eq!(classify("gemini", &[]), Some(Cli::Gemini));
        assert_eq!(classify("aider", &[]), Some(Cli::Aider));
        assert_eq!(classify("zsh", &[]), None);
    }

    #[test]
    fn classifies_node_cli_by_install_path() {
        // Claude Code runs as node with the package dir in argv - detect via the path segment.
        let cmd =
            vec!["node".into(), "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js".into()];
        assert_eq!(classify("node", &cmd), Some(Cli::Claude));
    }

    #[test]
    fn alias_and_extension_stripping() {
        assert_eq!(classify("/opt/gh-copilot", &[]), Some(Cli::Copilot));
        assert_eq!(classify("cursor-agent", &[]), Some(Cli::Cursor));
        assert_eq!(classify("gemini.js", &[]), Some(Cli::Gemini));
    }

    #[test]
    fn detects_cli_among_descendants() {
        // shell(100) -> node(200) -> child(300 = claude worker)
        let procs = vec![
            p(200, 100, "node", &["node", "/x/claude-code/cli.js"]),
            p(300, 200, "claude", &["claude"]),
            p(999, 1, "Finder", &["Finder"]), // unrelated
        ];
        assert_eq!(detect(&procs, 100), Some(Cli::Claude));
    }

    #[test]
    fn ignores_the_shell_itself_and_unrelated_trees() {
        // The root shell is named "claude" here (contrived) but must NOT self-match.
        let procs = vec![p(200, 100, "zsh", &["zsh"]), p(300, 1, "gemini", &["gemini"])];
        assert_eq!(detect(&procs, 100), None); // gemini is in a different tree
    }

    #[test]
    fn priority_prefers_earlier_table_entry() {
        let procs = vec![p(200, 100, "aider", &["aider"]), p(201, 100, "claude", &["claude"])];
        assert_eq!(detect(&procs, 100), Some(Cli::Claude)); // Claude outranks Aider
    }

    #[test]
    fn busy_child_names_the_deepest_descendant() {
        // shell(100) -> ssh(200) -> vim(300): the foreground-most program wins.
        let procs = vec![p(200, 100, "ssh", &["ssh"]), p(300, 200, "vim", &["vim"])];
        assert_eq!(busy_child(&procs, 100), Some("vim".into()));
    }

    #[test]
    fn busy_child_prefers_a_recognized_cli_label() {
        let procs = vec![p(200, 100, "node", &["node", "/x/claude-code/cli.js"])];
        assert_eq!(busy_child(&procs, 100), Some("claude".into()));
    }

    #[test]
    fn idle_shell_has_no_busy_child() {
        // No descendants of the shell; unrelated trees don't count.
        let procs = vec![p(300, 1, "Finder", &["Finder"])];
        assert_eq!(busy_child(&procs, 100), None);
    }

    #[test]
    fn running_children_lists_every_descendant_by_friendly_name() {
        // shell(100) -> node(200 = claude) -> worker(300); an unrelated tree is excluded.
        let procs = vec![
            p(200, 100, "node", &["node", "/x/claude-code/cli.js"]),
            p(300, 200, "worker", &["worker"]),
            p(999, 1, "Finder", &["Finder"]),
        ];
        let mut got = running_children(&procs, 100);
        got.sort();
        assert_eq!(got, vec!["claude".to_string(), "worker".to_string()]);
    }

    #[test]
    fn running_children_of_an_idle_shell_is_empty() {
        // A bare shell (no descendants) has nothing to terminate - the no-nag case.
        let procs = vec![p(999, 1, "Finder", &["Finder"])];
        assert!(running_children(&procs, 100).is_empty());
    }

    fn m(pid: u32, parent: u32, session: u32) -> Member {
        Member { pid, parent: Some(parent), session: Some(session) }
    }

    #[test]
    fn victims_span_the_session_and_the_tree_but_never_the_leader() {
        // shell(100) is the pty session leader. 200 = a foreground job in its own GROUP but the
        // shell's session (the `claude` shape). 300 = a job that SETSID'd into its own session while
        // staying 200's child (Claude Code's background bash), 400 its grandchild. 900 is unrelated.
        let procs = vec![
            m(200, 100, 100),
            m(300, 200, 300),
            m(400, 300, 300),
            m(900, 1, 900),
            m(100, 1, 100), // the leader itself
        ];
        let mut got = victims_of(&procs, 100);
        got.sort_unstable();
        assert_eq!(got, vec![200, 300, 400], "the escapee's whole subtree must be reachable");
    }

    #[test]
    fn an_orphaned_session_member_is_still_a_victim() {
        // The shell is already gone, so nothing links the job to it by parentage - the sid is the
        // only remaining evidence, and this is exactly the case teardown exists for.
        let procs = vec![m(200, 1, 100), m(900, 1, 900)];
        assert_eq!(victims_of(&procs, 100), vec![200]);
    }

    #[test]
    fn a_parent_cycle_from_pid_reuse_terminates() {
        // A recycled pid can make the table describe a loop; the walk must not spin on it.
        let procs = vec![m(200, 100, 100), m(100, 200, 100)];
        let mut got = victims_of(&procs, 100);
        got.sort_unstable();
        assert_eq!(got, vec![200]);
    }

    #[test]
    fn an_idle_shell_has_no_victims() {
        assert!(victims_of(&[m(900, 1, 900)], 100).is_empty());
    }

    #[test]
    fn the_live_process_table_puts_us_in_our_own_session() {
        // Grounds `pty_victims` in the real adapter rather than the pure walk: sysinfo must actually
        // answer `session_id` under a `nothing()` refresh (on macOS it is a live getsid), or the
        // session half of the teardown boundary would silently collapse to the tree half.
        let victims = pty_victims(std::process::id());
        assert!(!victims.contains(&std::process::id()), "the leader is never its own victim");
        // Our own session id, asked of the OS: every member of it must be in the sweep.
        #[allow(unsafe_code)] // SAFETY: getsid(0) queries our own session; plain int arg
        let our_sid = unsafe { libc::getsid(0) } as u32;
        let mates = pty_victims(our_sid);
        assert!(
            our_sid == std::process::id() || mates.contains(&std::process::id()),
            "we must show up in our own session's sweep (sid {our_sid})"
        );
    }

    #[test]
    fn a_live_process_cwd_comes_back_from_the_os() {
        // The whole point of the fallback: the OS knows where a process sits even though nothing
        // emitted OSC 7. Asked about OURSELVES, since that is a pid guaranteed to exist, and the
        // answer must be this test's own working directory. A platform where sysinfo can't answer
        // would silently degrade the handoff's tab names, so assert the real value, not just Some.
        let me = std::process::id();
        let want = std::env::current_dir().expect("a test always has a cwd");
        let got = process_cwd(me).expect("the OS must know our own cwd");
        assert_eq!(
            std::fs::canonicalize(&got).ok(),
            std::fs::canonicalize(&want).ok(),
            "process_cwd({me}) = {got}"
        );
        // A pid that cannot exist has no cwd - never a bogus path.
        assert_eq!(process_cwd(u32::MAX), None);
    }
}
