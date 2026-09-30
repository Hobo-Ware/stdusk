# Agent session resume: final design and verified contracts

Date: 2026-09-30. Implemented on `feat/agent-session-resume`, based on main 1.8.0.
This document describes the current capture without hooks or wrappers. Section 8 records the
probe evidence and known limits; removed designs and review chronology are omitted.

## 1. Goal

Restore each pane's exact interactive Claude Code or Codex conversation after relaunch,
including a crash or OS restart. Never choose a conversation with `--continue` or `--last`.
The shell starts in the session directory and receives `claude --resume <id>` or
`codex resume <id>` at its first prompt.

## 2. User contract

- `[session] resume_agents = "auto" | "prefill" | "off"`, default `auto`, requires
  `session.restore`. Unknown values fall back to `auto`. Settings > Session exposes these modes.
- Auto types the exact resume command and Enter; prefill types it without Enter; off disables
  tracking and resume. `tabs::resume_mode` is the shared gate, including screenshot suppression.
- A crash-marked session always restores as a hint without Enter, even in auto mode.
- A failed restore leaves a shell, a pane notice, and one grouped startup toast.
- A live update handoff adopts processes and records; it does not run resume commands.
- Open at login is a macOS OS setting, not a value in stdusk's config.

## 3. Boundaries

No agent hooks, shell wrappers, agent config edits, pane tokens, or stdusk agent-record directory.
Agent commands, prompts and credentials are not stored. Only canonical UUIDs, agent kind,
absolute cwd, optional rollout path and crash state enter the session snapshot.

Capture runs on the process-scan thread, roughly once per second, when CLI badges or agent
tracking are enabled. File reads stay off the render loop. Quit/handoff take a fresh process
scan but reuse the last Codex match. stdusk's OSC shell integration remains the status source.

## 4. Implementation

### 4.1. Ownership

| Module | Responsibility |
|---|---|
| `agents.rs` | Session IDs, Claude registry, session values and filesystem checks |
| `agent_codex.rs` | Pure timing verdict (`judge`), thread and process types |
| `agent_codex_scan.rs` | Rollout scanner (`CodexScanner`), day-folder walk, scan memory |
| `agent_track.rs` | Per-pane record state machine (`Track::scan`) |
| `agent_pane.rs` | Record plus queued resume input (`AgentPane`) |
| `agent_restore.rs` | Restore decisions, duplicate claims, notices and toast |
| `procwatch.rs` | Process identity/tree, live agent observations |
| `terminal.rs`, `tabs.rs`, `main.rs` | PTY input/status, restore, scans, save/quit wiring |

### 4.2. Process identity

Find the nearest descendant agent under the pane shell, by depth then pid. Do not capture the
shell root after `exec claude`, or a deeper nested agent behind another agent. Accept native
`claude`/`codex`, or node/bun/deno scripts in the corresponding npm package or named in `bin`/`.bin`.
Exclude `--version`, `-V`, `--help`, `-h`. A direct same-kind native child of a launcher is supported.
Refresh executable and argv each scan; on macOS derive the name from the executable to survive exec.

### 4.3. Capture

**Claude:** read `$CLAUDE_CONFIG_DIR/sessions/<pid>.json`, otherwise `~/.claude`, for the agent
or its direct same-kind child. Require matching pid, interactive or absent `kind`, canonical
`sessionId`, absolute cwd without controls, and `startedAt` not older than process start (whole
seconds, `startedAt / 1000 >= start`, so a stale file of a reused pid fails).
The registry immediately reflects `/clear` and `/resume`. `procStart` is not parsed.

**Codex:** read `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`, otherwise
`~/.codex`. There is no exact daemon-to-TUI pid link. `codex resume <id>` in argv names an exact
session. In all other cases the code matches a rollout thread to the live Codex processes by cwd
and start time. The cwd comes from `--cd`/`-C` when supplied, resolved against the process cwd
and canonicalized like rollout metadata. Codex keeps its OS cwd when it uses this option.
`agent_codex::judge` holds the exact rules, and its table test is the reference.
Agent homes come from stdusk's environment, not the pane's environment. A `codex exec` run is
never a TUI, so the match ignores it. It is not a rival and it is not kept after it ends.

The procwatch scan thread scans the day directories newest first. It skips a directory whose
modification time is older than the oldest live process start. It stops at the first directory
whose name date is more than 2 days older than the UTC date of that start. The 2 days cover the
local date in the folder name. The stop uses date arithmetic only, with no local time call. An old
resumed thread uses the exact lookup. The scan cost is linear in the number of rollouts and also
in the number of day directories inside the name bound. A steady scan of 3000 rollouts costs
3.5 ms, with 1008 older day directories in the tree (`scan_cost`, run by hand). The scanner
caches successful first-line reads by path (1 MiB cap, normally about 20 KB) and retries failed
reads five times for partial writes. Exact resume paths use a cached full-tree lookup, including
misses. No live Codex process means no rollout work.

### 4.4. Per-pane lifetime

| State | Meaning and transition |
|---|---|
| `Waiting { crash_hint }` | Restored record; keeps waiting without an agent, binds to a named session or matching session ID in argv |
| `Adopted { missed }` | Handoff record; binds to the first same-kind agent, including a silent one |
| `Bound { pid, missed }` | Named session changes replace it; a silent same pid keeps it; another silent agent drops it |
| `Ended { pid, at, crash }` | First command status observed; the first scan taken after it decides whether that pid really ended |
| `Gone { scans_left }` | Two missing scans hide the record; three more give up, allowing a delayed crash status in between |
| `Crashed` | Keep the hint through later commands; a matching agent binds it, another agent drops it |

A crash-marked adopted record becomes Waiting. Waiting/Adopted drop at their first command
status if nothing bound; unrelated silent agents drop waiting/crashed records. A quick restart
of the same session stays tracked. Scans taken at or before a status cannot settle it.

Only `133;D;<code>;stdusk` counts as a status; foreign marks still drive the tab dot.
The pty reader thread keeps the first status with an arrival timestamp until the egui frame in
`main.rs` or a procwatch scan step takes it. A status alone does not prove exit: Ctrl+Z, `bg`, `wait` and bash prompt Ctrl+C can report while the agent lives.
A later scan showing the same pid ignores the status; absence keeps a crash or clears a clean end.
Crash codes are 128 plus SIGILL, SIGTRAP, SIGABRT, SIGBUS, SIGFPE, SIGKILL or SIGSEGV;
SIGBUS is 10 on macOS and 7 elsewhere. Interrupt, hangup, termination and stop are not crashes.

### 4.5. Snapshot and durability

`SavedPane::Leaf.agent` holds `SavedAgent { kind, session, cwd, transcript?, crashed }`.
Malformed/unknown agents load as no agent; the old 1.4 `claude` field is accepted through an
alias and ignored. Unknown handoff `pane_token` fields also decode and are ignored.

**Downgrade limit:** 1.8.0 and older reject the new leaf `agent` field, start with defaults,
and overwrite the session file. A handoff to an older build aborts. This is an accepted tradeoff.

`SaveWriter` serializes writes: pid-specific temp file, `sync_all`, macOS `F_FULLFSYNC`, rename,
then best-effort directory sync. Queued snapshots coalesce to the latest and notify every waiter.
Autosave runs every 3 s and skips unchanged snapshots. Dead-pid temp files are removed on startup.

### 4.6. Restore and carried records

One `RestorePlan` checks each leaf: valid ID, existing cwd, existing transcript, then an unclaimed
`(kind, id)`. Claude can locate `projects/*/<id>.jsonl`; Codex needs its recorded rollout path.
Healthy records claim the ID. Crash hints do not claim it, but are skipped if already claimed
by an earlier healthy pane. A failed handoff adoption reports "shell not handed over".

Spawn the shell in the session cwd, never type a `cd`. Queue the command before spawn; the
reader consumes it once on an OSC 133 A only while the pane shell owns the PTY foreground
process group. A startup child's prompt mark leaves the command queued for the shell.
Integrated shells have no fallback. Other shells
get a 5 s fallback only when the PTY foreground process group is the shell; otherwise drop it
and show "shell busy at start". Any user input cancels pending typing.

Notices are chrome outside the grid and disappear on user input/click, not stdusk's typing.
The startup toast joins font/reattach messages and wraps. Up to three skips are named;
more are counted by reason. Crash-hint text says the command will be typed at the prompt.

### 4.7. Quit and OS termination

Settle records before the final snapshot, including pending statuses. Wait for the final
save even if unchanged, with a 2 s timeout logged to stderr; freeze autosaves, then kill panes.
A successful handoff freezes predecessor saves too. `App::on_exit` covers macOS termination.
Quit confirmation counts only healthy, filesystem-valid sessions that auto-resume; none for handoff.

### 4.8. Open at login

Show only for a macOS `.app` where `AnyClass::get(c"SMAppService")` succeeds (macOS 13+).
Guard every typed API entry point. The OS supplies status; refresh at most once per second
while Session settings are visible, and invalidate on section entry/actions. RequiresApproval
opens Login Items settings; action errors become toasts. Do not store a parallel config flag.

## 5. Input and filesystem safety

IDs must be canonical UUIDs before command construction or path lookup. Cwd is absolute and
free of controls. Resume input is one generated command, consumed once. Bounded rollout metadata
reads do not persist prompts/base instructions. Capture neither modifies agent config nor
injects environment into agent launches.

## 6. Validation coverage

Keep parser, identity/tree, timing verdict, state-transition, duplicate-claim, file-check,
config and serialization cases. Real PTYs cover first-prompt typing, idle/busy fallback,
user cancellation, zsh/bash status marks, foreign marks, Ctrl+Z/fg, crash vs clean exit,
final-save status application and capture under installed sh/bash/zsh/fish.
Headless egui covers settings/notice behavior; filesystem tests cover durable/coalesced saves.
Use `cargo +1.98.1` with offline fmt, Clippy (`--all-targets -- -D warnings`), build and tests.
Unix-socket handoff tests need a test run outside the filesystem sandbox.

## 7. Live acceptance

Passed on commit 2364ff5 with real Claude 2.1.285 and Codex 0.159.2, fake keys, an isolated
`--state-dir` window, and the macOS quit AppleEvent (the terminate action that the winit menu
binds to Cmd+Q, no key press sent):

- Claude flow: two tabs plus a split with `/clear` in one, then Cmd+Q and relaunch. Each pane
  resumed its own ID, and the cleared pane resumed its post-`/clear` ID.
- Two Codex panes in one repo started 3 s apart. Each resumed the thread its own TUI created.
- Two Codex TUIs started in the same second saved no ID for either pane.
- Crash hint: an agent crash followed by a quit within 1 s still saved the crash hint.
- Codex filter, on commit 92a97f5 (2364ff5 for the other checks): `/new`, a sub-agent, `/review`
  and `codex exec` each left the pane on the right thread.

Follow-up acceptance on 2026-09-30: the
[E2E receipt](../2026-09-30-agent-resume-e2e-results.md) records real Claude/Codex restore,
crash recovery, settings modes, and on-screen failure toasts and pane notices. The user
subsequently confirmed Open at login registration, de-registration, and successful restore
after a real macOS reboot/login. The user's confirmation does not specify bundle signing
identity and is separate from the isolated run's automated evidence.

The latest isolated run did not repeat live update handoff with real conversations, the
live `/new` ambiguity scenario, or GUI acceptance of the final Codex-exec filter; retain the
earlier probe and unit/PTY coverage as such. The optional
[VM plan](../plans/2026-09-30-macos-vm-e2e-plan.md) was not executed.

## 8. Verified CLI contracts and known limits

### Probe evidence (2026-09-30)

Isolated agent homes, disposable cwd, fake keys and real PTYs; requests ended in 401,
with no successful model call. Current capture probe: Claude 2.1.285 and Codex 0.159.2
(`probe-hookless/report.md`, scratch artifact, not checked in).

- Claude writes its registry before the first prompt, including `--resume`/`--continue`.
  `/clear` changes its ID immediately; `/resume` follows the chosen ID. Normal exit removes
  the file, kill -9 leaves it. `startedAt` is milliseconds and `procStart` the same time in UTC.
- Codex 0.159.2 uses one shared app-server per home; the daemon owns rollouts/locks. Locks
  neither identify TUI cwd nor reliably indicate live threads. Its first TUI environment
  also reaches later hooks, invalidating the earlier pane-token hook design.
- Codex rollout appears on the first turn. Its first `session_meta` line has ID, cwd,
  originator, thread_source and base instructions (about 20 KB). Exact resume was verified on 0.159.0 to open idle,
  display history and send no prompt. `/new` produces a later thread after a turn.
- Thread creation after TUI spawn: warm daemon +0.38 to +0.40 s; cold daemon (existing home)
  +0.45 to +0.66 s, at most 1.51 s against the floored start second; a fresh `CODEX_HOME`, first
  run ever, +3.2 to +3.6 s. sysinfo 0.38.4 reads whole-second `pbi_start_tvsec`; the match uses
  2 s. Four-TUI probe (two same-cwd starts 8 s apart) replayed in
  `the_probe_run_of_four_tuis_gets_four_matches`; a one-off scan of real rollouts agreed.
- First-line values, live with a mock server: TUI first thread and `/new` = `codex-tui` /
  `"vscode"` / `user`; `codex exec` = `codex_exec` / `"exec"` / `user`; sub-agent = `codex-tui` /
  `{"subagent":{"thread_spawn":…}}` / `subagent`; `/review` = `codex-tui` /
  `{"subagent":"review"}` / `subagent` (originator / source / thread_source).
- sysinfo source plus `sh -c 'sleep 1; exec sleep 8'` showed cached name/cmd across exec.
  Always refreshing exe/cmd fixed identity; measured scan cost about 10 ms for 900 processes.
- Published npm sources only (`npm pack`, no installation): Codex 0.158.0 `bin/codex.js`
  spawns native Codex with inherited stdio; Claude 2.1.283 replaces `bin/claude.exe` with the
  native binary at postinstall, with `cli-wrapper.cjs` spawning a child as fallback.
  Process-tree tests cover these shapes; live npm launches were not checked.
- Measured saves took 5–9 ms; synchronous quit/handoff process scans 13–25 ms on the dev Mac.
  A read-only ServiceManagement smoke test passed; no login item was registered in that probe. macOS 11–12
  availability handling has tests but no live verification.

### Known limits

- **Codex ownership is a heuristic on undocumented internals, without a version gate.**
  Changes to daemon/layout/UUIDs/metadata can lose capture or select a wrong conversation;
  the user accepted that rare risk.
- Sub-agent, `/review` and `codex exec` threads are filtered by originator and thread_source.
  An IDE or app thread that reports the TUI originator can still look like `/new` of the sole
  TUI. Verdict memory resets on handoff, allowing an old closed TUI's `/new` to be reassigned.
  The list of closed TUIs also resets on handoff. An empty process list clears the verdicts but
  keeps the list of closed TUIs.
- A TUI that starts and ends between two scans (shorter than 1 s) is never seen. Its late
  thread can go to a live pane in the same cwd.
- A `/new` in a live pane, made within one scan after another TUI of the same cwd closed, stays
  in doubt. The pane keeps its old thread.
- A gap between scans (a paused process, a sleeping Mac) delays the match. App Nap was not
  verified.
- A scan that misses a live TUI once counts it as closed at once. Its `/new` in that second
  can stay closed, and a closed rival can be forgotten too early. No cause of such a miss is
  known.
- More than 64 closed TUIs in one cwd while an idle TUI waits there evict the oldest. The idle
  TUI's first thread then stays in doubt. `codex exec` runs do not count, so this needs many
  real TUIs.
- Two same-cwd TUIs starting within the window, or an unknown cwd, leave no match. A `/new`
  while two TUIs share a cwd is in doubt: the pane keeps the conversation it had before the
  `/new`, not a plain shell, because a bound pane retains its last ID through a silent scan.
- A first-ever Codex run (fresh `CODEX_HOME`) creates its first thread +3.2 to +3.6 s after
  start. A second TUI in the same cwd started inside that gap takes it, and the first pane gets
  no ID for it (a `judge` table row). The window stays 2 s, because a wider one makes normal
  quick pairs ambiguous.
- `codex resume --last` and the picker count as fresh TUIs. They get no ID for the old thread
  they open, and may take a sibling's first thread inside 2 s.
- Two agents of one kind at one depth in a pane: the one in the tty foreground group wins,
  otherwise the lowest pid. Not verified on a real stopped job.
- Fresh Codex has no capture before its first turn. `/new` without a turn keeps the old ID;
  in-TUI `/resume` and `/fork` may also keep it (unprobed on 0.159.2).
- Agent-home overrides made only inside a pane are invisible. `exec claude` is deliberately
  not captured. Shells without zsh/bash integration cannot mark crashes and use the miss rule.
- Compound commands, pipelines and functions can hide the agent's crash behind another status.
  Bash `ignoredups` with the same status loses a mark; `ignorespace`, history off or HISTSIZE=0
  leave only status changes. Shared-history PROMPT_COMMAND can emit stale marks, and preceding
  PROMPT_COMMAND actions can clobber `$?`. There is deliberately no DEBUG trap.
- Only the first status between two takes counts. The egui frame in `main.rs` takes each status,
  so Ctrl+Z then a crash inside one frame loses the crash. A hidden window still runs frames, so
  this also holds while hidden. The record ends without auto-resume.
- User input cancels pending typing but the record stays until the next command. A cleared
  zsh hint returns next launch if no command ran; bash prompt Ctrl+C drops it. Resume failure
  inside the agent produces no stdusk toast and leaves a waiting record until the next command.
- A user `.zshenv` that redirects ZDOTDIR can bypass stdusk's generated `.zshrc`: no first-prompt
  OSC mark arrives, while assumed integration disables fallback. This review finding remains open.
- OS termination saves through on_exit but cannot show quit confirmation; Cmd+Q routing depends
  on the winit menu/accessory policy. Reboot/login restore is user-confirmed; a separate
  logout/login flow was not recorded.
- `sudo -E claude` and `sudo -E codex` run under another uid. The scan does not track them.
- A wall-clock step (manual change, NTP jump or sleep) can make the time comparisons misjudge.
  These compare process start, thread creation and status times.
- A `ZDOTDIR` that is a symlink to the shell-integration bridge dir is not seen as a bridge. The
  check reads the path text only, so the bridge rc files can source each other.
- Quit scan cost scales with process count and reuses the last Codex match, so a session started
  less than a second before quit may be absent. Downgrade compatibility is limited as in 4.5.
