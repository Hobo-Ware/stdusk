//! Ambient CLI awareness: figure out whether a known AI coding CLI (Claude, Codex, Gemini,
//! Copilot, ...) is running inside a tab, so the tab bar can show a small brand badge - "I've got
//! a claude going in tab 3". We look for a matching process among the *descendants* of the tab's
//! shell. The tree-walk + name matching is pure and unit-tested; `ProcScanner` refreshes the
//! process table ~1 Hz on its own thread and the UI runs the walks on the latest table.

use egui::Color32;

use crate::agent_codex::{self, CodexProc};
use crate::agent_codex_scan::CodexScanner;
use crate::agents::{self, AgentKind, AgentSession, Candidate, Seen};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Process start, seconds since the epoch. A session record older than this is stale.
    pub(crate) start_time: u64,
    /// Where the process runs. Only the Codex processes get one (see [`codex_sessions`]), because
    /// the read costs a system call each.
    pub(crate) cwd: Option<String>,
    /// The user of the process. Only the Codex processes get one, like `cwd`.
    pub(crate) uid: Option<sysinfo::Uid>,
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
            name: proc_name(p),
            cmd: p.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect(),
            start_time: p.start_time(),
            cwd: None,
            uid: None,
        })
        .collect()
}

/// The name of a process. macOS sysinfo sets the name once and keeps it across `exec`, so a scan
/// that caught a fork before its exec would call a `claude` process "zsh" for its whole life. The
/// scan thread re-reads the exe path each time (see [`scan_refresh_kind`]), and its file name is
/// the true name there. Linux re-reads the name each time, and its exe path has symlinks resolved,
/// so its name is right as it is.
fn proc_name(p: &sysinfo::Process) -> String {
    let exe_name =
        p.exe().and_then(std::path::Path::file_name).filter(|_| cfg!(target_os = "macos"));
    exe_name.unwrap_or_else(|| p.name()).to_string_lossy().into_owned()
}

/// What the scan thread reads of each process. The exe path and the command line are read on every
/// scan, because macOS sysinfo keeps the first value it saw (see [`proc_name`]). The process
/// arguments are read on every scan of a process anyway, so this costs one allocation each.
fn scan_refresh_kind() -> sysinfo::ProcessRefreshKind {
    sysinfo::ProcessRefreshKind::nothing()
        .with_cmd(sysinfo::UpdateKind::Always)
        .with_exe(sysinfo::UpdateKind::Always)
}

// --- Agent sessions ---------------------------------------------------------------------------

/// Strict identity of a Claude Code or Codex process. Stricter than [`classify`] (which serves the
/// badges): an editor with `claude.md` in its argv must not read as an agent, and neither does a
/// `--version` or `--help` query. A native binary is
/// named for the agent. A node/bun/deno install runs a script, and that script must be either in
/// the agent's package (`@openai/codex`, `@anthropic-ai/claude-code`) or a launcher named for the
/// agent in a `bin` directory. A repo or folder that is merely named `codex` does not count.
pub(crate) fn agent_kind(name: &str, cmd: &[String]) -> Option<AgentKind> {
    // A version or help query runs no session, and a scan that catches one under the pane's
    // shell must not read it as the pane's agent.
    if cmd.iter().skip(1).any(|arg| matches!(arg.as_str(), "--version" | "-V" | "--help" | "-h")) {
        return None;
    }
    let stem = |s: &str| strip_ext(s.rsplit(['/', '\\']).next().unwrap_or(s)).to_ascii_lowercase();
    let by_name = |s: &str| match s {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        _ => None,
    };
    if let Some(kind) = by_name(&stem(name)) {
        return Some(kind);
    }
    if !matches!(stem(name).as_str(), "node" | "bun" | "deno") {
        return None;
    }
    let script = cmd.get(1)?.to_ascii_lowercase();
    let dirs: Vec<&str> = script.split(['/', '\\']).collect();
    let (_file, parents) = dirs.split_last()?;
    let in_package = |scope: &str, package: &str| {
        parents.windows(2).any(|pair| pair[0] == scope && pair[1] == package)
    };
    if in_package("@openai", "codex") {
        return Some(AgentKind::Codex);
    }
    if in_package("@anthropic-ai", "claude-code") {
        return Some(AgentKind::Claude);
    }
    let in_bin_dir = parents.last().is_some_and(|d| matches!(*d, "bin" | ".bin"));
    by_name(&stem(&script)).filter(|_| in_bin_dir)
}

/// The agent process closest to `root` (a pane's shell): the smallest depth wins. An agent that a
/// tool of the outer agent starts is deeper, so it never counts. At one depth the agent in the
/// tty's foreground group (`foreground`, see `PtyTerm::foreground_pgid`) wins, so a stopped job
/// does not hide the agent the user works in. Otherwise the lowest pid wins.
pub(crate) fn nearest_agent(
    procs: &[Proc],
    root: u32,
    foreground: Option<u32>,
) -> Option<(AgentKind, &Proc)> {
    let mut children: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(par) = p.parent {
            children.entry(par).or_default().push(i);
        }
    }
    let mut seen = std::collections::HashSet::from([root]);
    let mut level = vec![root];
    while !level.is_empty() {
        let mut next: Vec<&Proc> = level
            .iter()
            .flat_map(|pid| children.get(pid).into_iter().flatten())
            .map(|&i| &procs[i])
            .filter(|p| seen.insert(p.pid)) // guards against pid-reuse cycles
            .collect();
        next.sort_by_key(|p| (Some(p.pid) != foreground, p.pid));
        if let Some(found) = next.iter().find_map(|p| agent_kind(&p.name, &p.cmd).map(|k| (k, *p)))
        {
            return Some(found);
        }
        level = next.iter().map(|p| p.pid).collect();
    }
    None
}

/// What the scan thread found for the agent sessions, so the UI thread does no file I/O.
#[derive(Default)]
pub(crate) struct AgentFiles {
    /// Claude registry files by pid, raw (the parser checks them against the process).
    pub(crate) registry: std::collections::HashMap<u32, String>,
    /// The Codex session of each Codex process, by pid (see [`agent_codex`]).
    pub(crate) codex: std::collections::HashMap<u32, AgentSession>,
}

/// One scan: the process table and the agent files read beside it.
pub(crate) struct Scan {
    pub(crate) procs: Vec<Proc>,
    pub(crate) files: AgentFiles,
    /// When the thread began this scan, before it read the process table. An exit status that
    /// arrived later than this is newer than everything the scan shows.
    pub(crate) taken: std::time::Instant,
}

/// The nearest agent `head` and its direct children of the same kind. Never a deeper descendant:
/// an agent that a tool of the outer agent starts (`claude -> zsh -> claude -p`) is not the pane's.
fn agent_family<'a>(procs: &'a [Proc], head: &'a Proc, kind: AgentKind) -> Vec<&'a Proc> {
    let is_child =
        |p: &&Proc| p.parent == Some(head.pid) && agent_kind(&p.name, &p.cmd) == Some(kind);
    std::iter::once(head).chain(procs.iter().filter(is_child)).collect()
}

/// What a scan saw under one pane's shell. The reported pid is the nearest agent's, even when its
/// child owns the Claude registry file. Claude's session is the registry's. Codex's is the match of
/// its rollout scan, and it is `None` when that match is in doubt.
pub(crate) fn pane_seen(scan: &Scan, root: u32, foreground: Option<u32>) -> Seen {
    let Some((kind, head)) = nearest_agent(&scan.procs, root, foreground) else {
        return Seen::NoAgent;
    };
    let session = match kind {
        AgentKind::Claude => {
            let family: Vec<Candidate> = agent_family(&scan.procs, head, kind)
                .into_iter()
                .map(|p| Candidate {
                    pid: p.pid,
                    start_secs: p.start_time,
                    registry_json: scan.files.registry.get(&p.pid).map(String::as_str),
                })
                .collect();
            agents::claude_session(&family)
        }
        AgentKind::Codex => scan.files.codex.get(&head.pid).cloned(),
    };
    Seen::Agent { kind, pid: head.pid, session, argv_session: agents::argv_session(&head.cmd) }
}

/// Words that make a `codex` process a service or a utility and not a session: the shared
/// app-server daemon (`app-server --listen ...`), the MCP server, and the login commands.
fn is_codex_service(cmd: &[String]) -> bool {
    const WORDS: [&str; 6] = ["app-server", "mcp-server", "mcp", "login", "logout", "completion"];
    cmd.iter().skip(1).any(|arg| WORDS.contains(&arg.as_str()))
}

/// The live Codex processes that may hold a thread. That is every Codex process on the machine and
/// not only the ones under a pane, because a TUI in another terminal makes threads in the same
/// daemon. Not the daemon, and not the native child of a launcher (the launcher stands for it).
pub(crate) fn codex_tuis(procs: &[Proc]) -> Vec<&Proc> {
    let by_pid: std::collections::HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let is_codex = |p: &Proc| agent_kind(&p.name, &p.cmd) == Some(AgentKind::Codex);
    procs
        .iter()
        .filter(|p| is_codex(p) && !is_codex_service(&p.cmd))
        .filter(|p| !p.parent.and_then(|pp| by_pid.get(&pp)).is_some_and(|pp| is_codex(pp)))
        .collect()
}

/// The Codex process as the rollout match sees it. `None` for a `codex exec` run, which is never
/// a TUI and so never a rival (see [`agent_codex::is_exec`]). `None` for a process of another user: its
/// threads live in that user's `CODEX_HOME`, which this scanner never lists, and a process with no
/// readable cwd would block every `/new` on the machine. A process whose user is unknown stays a
/// rival, which is the safe side.
fn codex_proc(p: &Proc, me: Option<&sysinfo::Uid>) -> Option<CodexProc> {
    let other_user = p.uid.as_ref().zip(me).is_some_and(|(theirs, mine)| theirs != mine);
    (!other_user && !agent_codex::is_exec(&p.cmd)).then(|| CodexProc {
        pid: p.pid,
        start_secs: p.start_time,
        cwd: agent_codex::working_dir(&p.cmd, p.cwd.as_deref()).and_then(|path| {
            let real = std::fs::canonicalize(&path).unwrap_or(path);
            real.to_str().and_then(agents::plain_abs_path)
        }),
        resumes: agent_codex::resume_id(&p.cmd),
    })
}

/// The Codex session of each live Codex process, from the rollout match (see [`agent_codex`]).
/// It fills the cwd and user of the Codex processes first, and only theirs.
fn codex_sessions(
    sys: &mut sysinfo::System,
    procs: &mut [Proc],
    scanner: &mut CodexScanner,
) -> std::collections::HashMap<u32, AgentSession> {
    let pids: std::collections::HashSet<u32> = codex_tuis(procs).iter().map(|p| p.pid).collect();
    let me = sysinfo::get_current_pid().ok();
    if !pids.is_empty() {
        // The user is set once per process. Our own process is in the list to get ours.
        let ids: Vec<sysinfo::Pid> =
            pids.iter().map(|p| sysinfo::Pid::from_u32(*p)).chain(me).collect();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&ids),
            false,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cwd(sysinfo::UpdateKind::Always)
                .with_user(sysinfo::UpdateKind::OnlyIfNotSet),
        );
        for p in procs.iter_mut().filter(|p| pids.contains(&p.pid)) {
            let live = sys.process(sysinfo::Pid::from_u32(p.pid));
            p.cwd = live.and_then(sysinfo::Process::cwd).map(|c| c.to_string_lossy().into_owned());
            p.uid = live.and_then(sysinfo::Process::user_id).cloned();
        }
    }
    let my_uid = me.and_then(|pid| sys.process(pid)).and_then(sysinfo::Process::user_id);
    let live: Vec<CodexProc> = procs
        .iter()
        .filter(|p| pids.contains(&p.pid))
        .filter_map(|p| codex_proc(p, my_uid))
        .collect();
    scanner.sessions(&live)
}

/// Largest registry file read. Real ones are under 1 KB.
const MAX_AGENT_FILE: u64 = 64 * 1024;

fn read_small(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path).ok()?.take(MAX_AGENT_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Read the Claude registry file of each Claude process in `procs`. Runs on the scan thread. An
/// unreadable or invalid file is just absent.
pub(crate) fn read_agent_files(procs: &[Proc], claude_home: &std::path::Path) -> AgentFiles {
    let mut files = AgentFiles::default();
    for p in procs.iter().filter(|p| agent_kind(&p.name, &p.cmd) == Some(AgentKind::Claude)) {
        if let Some(text) = read_small(&claude_home.join(format!("sessions/{}.json", p.pid))) {
            files.registry.insert(p.pid, text);
        }
    }
    files
}

const SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// The ~1 Hz process-table refresh, on its own thread: a full refresh takes 7-19 ms with ~850
/// processes, long enough to drop a frame or two when the UI thread paid for it.
pub(crate) struct ProcScanner {
    latest: Arc<Mutex<Option<Scan>>>,
    enabled: Arc<AtomicBool>,
}

/// One scan, taken now: refresh `sys` and read the agent files beside it. The scan thread runs it
/// every second. A quit or a handoff runs it once on the UI thread, so its final snapshot judges
/// the agents as they are and not as they were up to a second ago. `codex` is the rollout scanner
/// of the scan thread. The UI thread passes `None` and reuses the last match instead, which keeps
/// the scanner's memory in one place and the rollout reads off the UI thread.
pub(crate) fn scan_now(
    sys: &mut sysinfo::System,
    claude_home: &std::path::Path,
    codex: Option<&mut CodexScanner>,
) -> Scan {
    let taken = std::time::Instant::now();
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, scan_refresh_kind());
    let mut procs = snapshot(sys);
    let mut files = read_agent_files(&procs, claude_home);
    if let Some(scanner) = codex {
        files.codex = codex_sessions(sys, &mut procs, scanner);
    }
    Scan { procs, files, taken }
}

impl ProcScanner {
    pub(crate) fn spawn(ctx: egui::Context, enabled: bool) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let slot = Arc::downgrade(&latest);
        let on = enabled.clone();
        std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            let claude_home = agents::SystemFs::from_env().claude_home;
            let mut codex = CodexScanner::new(agent_codex::codex_home());
            loop {
                if on.load(Ordering::Relaxed) {
                    let scan = scan_now(&mut sys, &claude_home, Some(&mut codex));
                    let Some(slot) = slot.upgrade() else { return };
                    *slot.lock().unwrap() = Some(scan);
                    ctx.request_repaint();
                } else if slot.strong_count() == 0 {
                    return;
                }
                std::thread::sleep(SCAN_INTERVAL);
            }
        });
        Self { latest, enabled }
    }

    pub(crate) fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub(crate) fn take(&self) -> Option<Scan> {
        self.latest.lock().unwrap().take()
    }
}

/// Where a process is actually sitting, asked of the OS. `None` when the pid is gone, the OS won't
/// say, or the answer isn't a directory any more.
///
/// This is the fallback for a pane whose cwd we never learned: `TabState.cwd` is only ever filled by
/// OSC 7, and macOS zsh emits that from `/etc/zshrc_Apple_Terminal` - sourced ONLY when
/// `TERM_PROGRAM == Apple_Terminal`, which ours never is. Our own zsh and bash hooks (`shell.rs`)
/// emit it, but a shell with no hook (fish, sh, integration off) whose own rc files don't emit it
/// stays cwd-less forever, and its tab keeps the bare "zsh" placeholder. Asking the OS costs one
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

/// Whether a process with this pid exists. Startup uses it to tell a live writer from a dead one.
pub(crate) fn process_alive(pid: u32) -> bool {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        false,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some()
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
            start_time: 1000,
            cwd: None,
            uid: None,
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

    #[test]
    fn the_background_scanner_publishes_the_live_table_only_while_enabled() {
        let poll = |sc: &ProcScanner| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if let Some(t) = sc.take() {
                    return Some(t);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            None
        };
        let off = ProcScanner::spawn(egui::Context::default(), false);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(off.take().is_none(), "a disabled scanner must not refresh the table");

        let on = ProcScanner::spawn(egui::Context::default(), true);
        let table = poll(&on).expect("an enabled scanner publishes a table");
        assert!(table.procs.iter().any(|p| p.pid == std::process::id()), "the live table holds us");
        assert!(on.take().is_none(), "take drains the slot until the next scan");
    }

    // --- Agent sessions ------------------------------------------------------------------------

    const ID_A: &str = "0c2cbc96-1111-4222-8333-444455556666";
    const ID_B: &str = "d8b21abe-aaaa-4bbb-8ccc-ddddeeeeffff";

    #[test]
    fn agent_kind_is_strict_about_what_counts_as_an_agent() {
        let cases: [(&str, &[&str], Option<AgentKind>); 21] = [
            ("node", &["node", "/home/me/.npm/bin/claude"], Some(AgentKind::Claude)),
            ("bun", &["bun", "/x/.bin/codex"], Some(AgentKind::Codex)),
            ("node", &["node", "/x/bin/claudette"], None),
            ("claude", &["claude"], Some(AgentKind::Claude)),
            ("claude.exe", &["/x/bin/claude.exe"], Some(AgentKind::Claude)),
            ("codex", &["codex", "resume", ID_A], Some(AgentKind::Codex)),
            (
                "node",
                &["node", "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"],
                Some(AgentKind::Claude),
            ),
            (
                "node",
                &["node", "/x/node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs"],
                Some(AgentKind::Claude),
            ),
            ("node", &["node", "/opt/@openai/codex/bin/codex.js"], Some(AgentKind::Codex)),
            // The launcher is `codex.js` under a `bin` dir, whatever package holds it.
            ("node", &["node", "/pnpm/global/5/.pnpm/x/bin/codex.js"], Some(AgentKind::Codex)),
            // A directory or a repo that is merely named for an agent is not an agent.
            ("node", &["node", "/Users/me/Repos/codex/scripts/build.js"], None),
            ("node", &["node", "/Users/me/Repos/claude/server.js"], None),
            ("node", &["node", "/Users/me/Repos/claude-code/scripts/lint.js"], None),
            ("node", &["node", "/Users/me/Repos/codex/node_modules/.bin/jest"], None),
            ("node", &["node", "/x/scripts/codex"], None),
            ("vim", &["vim", "claude.md"], None),
            ("tail", &["tail", "-f", "/var/log/codex.log"], None),
            ("node", &["node", "server.js", "claude"], None),
            ("node", &["node"], None),
            ("zsh", &["zsh"], None),
            ("gemini", &["gemini"], None),
        ];
        for (name, cmd, want) in cases {
            let cmd: Vec<String> = cmd.iter().map(|s| (*s).to_string()).collect();
            assert_eq!(agent_kind(name, &cmd), want, "{name} {cmd:?}");
        }
    }

    #[test]
    fn a_version_or_help_query_is_not_an_agent_for_tracking() {
        // A user (or a shell prompt) may run `codex --version` at any time. A scan that
        // catches that process must not read it as the pane's agent: it holds no session.
        let cases: [(&str, &[&str]); 8] = [
            ("codex", &["codex", "--version"]),
            ("codex", &["codex", "-V"]),
            ("claude", &["claude", "--version"]),
            ("claude", &["claude", "-h"]),
            ("claude", &["claude", "--help"]),
            ("node", &["node", "/opt/@openai/codex/bin/codex.js", "--version"]),
            ("codex", &["codex", "resume", ID_A, "--help"]),
            ("codex", &["codex", "-c", "x=1", "-V"]),
        ];
        for (name, cmd) in cases {
            let cmd: Vec<String> = cmd.iter().map(|s| (*s).to_string()).collect();
            assert_eq!(agent_kind(name, &cmd), None, "{cmd:?}");
        }
        // The same words as the value of another flag, or in a prompt, are not queries.
        let prompt = ["claude", "-p", "what does -h do"].map(str::to_owned);
        assert_eq!(agent_kind("claude", &prompt), Some(AgentKind::Claude));
        // A scan under the pane's shell sees no agent while the probe runs.
        let procs = vec![p(200, 100, "codex", &["codex", "--version"])];
        assert!(nearest_agent(&procs, 100, None).is_none());
        let procs = vec![p(200, 100, "codex", &["codex", "resume", ID_A])];
        assert_eq!(nearest_agent(&procs, 100, None).map(|(k, _)| k), Some(AgentKind::Codex));
    }

    #[test]
    fn nearest_agent_prefers_the_outer_one() {
        // shell(100) > claude(200) > zsh tool(300) > claude(400)
        let procs = vec![
            p(200, 100, "claude", &["claude"]),
            p(300, 200, "zsh", &["zsh"]),
            p(400, 300, "claude", &["claude", "-p", "x"]),
        ];
        let (kind, proc) = nearest_agent(&procs, 100, None).unwrap();
        assert_eq!((kind, proc.pid), (AgentKind::Claude, 200));
    }

    #[test]
    fn nearest_agent_ignores_the_shell_itself_and_other_trees() {
        let procs = vec![p(100, 1, "claude", &["claude"]), p(500, 1, "codex", &["codex"])];
        assert!(nearest_agent(&procs, 100, None).is_none());
    }

    #[test]
    fn nearest_agent_breaks_a_depth_tie_by_pid() {
        let procs = vec![p(300, 100, "codex", &["codex"]), p(200, 100, "claude", &["claude"])];
        assert_eq!(nearest_agent(&procs, 100, None).map(|(_, p)| p.pid), Some(200));
    }

    #[test]
    fn nearest_agent_prefers_the_foreground_agent_over_a_stopped_one() {
        // `claude --resume` was stopped (Ctrl+Z) and a second `claude` runs in the foreground.
        let procs = vec![
            p(200, 100, "claude", &["claude", "--resume", ID_A]),
            p(300, 100, "claude", &["claude"]),
        ];
        let pid = |fg| nearest_agent(&procs, 100, fg).map(|(_, p)| p.pid);
        assert_eq!(pid(Some(300)), Some(300));
        // The shell is in the foreground, or nothing is known: the lowest pid, as before.
        assert_eq!(pid(Some(100)), Some(200));
        assert_eq!(pid(None), Some(200));
        // A foreground group that is deeper than the nearest agent does not change the answer.
        let deep = vec![p(200, 100, "claude", &["claude"]), p(300, 200, "claude", &["claude"])];
        assert_eq!(nearest_agent(&deep, 100, Some(300)).map(|(_, p)| p.pid), Some(200));
    }

    #[test]
    fn a_codex_process_of_another_user_is_not_a_rival() {
        let mine = sysinfo::Uid::try_from(501usize).unwrap();
        let theirs = sysinfo::Uid::try_from(502usize).unwrap();
        let owned_by = |uid: Option<&sysinfo::Uid>| Proc {
            cwd: Some("/work/a".into()),
            uid: uid.cloned(),
            ..p(200, 100, "codex", &["codex"])
        };
        assert!(codex_proc(&owned_by(Some(&mine)), Some(&mine)).is_some());
        assert!(codex_proc(&owned_by(Some(&theirs)), Some(&mine)).is_none());
        // Unknown users stay rivals, the safe side.
        assert!(codex_proc(&owned_by(None), Some(&mine)).is_some());
        assert!(codex_proc(&owned_by(Some(&theirs)), None).is_some());
    }

    #[test]
    fn codex_cd_uses_the_rollout_directory_instead_of_the_process_cwd() {
        for args in [
            vec!["codex", "-C", "target"],
            vec!["codex", "--cd", "target"],
            vec!["codex", "--cd=target"],
            vec!["codex", "-Ctarget"],
        ] {
            let process = Proc { cwd: Some("/work/launch".into()), ..p(200, 100, "codex", &args) };
            assert_eq!(
                codex_proc(&process, None).unwrap().cwd.as_deref(),
                Some("/work/launch/target"),
                "{args:?}"
            );
        }
    }

    #[test]
    fn codex_cd_resolves_symlinks_like_rollout_metadata() {
        let base = std::env::temp_dir().join(format!("stdusk-codex-cd-{}", std::process::id()));
        std::fs::create_dir_all(base.join("target")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(base.join("target"), &link).unwrap();
        let process = Proc {
            cwd: Some(base.to_str().unwrap().into()),
            ..p(200, 100, "codex", &["codex", "--cd", "link"])
        };
        let got = codex_proc(&process, None).unwrap().cwd;
        let expected = std::fs::canonicalize(base.join("target")).unwrap();
        std::fs::remove_dir_all(base).unwrap();
        assert_eq!(got.as_deref(), expected.to_str());
    }

    #[test]
    fn a_codex_exec_run_is_not_a_rival() {
        let run = |cmd: &[&str]| codex_proc(&p(12, 500, "codex", cmd), None);
        assert!(run(&["codex", "exec", "fix it"]).is_none());
        assert!(run(&["codex", "-c", "a=b", "exec", "fix it"]).is_none());
        assert!(run(&["codex"]).is_some());
    }

    fn scan_with(procs: Vec<Proc>, files: AgentFiles) -> Scan {
        Scan { procs, files, taken: std::time::Instant::now() }
    }

    fn codex_session(id: &str, cwd: &str) -> agents::AgentSession {
        agents::AgentSession {
            kind: AgentKind::Codex,
            id: agents::SessionId::parse(id).unwrap(),
            cwd: cwd.into(),
            transcript: None,
        }
    }

    #[test]
    fn pane_seen_reports_no_agent_for_a_bare_shell() {
        let scan = scan_with(vec![p(200, 100, "vim", &["vim"])], AgentFiles::default());
        assert_eq!(pane_seen(&scan, 100, None), Seen::NoAgent);
    }

    #[test]
    fn pane_seen_gives_each_codex_pane_the_session_of_its_own_process() {
        let (mine, other) = (codex_session(ID_A, "/mine"), codex_session(ID_B, "/other"));
        let mut files = AgentFiles::default();
        files.codex.insert(200, mine.clone());
        files.codex.insert(300, other.clone());
        // Two codex panes in one cwd: each shell sees only its own process.
        let scan = scan_with(
            vec![p(200, 100, "codex", &["codex"]), p(300, 101, "codex", &["codex"])],
            files,
        );
        let seen =
            |pid, session| Seen::Agent { kind: AgentKind::Codex, pid, session, argv_session: None };
        assert_eq!(pane_seen(&scan, 100, None), seen(200, Some(mine)));
        assert_eq!(pane_seen(&scan, 101, None), seen(300, Some(other)));
        // A process the match left out (in doubt, or not yet seen) runs with no session.
        let scan = scan_with(vec![p(200, 100, "codex", &["codex"])], AgentFiles::default());
        assert_eq!(pane_seen(&scan, 100, None), seen(200, None));
    }

    #[test]
    fn pane_seen_names_the_codex_launcher_and_never_a_deeper_process() {
        // `@openai/codex` runs `node codex.js`, which spawns the native codex. The pane keeps the
        // launcher pid (the nearest agent), and the match names the launcher (see `codex_tuis`).
        let mine = codex_session(ID_A, "/mine");
        let inner = codex_session(ID_B, "/inner");
        let mut files = AgentFiles::default();
        files.codex.insert(200, mine.clone());
        files.codex.insert(400, inner);
        let scan = scan_with(
            vec![
                p(200, 100, "node", &["node", "/opt/@openai/codex/bin/codex.js"]),
                p(201, 200, "codex", &["codex"]),
                // codex -> zsh tool -> codex: the inner one is a grandchild and never the pane's.
                p(300, 201, "zsh", &["zsh"]),
                p(400, 300, "codex", &["codex", "exec", "x"]),
            ],
            files,
        );
        let Seen::Agent { pid: 200, session, .. } = pane_seen(&scan, 100, None) else {
            panic!("expected the launcher")
        };
        assert_eq!(session, Some(mine));
    }

    #[test]
    fn codex_tuis_are_every_session_process_and_not_the_daemon_or_a_launcher_child() {
        let procs = vec![
            // The shared daemon, as `ps` shows it on 0.159.2. It has no parent that matters.
            p(
                1,
                0,
                "codex",
                &[
                    "/h/.codex/packages/app-server-daemon/releases/x/bin/codex",
                    "app-server",
                    "--listen",
                    "unix://",
                    "--managed-daemon",
                ],
            ),
            p(2, 0, "codex", &["/h/bin/codex", "app-server", "daemon", "pid-update-loop"]),
            p(10, 100, "codex", &["codex"]),
            p(11, 100, "codex", &["codex", "resume", ID_A]),
            p(12, 500, "codex", &["codex", "exec", "fix it"]), // another terminal's exec
            p(20, 100, "node", &["node", "/opt/@openai/codex/bin/codex.js"]),
            p(21, 20, "codex", &["codex"]), // the launcher's child: the launcher stands for it
            p(30, 100, "codex", &["codex", "mcp-server"]),
            p(31, 100, "codex", &["codex", "login"]),
            p(40, 100, "codex", &["codex", "--version"]), // not an agent at all
            p(50, 100, "claude", &["claude"]),
        ];
        let mut pids: Vec<u32> = codex_tuis(&procs).iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, [10, 11, 12, 20]);
    }

    #[test]
    fn pane_seen_reports_the_session_id_in_the_nearest_agents_command_line() {
        let cases = [
            ("codex", vec!["codex", "resume", ID_A], Some(ID_A)),
            ("node", vec!["node", "/opt/@openai/codex/bin/codex.js", "resume", ID_A], Some(ID_A)),
            ("claude", vec!["claude", "--resume", ID_A], Some(ID_A)),
            ("claude", vec!["claude"], None),
        ];
        for (name, cmd, want) in cases {
            let scan = scan_with(vec![p(200, 100, name, &cmd)], AgentFiles::default());
            let Seen::Agent { argv_session, .. } = pane_seen(&scan, 100, None) else {
                panic!("expected an agent for {cmd:?}");
            };
            assert_eq!(argv_session.as_ref().map(agents::SessionId::as_str), want, "{cmd:?}");
        }
    }

    #[test]
    fn process_alive_tells_a_live_pid_from_a_reaped_one() {
        assert!(process_alive(std::process::id()));
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }

    #[test]
    fn a_process_that_execs_after_a_scan_gets_its_new_name() {
        // A shell forks and execs. A scan between the two sees the shell's name. The next scan,
        // after the exec, must show the new name, though the pid and the start time stay. The
        // shell waits on a pipe that the test controls, so no timing decides the order.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read x; exec sleep 8"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("/bin/sh exists on every unix");
        let pid = sysinfo::Pid::from_u32(child.id());
        let mut sys = sysinfo::System::new();
        let mut name_now = || {
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                scan_refresh_kind(),
            );
            snapshot(&sys).into_iter().find(|p| p.pid == pid.as_u32()).map(|p| p.name)
        };
        // The child may still be between its fork and its exec, and show the name of this test
        // binary. Wait for the shell's name. It holds until the test writes to the pipe.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut name = name_now();
        while name.as_deref() != Some("sh") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            name = name_now();
        }
        assert_eq!(name.as_deref(), Some("sh"), "before the exec");
        // Let `read` finish. The shell then execs `sleep`.
        let mut pipe = child.stdin.take().expect("stdin was piped");
        std::io::Write::write_all(&mut pipe, b"go\n").unwrap();
        drop(pipe);
        while name.as_deref() != Some("sleep") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            name = name_now();
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"), "after the exec");
    }

    /// A scan reads the whole process table, so two tests that each spawn a fake `codex` child of
    /// this test binary would see each other's child. One test at a time owns the table.
    fn own_the_process_table() -> std::sync::MutexGuard<'static, ()> {
        static TABLE: Mutex<()> = Mutex::new(());
        // A failed test poisons the lock. The next test still needs its turn.
        TABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Scan until `ready` accepts the scan (or 5 s pass). A child just spawned may still be between
    /// its fork and its exec, and shows the name of this test binary until then.
    fn scan_until(
        sys: &mut sysinfo::System,
        home: &std::path::Path,
        mut codex: Option<&mut CodexScanner>,
        ready: impl Fn(&Scan) -> bool,
    ) -> Scan {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let scan = scan_now(sys, home, codex.as_deref_mut());
            if ready(&scan) || std::time::Instant::now() > deadline {
                return scan;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn a_scan_taken_now_sees_the_agent_under_this_process() {
        // The quit and handoff paths take one scan on the UI thread. A `codex` symlink to `sleep`
        // stands in for the agent, a child of this test process as an agent is a child of a shell.
        let _table = own_the_process_table();
        let dir = std::env::temp_dir().join(format!("stdusk-scannow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("/bin/sleep", dir.join("codex")).unwrap();
        let mut child = std::process::Command::new(dir.join("codex")).arg("30").spawn().unwrap();
        let mut sys = sysinfo::System::new();
        let root = std::process::id();
        let before = std::time::Instant::now();
        let scan =
            scan_until(&mut sys, &dir, None, |s| nearest_agent(&s.procs, root, None).is_some());
        let took = before.elapsed();
        let found = nearest_agent(&scan.procs, root, None).map(|(kind, p)| (kind, p.pid));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Some((AgentKind::Codex, child.id())));
        assert!(scan.taken >= before && scan.taken <= before + took, "taken is stamped first");
        eprintln!("scan_now took {took:?} for {} processes", scan.procs.len());
    }

    #[test]
    fn pane_seen_falls_back_to_the_claude_registry_by_pid() {
        let json = format!(
            r#"{{"pid":200,"sessionId":"{ID_A}","cwd":"/reg","startedAt":1000000,"kind":"interactive"}}"#
        );
        let mut files = AgentFiles::default();
        files.registry.insert(200, json);
        let scan = scan_with(vec![p(200, 100, "claude", &["claude"])], files);
        let Seen::Agent { kind: AgentKind::Claude, pid: 200, session: Some(s), .. } =
            pane_seen(&scan, 100, None)
        else {
            panic!("expected a registry session");
        };
        assert_eq!((s.id.as_str(), s.cwd.as_str()), (ID_A, "/reg"));
    }

    #[test]
    fn pane_seen_reads_the_claude_registry_of_a_launcher_child() {
        // `cli-wrapper.cjs` (the npm fallback launcher) runs the native claude as its child, and
        // that child owns the registry file.
        let json = format!(
            r#"{{"pid":201,"sessionId":"{ID_A}","cwd":"/reg","startedAt":1000000,"kind":"interactive"}}"#
        );
        let mut files = AgentFiles::default();
        files.registry.insert(201, json);
        let scan = scan_with(
            vec![
                p(
                    200,
                    100,
                    "node",
                    &["node", "/x/node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs"],
                ),
                p(201, 200, "claude", &["claude"]),
            ],
            files,
        );
        let Seen::Agent { pid: 200, session: Some(s), .. } = pane_seen(&scan, 100, None) else {
            panic!("expected the child's registry session");
        };
        assert_eq!(s.id.as_str(), ID_A);
    }

    #[test]
    fn read_agent_files_reads_the_registry_of_claude_processes_only() {
        let base = std::env::temp_dir().join(format!("stdusk-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("claude");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::write(home.join("sessions/200.json"), "{\"pid\":200}").unwrap();
        std::fs::write(home.join("sessions/300.json"), "{\"pid\":300}").unwrap();
        let procs = vec![
            p(200, 100, "claude", &["claude"]),
            p(300, 100, "codex", &["codex"]), // not a Claude process: its file is not read
            p(400, 100, "claude", &["claude"]), // a Claude process with no file
        ];

        let files = read_agent_files(&procs, &home);

        assert_eq!(files.registry.len(), 1);
        assert_eq!(files.registry.get(&200).map(String::as_str), Some("{\"pid\":200}"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_live_codex_process_gets_its_cwd_and_its_thread_from_the_rollout_scan() {
        // A `codex` symlink to `sleep` stands in for the TUI. The rollout is the one Codex would
        // write 0.3 s after the start. The real OS answers for the start time and the cwd.
        let _table = own_the_process_table();
        let base = std::env::temp_dir().join(format!("stdusk-livecodex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (bin, work, home) = (base.join("bin"), base.join("work"), base.join("codex-home"));
        for d in [&bin, &work] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::os::unix::fs::symlink("/bin/sleep", bin.join("codex")).unwrap();
        let mut child = std::process::Command::new(bin.join("codex"))
            .arg("30")
            .current_dir(&work)
            .spawn()
            .unwrap();
        let now_ms =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
                as u64;
        let t = now_ms + 300;
        let id = format!("{:08x}-{:04x}-7000-8000-{:012x}", t >> 16, t & 0xffff, 1);
        let day = home.join("sessions/2026/09/30");
        std::fs::create_dir_all(&day).unwrap();
        // The cwd goes in as the shell would say it: through the symlink /var -> /private/var.
        let line = format!(
            r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":{:?},"originator":"codex-tui","thread_source":"user"}}}}"#,
            work.to_str().unwrap()
        );
        std::fs::write(day.join(format!("rollout-2026-09-30T12-00-00-{id}.jsonl")), line).unwrap();

        let mut sys = sysinfo::System::new();
        let mut scanner = CodexScanner::new(home);
        let pid = child.id();
        let scan =
            scan_until(&mut sys, &base, Some(&mut scanner), |s| s.files.codex.contains_key(&pid));

        let _ = child.kill();
        let _ = child.wait();
        // The OS gave the user too, so the other-user filter had something to compare.
        let uid = scan.procs.iter().find(|p| p.pid == pid).map(|p| p.uid.is_some());
        assert_eq!(uid, Some(true));
        let got = scan.files.codex.get(&child.id()).map(|s| s.id.as_str().to_owned());
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(got.as_deref(), Some(id.as_str()));
    }
}
