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

/// Live scan: snapshot sysinfo's process table into `Proc`s and run `detect`. Cheap enough for a
/// ~1 Hz call; sysinfo refresh happens in the caller so one refresh serves every tab.
pub(crate) fn scan(sys: &sysinfo::System, root: u32) -> Option<Cli> {
    let procs: Vec<Proc> = sys
        .processes()
        .values()
        .map(|p| Proc {
            pid: p.pid().as_u32(),
            parent: p.parent().map(sysinfo::Pid::as_u32),
            name: p.name().to_string_lossy().into_owned(),
            cmd: p.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect(),
        })
        .collect();
    detect(&procs, root)
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
}
