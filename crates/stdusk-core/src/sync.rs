//! Settings sync over the system `git` with the user's own credentials; `run` blocks, so call it off the UI thread.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Push,
    Pull,
    /// Commit local edits, merge what the repo gained and push. Local content wins every
    /// conflict, so nothing on this machine is ever overwritten.
    Reconcile,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Synced,
    /// The repo and this machine edited the same lines; this machine's version was kept.
    KeptLocal,
}

/// Which files in the folder travel. Everything else stays on this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Files {
    Except(&'static [&'static str]),
    Only(&'static [&'static str]),
}

impl Files {
    #[must_use]
    pub fn gitignore(self) -> String {
        match self {
            Self::Except(names) => ignore_lines("", names),
            Self::Only(names) => format!("*\n{}", ignore_lines("!", names)),
        }
    }
}

fn ignore_lines(prefix: &str, names: &[&str]) -> String {
    names.iter().flat_map(|name| [prefix, name, "\n"]).collect()
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub dir: PathBuf,
    pub files: Files,
    pub app: &'static str,
    pub env: Vec<(OsString, OsString)>,
}

impl Spec {
    #[must_use]
    pub fn new(dir: PathBuf, files: Files, app: &'static str) -> Self {
        Self { dir, files, app, env: Vec::new() }
    }
}

/// One git invocation. `tolerant` marks a step that may legitimately fail, such as removing a
/// remote that is not there yet or committing when nothing changed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    pub args: Vec<String>,
    pub tolerant: bool,
}

fn step(args: &[&str], tolerant: bool) -> Step {
    Step { args: args.iter().map(|arg| (*arg).to_owned()).collect(), tolerant }
}

fn bootstrap(repo: &str) -> Vec<Step> {
    vec![
        step(&["init", "-q", "-b", "main"], false),
        step(&["remote", "remove", "origin"], true),
        step(&["remote", "add", "origin", repo], false),
    ]
}

fn snapshot(app: &str) -> Vec<Step> {
    vec![
        step(&["add", "-A"], false),
        step(&["commit", "-q", "-m", &format!("{app} settings sync")], true),
    ]
}

/// The git command plan for `op` against `repo`. `Reconcile` returns only the steps that always
/// run (bootstrap and a local snapshot); `run` adds fetch, merge and push once it knows whether
/// the repo has a branch yet.
#[must_use]
pub fn plan(op: Op, repo: &str, app: &str) -> Vec<Step> {
    let mut steps = bootstrap(repo);
    match op {
        Op::Push => {
            steps.extend(snapshot(app));
            steps.push(step(&["push", "-q", "-u", "origin", "main"], false));
        }
        Op::Pull => {
            steps.push(step(&["fetch", "-q", "origin", "main"], false));
            steps.push(step(&["reset", "-q", "--hard", "FETCH_HEAD"], false));
        }
        Op::Reconcile => steps.extend(snapshot(app)),
    }
    steps
}

/// The merges `Reconcile` tries in order, each less gentle with the repo's side than the last.
fn merge_ladder() -> [Step; 3] {
    let merge = |extra: &[&'static str]| {
        let mut args = vec!["merge", "-q", "--no-edit", "--allow-unrelated-histories"];
        args.extend_from_slice(extra);
        args.push("FETCH_HEAD");
        step(&args, false)
    };
    [merge(&[]), merge(&["-X", "ours"]), merge(&["-s", "ours"])]
}

fn git(spec: &Spec, args: &[String]) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(&spec.dir).args(args);
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.output().map_err(|e| e.to_string())
}

fn failure(step: &Step, out: &Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    format!("git {}: {}", step.args.join(" "), err.trim())
}

fn exec(spec: &Spec, step: &Step) -> Result<Output, String> {
    let out = git(spec, &step.args)?;
    if out.status.success() || step.tolerant { Ok(out) } else { Err(failure(step, &out)) }
}

fn exec_all(spec: &Spec, steps: &[Step]) -> Result<(), String> {
    steps.iter().try_for_each(|s| exec(spec, s).map(drop))
}

fn succeeds(spec: &Spec, args: &[&str]) -> Result<bool, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    Ok(git(spec, &args)?.status.success())
}

fn remote_has_main(spec: &Spec) -> Result<bool, String> {
    let probe = step(&["ls-remote", "--exit-code", "--heads", "origin", "main"], true);
    let out = exec(spec, &probe)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        _ => Err(failure(&probe, &out)),
    }
}

fn merge_in_progress(spec: &Spec) -> Result<bool, String> {
    succeeds(spec, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
}

fn merge_keeping_local(spec: &Spec) -> Result<Outcome, String> {
    let mut outcome = Outcome::Synced;
    let mut last = String::new();
    for attempt in merge_ladder() {
        let out = exec(spec, &Step { tolerant: true, ..attempt.clone() })?;
        if out.status.success() {
            return Ok(outcome);
        }
        last = failure(&attempt, &out);
        if !merge_in_progress(spec)? {
            return Err(last);
        }
        outcome = Outcome::KeptLocal;
        exec(spec, &step(&["merge", "--abort"], true))?;
    }
    Err(last)
}

fn reconcile(spec: &Spec, repo: &str) -> Result<Outcome, String> {
    exec_all(spec, &plan(Op::Reconcile, repo, spec.app))?;
    let mut outcome = Outcome::Synced;
    if remote_has_main(spec)? {
        exec(spec, &step(&["fetch", "-q", "origin", "main"], false))?;
        outcome = merge_keeping_local(spec)?;
    }
    if succeeds(spec, &["rev-parse", "-q", "--verify", "HEAD"])? {
        exec(spec, &step(&["push", "-q", "-u", "origin", "main"], false))?;
    }
    Ok(outcome)
}

/// Runs `op` in the spec's folder. Blocking.
pub fn run(spec: &Spec, op: Op, repo: &str) -> Result<Outcome, String> {
    std::fs::create_dir_all(&spec.dir).map_err(|e| e.to_string())?;
    std::fs::write(spec.dir.join(".gitignore"), spec.files.gitignore())
        .map_err(|e| e.to_string())?;
    match op {
        Op::Reconcile => reconcile(spec, repo),
        Op::Push | Op::Pull => {
            exec_all(spec, &plan(op, repo, spec.app))?;
            Ok(Outcome::Synced)
        }
    }
}

/// The in-flight check is the debounce: rapid saves collapse into the sync already running.
#[must_use]
pub fn should_autosync(auto: bool, repo_set: bool, busy: bool) -> bool {
    auto && repo_set && !busy
}

/// `None` is a manual pull, which is never stale.
#[must_use]
pub fn pull_is_stale(launch_baseline: Option<&str>, current_toml: &str) -> bool {
    launch_baseline.is_some_and(|b| b != current_toml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const STDUSK_IGNORED: &[&str] = &["session.toml", "shell/"];

    fn flat(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| s.args.join(" ")).collect()
    }

    #[test]
    fn push_plan_bootstraps_commits_and_pushes() {
        let p = plan(Op::Push, "git@github.com:me/stdusk-settings.git", "stdusk");
        assert_eq!(
            flat(&p),
            [
                "init -q -b main",
                "remote remove origin",
                "remote add origin git@github.com:me/stdusk-settings.git",
                "add -A",
                "commit -q -m stdusk settings sync",
                "push -q -u origin main",
            ]
        );
        let tolerant: Vec<bool> = p.iter().map(|s| s.tolerant).collect();
        assert_eq!(tolerant, [false, true, false, false, true, false]);
    }

    #[test]
    fn the_commit_message_names_the_app() {
        let p = plan(Op::Push, "url", "tidewatch");
        assert!(flat(&p).contains(&"commit -q -m tidewatch settings sync".to_owned()));
    }

    #[test]
    fn pull_plan_fetches_and_hard_resets() {
        let p = plan(Op::Pull, "url", "stdusk");
        let last = &p[p.len() - 1];
        assert_eq!(last.args, ["reset", "-q", "--hard", "FETCH_HEAD"]);
        assert!(p.iter().any(|s| s.args.first().map(String::as_str) == Some("fetch")));
    }

    #[test]
    fn reconcile_plan_only_snapshots_locally() {
        let steps = flat(&plan(Op::Reconcile, "url", "tidewatch"));
        assert_eq!(steps[3], "add -A");
        assert!(steps.iter().all(|s| !s.starts_with("push") && !s.starts_with("reset")));
    }

    #[test]
    fn the_merge_ladder_ends_with_the_gentlest_side_taking_everything_local() {
        let ladder = flat(&merge_ladder());
        assert_eq!(ladder[1], "merge -q --no-edit --allow-unrelated-histories -X ours FETCH_HEAD");
        assert_eq!(ladder[2], "merge -q --no-edit --allow-unrelated-histories -s ours FETCH_HEAD");
    }

    #[test]
    fn autosync_needs_opt_in_repo_and_an_idle_worker() {
        let cases = [
            (true, true, false, true),
            (true, true, true, false),
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
        assert!(!pull_is_stale(Some(spawn_time), "a = 1"));
        assert!(pull_is_stale(Some(spawn_time), "a = 2"));
        assert!(!pull_is_stale(None, "a = 2"));
    }

    #[test]
    fn except_lists_what_stays_home_and_only_lists_what_travels() {
        assert_eq!(Files::Except(STDUSK_IGNORED).gitignore(), "session.toml\nshell/\n");
        assert_eq!(Files::Only(&["a.toml", "b.toml"]).gitignore(), "*\n!a.toml\n!b.toml\n");
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static CALLS: AtomicUsize = AtomicUsize::new(0);
            let call = CALLS.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("stdusk-sync-test-{}-{call}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn hermetic_env() -> Vec<(OsString, OsString)> {
        [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_AUTHOR_NAME", "Test"),
            ("GIT_AUTHOR_EMAIL", "test@example.invalid"),
            ("GIT_COMMITTER_NAME", "Test"),
            ("GIT_COMMITTER_EMAIL", "test@example.invalid"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    fn machine(scratch: &Scratch, name: &str, files: Files) -> Spec {
        let mut spec = Spec::new(scratch.path(name), files, "test");
        spec.env = hermetic_env();
        spec
    }

    fn bare_remote(scratch: &Scratch) -> String {
        let dir = scratch.path("remote.git");
        let status = Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&dir)
            .envs(hermetic_env())
            .status()
            .unwrap();
        assert!(status.success());
        dir.to_string_lossy().into_owned()
    }

    fn write(spec: &Spec, name: &str, text: &str) {
        std::fs::create_dir_all(&spec.dir).unwrap();
        std::fs::write(spec.dir.join(name), text).unwrap();
    }

    fn read(spec: &Spec, name: &str) -> String {
        std::fs::read_to_string(spec.dir.join(name)).unwrap_or_default()
    }

    fn remote_files(remote: &str) -> Vec<String> {
        let out = Command::new("git")
            .args(["-C", remote, "ls-tree", "-r", "--name-only", "main"])
            .envs(hermetic_env())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }

    fn remote_text(remote: &str, name: &str) -> String {
        let out = Command::new("git")
            .args(["-C", remote, "show", &format!("main:{name}")])
            .envs(hermetic_env())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn exists(dir: &Path, name: &str) -> bool {
        dir.join(name).exists()
    }

    const TRAVELS: &[&str] = &["settings.toml", "profiles.toml"];

    #[test]
    fn only_the_listed_files_reach_the_repo() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        write(&mac, "profiles.toml", "b = 1\n");
        write(&mac, "trust.toml", "secret = true\n");
        write(&mac, "notes.md", "local\n");
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::Synced));
        let mut shipped = remote_files(&remote);
        shipped.sort();
        assert_eq!(shipped, ["profiles.toml", "settings.toml"]);
    }

    #[test]
    fn a_second_machine_receives_the_files_and_keeps_its_own_extras() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&other, "trust.toml", "mine = true\n");
        assert_eq!(run(&other, Op::Reconcile, &remote), Ok(Outcome::Synced));
        assert_eq!(read(&other, "settings.toml"), "a = 1\n");
        assert_eq!(read(&other, "trust.toml"), "mine = true\n");
        assert!(!exists(&scratch.path("remote.git"), "trust.toml"));
    }

    #[test]
    fn a_change_made_elsewhere_arrives_on_the_next_sync() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&other, "profiles.toml", "b = 2\n");
        run(&other, Op::Reconcile, &remote).unwrap();
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::Synced));
        assert_eq!(read(&mac, "profiles.toml"), "b = 2\n");
        assert_eq!(read(&mac, "settings.toml"), "a = 1\n");
    }

    #[test]
    fn edits_to_different_files_merge_without_a_conflict() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        write(&mac, "profiles.toml", "b = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&other, "profiles.toml", "b = 2\n");
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&mac, "settings.toml", "a = 5\n");
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::Synced));
        assert_eq!(read(&mac, "settings.toml"), "a = 5\n");
        assert_eq!(read(&mac, "profiles.toml"), "b = 2\n");
        assert_eq!(remote_text(&remote, "settings.toml"), "a = 5\n");
        assert_eq!(remote_text(&remote, "profiles.toml"), "b = 2\n");
    }

    #[test]
    fn a_conflict_keeps_the_local_version_and_says_so() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&other, "settings.toml", "a = 2\n");
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&mac, "settings.toml", "a = 3\n");
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::KeptLocal));
        assert_eq!(read(&mac, "settings.toml"), "a = 3\n");
        assert_eq!(remote_text(&remote, "settings.toml"), "a = 3\n");
    }

    #[test]
    fn a_conflict_still_takes_the_non_conflicting_part_of_the_repo() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n\nz = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&other, "settings.toml", "a = 2\n\nz = 9\n");
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&mac, "settings.toml", "a = 3\n\nz = 1\n");
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::KeptLocal));
        assert_eq!(read(&mac, "settings.toml"), "a = 3\n\nz = 9\n");
    }

    #[test]
    fn two_machines_that_both_started_alone_keep_local_on_the_same_file() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        write(&other, "settings.toml", "a = 2\n");
        assert_eq!(run(&other, Op::Reconcile, &remote), Ok(Outcome::KeptLocal));
        assert_eq!(read(&other, "settings.toml"), "a = 2\n");
        assert_eq!(remote_text(&remote, "settings.toml"), "a = 2\n");
    }

    #[test]
    fn a_file_deleted_in_the_repo_but_edited_here_survives() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        let other = machine(&scratch, "other", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        write(&mac, "profiles.toml", "b = 1\n");
        run(&mac, Op::Reconcile, &remote).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        std::fs::remove_file(other.dir.join("profiles.toml")).unwrap();
        run(&other, Op::Reconcile, &remote).unwrap();
        write(&mac, "profiles.toml", "b = 7\n");
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::KeptLocal));
        assert_eq!(read(&mac, "profiles.toml"), "b = 7\n");
    }

    #[test]
    fn nothing_to_sync_against_an_empty_repo_is_not_an_error() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        assert_eq!(run(&mac, Op::Reconcile, &remote), Ok(Outcome::Synced));
    }

    #[test]
    fn a_missing_repo_is_an_error_naming_git() {
        let scratch = Scratch::new();
        let mac = machine(&scratch, "mac", Files::Only(TRAVELS));
        write(&mac, "settings.toml", "a = 1\n");
        let missing = scratch.path("nope.git").to_string_lossy().into_owned();
        let err = run(&mac, Op::Reconcile, &missing).unwrap_err();
        assert!(err.starts_with("git ls-remote"), "{err}");
    }

    #[test]
    fn stdusk_push_then_pull_round_trips_its_config_and_skips_session_state() {
        let scratch = Scratch::new();
        let remote = bare_remote(&scratch);
        let first = machine(&scratch, "first", Files::Except(STDUSK_IGNORED));
        write(&first, "config.toml", "theme = 1\n");
        write(&first, "session.toml", "tabs = 3\n");
        assert_eq!(run(&first, Op::Push, &remote), Ok(Outcome::Synced));
        let mut shipped = remote_files(&remote);
        shipped.sort();
        assert_eq!(shipped, [".gitignore", "config.toml"]);
        let second = machine(&scratch, "second", Files::Except(STDUSK_IGNORED));
        write(&second, "config.toml", "theme = 9\n");
        assert_eq!(run(&second, Op::Pull, &remote), Ok(Outcome::Synced));
        assert_eq!(read(&second, "config.toml"), "theme = 1\n");
    }
}
