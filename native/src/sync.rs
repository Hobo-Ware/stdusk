//! Settings sync: push/pull `~/.config/stdusk` (config.toml + custom schemes) to a
//! user-provided git repo - typically a private GitHub repo - using the system `git` and the
//! user's existing credentials (SSH key / credential helper). No OAuth, no tokens stored.
//!
//! The command plan is pure and unit-tested; execution runs on a background thread and
//! reports back through a shared slot the UI polls each frame.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Volatile files that must never leave the machine (session state, generated shell hooks).
const GITIGNORE: &str = "session.toml\nshell/\n";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Push,
    Pull,
}

/// One step of the plan: the git args, plus whether a non-zero exit is tolerated (e.g.
/// removing a remote that isn't there yet, committing when nothing changed).
pub(crate) struct Step {
    pub(crate) args: Vec<&'static str>,
    pub(crate) arg: Option<String>, // trailing dynamic argument (the repo url), if any
    pub(crate) tolerant: bool,
}

fn step(args: &[&'static str], tolerant: bool) -> Step {
    Step { args: args.to_vec(), arg: None, tolerant }
}

/// The git command plan for `op` against `repo`. Pure - unit-tested below.
pub(crate) fn plan(op: Op, repo: &str) -> Vec<Step> {
    let mut steps = vec![
        // Idempotent bootstrap: (re)init and point origin at the configured repo.
        step(&["init", "-q", "-b", "main"], false),
        step(&["remote", "remove", "origin"], true), // absent on first run
        Step { args: vec!["remote", "add", "origin"], arg: Some(repo.to_owned()), tolerant: false },
    ];
    match op {
        Op::Push => {
            steps.push(step(&["add", "-A"], false));
            // Nothing-to-commit is fine; the push still syncs any earlier commit.
            steps.push(step(&["commit", "-q", "-m", "stdusk settings sync"], true));
            steps.push(step(&["push", "-q", "-u", "origin", "main"], false));
        }
        Op::Pull => {
            steps.push(step(&["fetch", "-q", "origin", "main"], false));
            // Sync semantics: the repo wins over local edits.
            steps.push(step(&["reset", "-q", "--hard", "FETCH_HEAD"], false));
        }
    }
    steps
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config/stdusk"))
}

/// Run the plan in `~/.config/stdusk`. Blocking - call from `spawn`.
fn run(op: Op, repo: &str) -> Result<(), String> {
    let dir = config_dir().ok_or("no HOME")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Keep machine-local files out of the repo before any `add -A`.
    std::fs::write(dir.join(".gitignore"), GITIGNORE).map_err(|e| e.to_string())?;
    for s in plan(op, repo) {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&dir).args(&s.args);
        if let Some(a) = &s.arg {
            cmd.arg(a);
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !out.status.success() && !s.tolerant {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git {}: {}", s.args.join(" "), err.trim()));
        }
    }
    Ok(())
}

/// Should an automatic sync op start now? On only when the user opted in, a repo is
/// configured, and no sync is already in flight (the in-flight check is the debounce:
/// rapid Saves collapse into whatever push is already running).
pub(crate) fn should_autosync(auto: bool, repo_set: bool, busy: bool) -> bool {
    auto && repo_set && !busy
}

/// Is a finished launch-autosync pull STALE? True when the config changed (a Save or a live
/// settings edit) while the pull ran - the user's version wins; the caller skips the apply
/// and restores the local file (the pull's hard reset already replaced it on disk). A manual
/// Pull passes no baseline (`None`) and is never stale: overwriting local is its whole point.
pub(crate) fn pull_is_stale(launch_baseline: Option<&str>, current_toml: &str) -> bool {
    launch_baseline.is_some_and(|b| b != current_toml)
}

/// Result slot the UI polls: set once by the worker thread, taken by the frame loop.
pub(crate) type SyncSlot = Arc<Mutex<Option<(Op, Result<(), String>)>>>;

/// Run `op` on a background thread; wakes the UI when done.
pub(crate) fn spawn(op: Op, repo: String, slot: &SyncSlot, ctx: egui::Context) {
    let slot = slot.clone();
    std::thread::spawn(move || {
        let res = run(op, &repo);
        *slot.lock().unwrap() = Some((op, res));
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_plan_bootstraps_commits_and_pushes() {
        let p = plan(Op::Push, "git@github.com:me/stdusk-settings.git");
        let flat: Vec<String> = p
            .iter()
            .map(|s| {
                let mut v = s.args.join(" ");
                if let Some(a) = &s.arg {
                    v.push(' ');
                    v.push_str(a);
                }
                v
            })
            .collect();
        assert_eq!(
            flat,
            [
                "init -q -b main",
                "remote remove origin",
                "remote add origin git@github.com:me/stdusk-settings.git",
                "add -A",
                "commit -q -m stdusk settings sync",
                "push -q -u origin main",
            ]
        );
        // Only the may-legitimately-fail steps are tolerant.
        let tolerant: Vec<bool> = p.iter().map(|s| s.tolerant).collect();
        assert_eq!(tolerant, [false, true, false, false, true, false]);
    }

    #[test]
    fn pull_plan_fetches_and_hard_resets() {
        let p = plan(Op::Pull, "url");
        let last = &p[p.len() - 1];
        assert_eq!(last.args, ["reset", "-q", "--hard", "FETCH_HEAD"]);
        assert!(p.iter().any(|s| s.args.first() == Some(&"fetch")));
    }

    #[test]
    fn autosync_needs_opt_in_repo_and_an_idle_worker() {
        // (auto, repo_set, busy) -> start?
        let cases = [
            (true, true, false, true),
            (true, true, true, false), // in-flight op wins (the debounce)
            (true, false, false, false),
            (false, true, false, false),
            (false, false, false, false),
        ];
        for (auto, repo, busy, want) in cases {
            assert_eq!(
                should_autosync(auto, repo, busy),
                want,
                "auto={auto} repo={repo} busy={busy}"
            );
        }
    }

    #[test]
    fn launch_pull_is_stale_only_when_the_config_changed_under_it() {
        let spawn_time = "a = 1";
        assert!(!pull_is_stale(Some(spawn_time), "a = 1")); // untouched: apply the pull
        assert!(pull_is_stale(Some(spawn_time), "a = 2")); // edited/saved meanwhile: skip
        assert!(!pull_is_stale(None, "a = 2")); // manual Pull: always applies
    }

    #[test]
    fn gitignore_excludes_machine_local_files() {
        assert!(GITIGNORE.contains("session.toml"));
        assert!(GITIGNORE.contains("shell/"));
    }
}
