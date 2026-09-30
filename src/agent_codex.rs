//! Codex session capture with no hook and no wrapper. On Codex 0.159.x the TUI holds no session
//! id: it talks to one shared app-server daemon per `CODEX_HOME`, and the daemon owns the threads.
//! So stdusk matches each live Codex TUI to a thread by facts it can see from outside:
//!
//! - Exact: a TUI started as `codex resume <id>` runs that thread. This covers every pane that
//!   stdusk itself resumed.
//! - Timing: a thread id is a UUIDv7, so it holds its creation time. Codex creates the first thread
//!   about 0.4 s to 0.7 s after its TUI starts, and `/new` makes a later one. A user thread of a TUI
//!   belongs to a TUI with the same cwd that started before it. See [`judge`] for the rules.
//! - Kind: only a thread that the TUI made for the user counts (see `tui_user_thread_cwd` in
//!   `agent_codex_scan`). A sub-agent, a `/review` and a `codex exec` write rollouts too, and they
//!   are not the pane's.
//! - Doubt: when two TUIs could own a thread, nobody gets it. The pane then has no id for it.
//!   A TUI that closed still counts for a thread it made, because the daemon writes some late.
//!   This holds only for a TUI that at least one scan saw.
//!
//! The timing rules are a heuristic on undocumented Codex internals (the daemon, and the rollout
//! layout under `$CODEX_HOME/sessions`). A wrong pick is possible in rare cases. Section 8 of the
//! meta-plan lists them. This file is pure: the types, [`judge`] and the argv and env readers. The
//! rollout reads and the scan memory are in `agent_codex_scan`.

use std::path::PathBuf;

use crate::agents::SessionId;

/// How long after its TUI starts Codex creates the first thread. Measured on 0.159.2, from the TUI
/// spawn to the thread id time:
/// - warm daemon: +0.38 s to +0.40 s
/// - cold daemon (after a login or a reboot): +0.45 s to +0.66 s
/// - a fresh `CODEX_HOME`, the very first run: +3.2 s to +3.6 s
///
/// The OS start time is whole seconds (sysinfo drops the microseconds), so the gap we see is up to
/// 1 s longer. The cold daemon reaches 1.51 s at most, so 2 s holds with 0.5 s to spare. The fresh
/// home does not fit, and a wider window would make two panes opened 2 s to 4 s apart ambiguous,
/// which is the common case. So the window stays, and the fresh home is a known limit (see the
/// last case of the `judge` test).
const FIRST_THREAD_WINDOW_MS: u64 = 2_000;

/// One Codex thread that has a rollout file (so it can be resumed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Thread {
    pub(crate) id: SessionId,
    /// The directory from the rollout's first line, symlinks resolved as far as they still exist.
    pub(crate) cwd: String,
    /// From the UUIDv7 id.
    pub(crate) time_ms: u64,
    pub(crate) rollout: String,
}

/// One live Codex process that may hold a thread: a TUI, or another command that is not a service
/// (`codex sandbox`, `codex agents`). Each one is a rival for the threads in its cwd, which is the
/// safe side. A `codex exec` run is not one (see [`is_exec`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexProc {
    pub(crate) pid: u32,
    /// Whole seconds. The OS gives no finer start time.
    pub(crate) start_secs: u64,
    /// `None` when the OS would not say.
    pub(crate) cwd: Option<String>,
    /// The id after `resume` in its command line: its thread, and exact.
    pub(crate) resumes: Option<SessionId>,
}

/// A Codex process that at least one scan saw and that is not live now. The shared daemon writes the
/// threads of a closed TUI late (an empty `/new`, a TUI closed at once), so a thread made while the
/// process lived can still show up. Until it does, the process stays a rival for it. A process
/// that started and ended between two scans is never seen, so it is never a rival.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    pub(crate) proc: CodexProc,
    /// The time of the first scan that missed it. A thread made after this is not its thread.
    pub(crate) ended_ms: u64,
}

/// Who owns one thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Owner(u32),
    /// More than one TUI could own it, so nobody gets it.
    Ambiguous,
    /// No TUI could own it, gone or live (it began before every live TUI).
    Nobody,
    /// Only a gone TUI could own it. Its pane is closed, so nobody gets it, and this is final.
    Closed,
}

fn start_ms(tui: &CodexProc) -> u64 {
    tui.start_secs.saturating_mul(1000)
}

/// True when `a` and `b` are one process: the same pid and the same start time.
pub(crate) fn same_process(a: &CodexProc, b: &CodexProc) -> bool {
    a.pid == b.pid && a.start_secs == b.start_secs
}

/// The rules for one thread, simplest first:
/// 1. Only a TUI with the same cwd counts, and only one that started before the thread.
/// 2. If the thread appeared within [`FIRST_THREAD_WINDOW_MS`] of the start of exactly one such
///    TUI, it is that TUI's first thread. A resumed TUI (`resume <id>`) makes no thread at start,
///    so it cannot own a first thread.
/// 3. Otherwise it is a later thread (`/new`). It belongs to the TUI if exactly one TUI counts.
/// 4. With more than one candidate, nobody gets it. A TUI with an unknown cwd could be anywhere,
///    so it blocks a match and cannot get one.
/// 5. A gone TUI counts as a candidate for a thread made before it left. If it is the only one,
///    the verdict is [`Verdict::Closed`]: nobody gets the thread, because its pane is closed. A
///    gone entry that is also a live process (a scan missed it once) is not a rival of itself, so
///    it is skipped.
///
/// Each thread has one verdict, so one thread never goes to two panes.
pub(crate) fn judge(thread: &Thread, procs: &[CodexProc], gone: &[Gone]) -> Verdict {
    let alive = procs.iter().map(|p| (p, true));
    let left = gone
        .iter()
        .filter(|g| thread.time_ms <= g.ended_ms)
        .filter(|g| !procs.iter().any(|p| same_process(p, &g.proc)))
        .map(|g| (&g.proc, false));
    let started: Vec<(&CodexProc, bool)> = alive
        .chain(left)
        .filter(|(t, _)| t.cwd.as_deref().is_none_or(|c| c == thread.cwd))
        .filter(|(t, _)| thread.time_ms >= start_ms(t))
        .collect();
    let is_first = |t: &CodexProc| {
        t.resumes.is_none() && thread.time_ms.saturating_sub(start_ms(t)) <= FIRST_THREAD_WINDOW_MS
    };
    let firsts: Vec<(&CodexProc, bool)> =
        started.iter().copied().filter(|(t, _)| is_first(t)).collect();
    let candidates = if firsts.is_empty() { started } else { firsts };
    match candidates.as_slice() {
        [] => Verdict::Nobody,
        [(_, false)] => Verdict::Closed,
        [(one, true)] if one.cwd.is_some() => Verdict::Owner(one.pid),
        _ => Verdict::Ambiguous,
    }
}

/// True when the live processes with no known cwd are why a thread is in doubt: the same thread
/// is not in doubt among the others. A later scan may read their cwd, so this doubt is not final.
pub(crate) fn blind_rival_causes_doubt(
    thread: &Thread,
    procs: &[CodexProc],
    gone: &[Gone],
) -> bool {
    let sighted: Vec<CodexProc> = procs.iter().filter(|p| p.cwd.is_some()).cloned().collect();
    // Skip the gone entries that a blind live process explains, as `judge` would with all of them.
    let gone: Vec<Gone> =
        gone.iter().filter(|g| !procs.iter().any(|p| same_process(p, &g.proc))).cloned().collect();
    sighted.len() < procs.len() && judge(thread, &sighted, &gone) != Verdict::Ambiguous
}

/// True when no live process can take a thread that `gone` made, so `gone` can be forgotten. A live
/// process takes a thread of `gone` only if it started by `ended_ms` and its cwd is the same or
/// unknown. The list `procs` must not be empty, or the answer is no: a TUI that starts later may
/// still count. The floored start time can be up to a second early, so `gone` is kept for a second.
pub(crate) fn rivals_no_live_process(gone: &Gone, procs: &[CodexProc], now_ms: u64) -> bool {
    let cwd_may_match = |p: &CodexProc| match (&p.cwd, &gone.proc.cwd) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    !procs.is_empty()
        && now_ms >= gone.ended_ms.saturating_add(1000)
        && !procs.iter().any(|p| start_ms(p) <= gone.ended_ms && cwd_may_match(p))
}

/// `$CODEX_HOME`, else `~/.codex`, from stdusk's own environment. A pane's own environment is not
/// visible, so a pane that runs Codex with another `CODEX_HOME` is not tracked.
pub(crate) fn codex_home() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".codex");
    std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()).map_or_else(home, PathBuf::from)
}

/// The id in `resume <id>`. Only the word right before the id counts, so `codex fork <id>`, which
/// makes a thread of its own, is not a resume. `codex resume --last` and the `codex resume` picker
/// name no id, so they count as fresh TUIs: they get no id for the old thread they open.
pub(crate) fn resume_id(cmd: &[String]) -> Option<SessionId> {
    cmd.windows(2).find(|w| w[0] == "resume").and_then(|w| SessionId::parse(&w[1]))
}

/// Codex keeps its OS cwd when `--cd` selects another workspace. Relative paths are based on
/// that OS cwd, and arguments after `--` are prompt text.
pub(crate) fn working_dir(cmd: &[String], process_cwd: Option<&str>) -> Option<PathBuf> {
    let mut args = cmd.iter().skip(1);
    let mut selected = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--cd" || arg == "-C" {
            Some(args.next()?.as_str())
        } else {
            arg.strip_prefix("--cd=").or_else(|| {
                arg.strip_prefix("-C").map(|value| value.strip_prefix('=').unwrap_or(value))
            })
        };
        if let Some(value) = value {
            selected = Some(value);
        }
    }
    let path = PathBuf::from(selected.or(process_cwd)?);
    if path.is_absolute() {
        Some(path)
    } else {
        process_cwd.map(|cwd| PathBuf::from(cwd).join(path))
    }
}

/// True for `codex exec` (also `codex e`): a short non-interactive run. It is never a TUI, so it
/// makes no thread for a pane and must not be a rival. Its own threads are already refused by
/// originator (see `tui_user_thread_cwd` in `agent_codex_scan`). Found as `resume_id` finds
/// `resume`: a whole word, after the program name, so a global flag before it (`-c a=b exec`) does
/// not hide it. A prompt that is exactly `exec` or `e` would also match, which is far less likely
/// than the runs this removes from the rivals.
pub(crate) fn is_exec(cmd: &[String]) -> bool {
    cmd.iter().skip(1).any(|arg| arg == "exec" || arg == "e")
}

/// Shared by the tests of this file and of `agent_codex_scan`.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A base time in seconds, and helpers for ids and TUIs relative to it.
    pub(crate) const T0: u64 = 1_790_761_600;

    /// A v7 id created `ms` after `T0`, with `n` to tell two of them apart.
    pub(crate) fn id_at(ms: u64, n: u64) -> SessionId {
        let t = T0 * 1000 + ms;
        let text = format!("{:08x}-{:04x}-7{:03x}-8000-{:012x}", t >> 16, t & 0xffff, n, n);
        SessionId::parse(&text).expect("built as a canonical id")
    }

    pub(crate) fn thread(ms: u64, n: u64, cwd: &str) -> Thread {
        Thread {
            id: id_at(ms, n),
            cwd: cwd.into(),
            time_ms: T0 * 1000 + ms,
            rollout: format!("/r/rollout-{n}.jsonl"),
        }
    }

    /// A TUI that started `secs` after `T0`.
    pub(crate) fn tui(pid: u32, secs: u64, cwd: &str) -> CodexProc {
        CodexProc { pid, start_secs: T0 + secs, cwd: Some(cwd.into()), resumes: None }
    }

    pub(crate) fn resumed(pid: u32, secs: u64, cwd: &str) -> CodexProc {
        CodexProc { resumes: Some(id_at(0, 99)), ..tui(pid, secs, cwd) }
    }

    pub(crate) fn blind(pid: u32, secs: u64) -> CodexProc {
        CodexProc { cwd: None, ..tui(pid, secs, "") }
    }

    pub(crate) const W: &str = "/work/a";
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn working_directory_flags_resolve_against_the_process_and_stop_at_prompt_text() {
        let run = |args: &[&str], cwd| {
            working_dir(&args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(), cwd)
        };
        assert_eq!(run(&["codex"], Some("/work")), Some("/work".into()));
        assert_eq!(run(&["codex", "-C=/other work"], None), Some("/other work".into()));
        assert_eq!(run(&["codex", "--cd", "/other"], None), Some("/other".into()));
        assert_eq!(run(&["codex", "--", "--cd=/other"], Some("/work")), Some("/work".into()));
        assert_eq!(run(&["codex", "--cd", "relative"], None), None);
        assert_eq!(run(&["codex", "--cd"], Some("/work")), None);
        assert_eq!(run(&["codex"], None), None);
    }

    #[test]
    fn a_thread_goes_to_the_one_tui_that_could_own_it_and_to_nobody_in_doubt() {
        use Verdict::{Ambiguous, Nobody, Owner};
        let cases: Vec<(&str, Thread, Vec<CodexProc>, Verdict)> = vec![
            ("no TUI at all", thread(5_300, 1, W), vec![], Nobody),
            // The probe: a thread appears 0.31 s to 0.33 s after its TUI starts (whole seconds here).
            ("first thread of a lone TUI", thread(5_320, 1, W), vec![tui(10, 5, W)], Owner(10)),
            ("a /new a long time later", thread(600_000, 1, W), vec![tui(10, 5, W)], Owner(10)),
            ("a thread from before the TUI", thread(3_000, 1, W), vec![tui(10, 5, W)], Nobody),
            ("another cwd", thread(5_320, 1, W), vec![tui(10, 5, "/work/b")], Nobody),
            // Two TUIs in one cwd.
            (
                "far apart: the first thread goes to the TUI whose start it follows",
                thread(15_320, 1, W),
                vec![tui(10, 5, W), tui(11, 15, W)],
                Owner(11),
            ),
            (
                "far apart: the earlier one keeps its own first thread",
                thread(5_320, 1, W),
                vec![tui(10, 5, W), tui(11, 15, W)],
                Owner(10),
            ),
            (
                "one second apart: both windows hold the thread",
                thread(6_320, 1, W),
                vec![tui(10, 5, W), tui(11, 6, W)],
                Ambiguous,
            ),
            ("the same second", thread(5_320, 1, W), vec![tui(10, 5, W), tui(11, 5, W)], Ambiguous),
            // A known wrong pick. The first run on a machine has a fresh CODEX_HOME, and Codex
            // needs 3.2 s to 3.6 s for the first thread (Codex 0.159.2). It is outside the 2 s
            // window of TUI 10, and inside that of TUI 11, which started 3 s later in the same cwd.
            (
                "fresh CODEX_HOME: a slow first thread goes to a TUI that started later",
                thread(3_600, 1, W),
                vec![tui(10, 0, W), tui(11, 3, W)],
                Owner(11),
            ),
            (
                "just past the window of the earlier one",
                thread(7_400, 1, W), // 2.4 s after TUI 10, 0.4 s after TUI 11
                vec![tui(10, 5, W), tui(11, 7, W)],
                Owner(11),
            ),
            (
                "at the edge of the window of the earlier one",
                thread(7_000, 1, W), // exactly 2 s after TUI 10, the second TUI 11 starts
                vec![tui(10, 5, W), tui(11, 7, W)],
                Ambiguous,
            ),
            (
                "a later thread, two TUIs started before it",
                thread(600_000, 1, W),
                vec![tui(10, 5, W), tui(11, 100, W)],
                Ambiguous,
            ),
            (
                "a later thread, the second TUI started after it",
                thread(60_000, 1, W),
                vec![tui(10, 5, W), tui(11, 100, W)],
                Owner(10),
            ),
            (
                "a later thread and a TUI in another cwd",
                thread(600_000, 1, W),
                vec![tui(10, 5, W), tui(11, 100, "/work/b")],
                Owner(10),
            ),
            // A resumed TUI makes no thread at its start.
            (
                "a resumed TUI does not own a first thread beside a fresh one",
                thread(5_320, 1, W),
                vec![resumed(10, 5, W), tui(11, 5, W)],
                Owner(11),
            ),
            (
                "a lone resumed TUI owns its /new",
                thread(9_000, 1, W),
                vec![resumed(10, 5, W)],
                Owner(10),
            ),
            (
                "a later thread, a resumed TUI and a fresh one",
                thread(600_000, 1, W),
                vec![resumed(10, 5, W), tui(11, 6, W)],
                Ambiguous,
            ),
            // A TUI whose cwd the OS would not say could be anywhere.
            (
                "a TUI with no cwd is not given a thread",
                thread(5_320, 1, W),
                vec![blind(10, 5)],
                Ambiguous,
            ),
            (
                "a TUI with no cwd blocks a match",
                thread(5_320, 1, W),
                vec![tui(10, 5, W), blind(11, 5)],
                Ambiguous,
            ),
        ];
        for (why, thread, procs, want) in cases {
            assert_eq!(judge(&thread, &procs, &[]), want, "{why}");
        }
    }

    #[test]
    fn a_gone_tui_is_a_rival_only_for_threads_made_while_it_lived() {
        use Verdict::{Ambiguous, Closed, Owner};
        let gone =
            |proc: CodexProc, ended_secs: u64| Gone { proc, ended_ms: (T0 + ended_secs) * 1000 };
        let cases = [
            (
                "a /new of the gone TUI: the live one must not take it",
                thread(240_000, 1, W),
                vec![tui(11, 105, W)],
                vec![gone(tui(10, 5, W), 250)],
                Ambiguous,
            ),
            (
                "a thread made after it left is not its thread",
                thread(300_000, 1, W),
                vec![tui(11, 105, W)],
                vec![gone(tui(10, 5, W), 250)],
                Owner(11),
            ),
            (
                "only the gone TUI could own it",
                thread(240_000, 1, W),
                vec![tui(11, 260, W)],
                vec![gone(tui(10, 5, W), 250)],
                Closed,
            ),
            (
                "the first thread of the gone TUI, with a live TUI started beside it",
                thread(5_320, 1, W),
                vec![tui(11, 5, W)],
                vec![gone(tui(10, 5, W), 250)],
                Ambiguous,
            ),
            (
                "another cwd",
                thread(240_000, 1, W),
                vec![tui(11, 105, W)],
                vec![gone(tui(10, 5, "/work/b"), 250)],
                Owner(11),
            ),
            (
                "the gone TUI started after the thread",
                thread(3_000, 1, W),
                vec![tui(11, 1, W)],
                vec![gone(tui(10, 5, W), 250)],
                Owner(11),
            ),
        ];
        for (why, thread, procs, gone, want) in cases {
            assert_eq!(judge(&thread, &procs, &gone), want, "{why}");
        }
    }

    #[test]
    fn the_probe_run_of_four_tuis_gets_four_matches() {
        // Codex 0.159.2, warm daemon, four TUIs (two in workA started 8 s apart, one in workB).
        // The thread times are the UUIDv7 times of the real ids. The starts are the spawn times
        // floored to whole seconds, as sysinfo reports them.
        let real = |ms: u64, id: &str, cwd: &str| Thread {
            id: SessionId::parse(id).unwrap(),
            cwd: cwd.into(),
            time_ms: ms,
            rollout: String::new(),
        };
        let at = |pid, start_secs, cwd: &str| CodexProc {
            pid,
            start_secs,
            cwd: Some(cwd.into()),
            resumes: None,
        };
        let procs = [
            at(1, 1_790_761_619, "/w/a"),
            at(2, 1_790_761_623, "/w/a"),
            at(3, 1_790_761_627, "/w/b"),
            at(4, 1_790_761_631, "/w/a"),
        ];
        let threads = [
            (real(1_790_761_619_402, "01a0f1b5-cfca-7e30-a7aa-f5af4aa03f35", "/w/a"), 1),
            (real(1_790_761_624_012, "01a0f1b5-e1cc-70b3-9980-45a40737d002", "/w/a"), 2),
            (real(1_790_761_628_022, "01a0f1b5-f176-7d22-b96a-e488a45c6bd6", "/w/b"), 3),
            (real(1_790_761_632_217, "01a0f1b6-01d9-7c41-8609-653ba69ecc72", "/w/a"), 4),
        ];
        for (thread, pid) in threads {
            assert_eq!(judge(&thread, &procs, &[]), Verdict::Owner(pid), "{}", thread.id.as_str());
        }
    }

    #[test]
    fn a_resume_id_is_only_the_word_after_resume() {
        let id = id_at(0, 1);
        let argv = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let cases = [
            (format!("codex resume {}", id.as_str()), Some(&id)),
            (format!("node /x/bin/codex.js resume {}", id.as_str()), Some(&id)),
            (format!("codex -c a=b resume {}", id.as_str()), Some(&id)),
            (format!("codex fork {}", id.as_str()), None),
            (format!("codex {}", id.as_str()), None),
            ("codex resume --last".to_owned(), None),
            ("codex resume".to_owned(), None),
            ("codex".to_owned(), None),
            (format!("codex resume {}", id.as_str().to_uppercase()), None),
        ];
        for (line, want) in cases {
            assert_eq!(resume_id(&argv(&line)).as_ref(), want, "{line}");
        }
    }

    #[test]
    fn a_codex_exec_command_line_is_found_after_global_flags() {
        let argv = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let cases = [
            ("codex exec fix the bug", true),
            ("/h/bin/codex exec --json hi", true),
            ("codex -c model=o3 exec hi", true),
            ("codex --model gpt-5 exec hi", true),
            ("node /x/bin/codex.js exec hi", true),
            ("codex e hi", true),
            ("codex", false),
            ("codex fixexec", false),
            ("codex --model execute", false),
            ("codex resume", false),
            ("exec", false),
        ];
        for (line, want) in cases {
            assert_eq!(is_exec(&argv(line)), want, "{line}");
        }
    }

    #[test]
    fn a_gone_tui_is_forgotten_only_when_no_live_tui_can_compete_with_it() {
        let gone = |cwd: Option<&str>| Gone {
            proc: CodexProc { cwd: cwd.map(str::to_owned), ..tui(10, 100, W) },
            ended_ms: (T0 + 150) * 1000,
        };
        let now = (T0 + 160) * 1000;
        let cases = [
            (
                "a live TUI of the cwd that started before it left",
                gone(Some(W)),
                vec![tui(11, 5, W)],
                now,
                false,
            ),
            ("that TUI started after it left", gone(Some(W)), vec![tui(11, 155, W)], now, true),
            ("another cwd", gone(Some(W)), vec![tui(11, 5, "/work/b")], now, true),
            ("a live TUI with no cwd", gone(Some(W)), vec![blind(11, 5)], now, false),
            ("a gone TUI with no cwd", gone(None), vec![tui(11, 5, "/work/b")], now, false),
            ("no live TUI at all", gone(Some(W)), vec![], now, false),
            (
                "less than a second after it left",
                gone(Some(W)),
                vec![tui(11, 155, W)],
                (T0 + 150) * 1000 + 999,
                false,
            ),
        ];
        for (why, gone, procs, now_ms, want) in cases {
            assert_eq!(rivals_no_live_process(&gone, &procs, now_ms), want, "{why}");
        }
    }
}
