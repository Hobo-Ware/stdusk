# Agent resume E2E results - 2026-09-30

No feature failure was found in the completed checks. Real Claude and Codex capture, exact-ID
restore, crash recovery, and split restore passed. All three restore settings passed. After
this isolated run, the user confirmed successful login-item registration and de-registration,
plus restore after a real macOS reboot/login (2026-09-30). Those manual confirmations are
separate from the automated evidence below.

## Build and isolation

- macOS 26.7.1, Rust 1.98.1, Codex 0.159.2, Claude Code 2.1.285.
- The GUI tests used a frozen source copy based on `73e4c6d`, with a SHA-256 source manifest.
  The final tree at `4948784` differs from that copy in `agent_codex.rs` and `procwatch.rs`
  (the additional exclusion of `codex exec` from competing processes). The full suite was
  rerun on `4948784`, including those tests. The GUI run does not validate that later filter.
- Separate ad-hoc-signed app bundles, disposable homes, explicit `--state-dir`, and a
  localhost mock Responses and Anthropic Messages APIs. Test conversations used the local mock
  provider, not a real model service.
- All evidence and the launch-matrix runner are under `/tmp/stdusk-e2e-ypwlw3nw`.
  That temporary directory is not a permanent repository artifact.

## Completed checks

| Check | Result and evidence |
| --- | --- |
| Full suite on final tree | 585 passed, 0 failed, 4 opt-in tests omitted by default; `current-test.log`. |
| Live shell handoff | Both opt-in tests passed: real successor adopted a shell and acknowledged it; real bundle launch delivered successor arguments. `handoff-live.log`. |
| Native login status | Opt-in read-only ServiceManagement test passed outside a bundle. The Session UI showed Open at login off in the test bundle. Registration and actual login were not exercised. |
| Codex scan cost | Opt-in release test passed: 3,000 rollout files plus three years of old day directories; steady-state scan 3.314 ms against a 100 ms bound. `scan-cost.log`. |
| Formatting and lint | `cargo +1.98.1 fmt --check` and offline Clippy with `--all-targets -- -D warnings`. |
| Launch matrix | Nine real-app launch scenarios passed using CLI stand-ins, real zsh prompts, persisted fixtures, and command/buffer logs. `restore-matrix-results.json`, `restore_matrix.py`, and `matrix/`. |
| Real Codex resume | A local-mock `codex exec` created an initial transcript. The GUI reopened that exact ID, displayed the prior turn, and completed another local-mock turn. Cmd+Q and relaunch restored both turns. |
| Fresh Codex capture | A second interactive Codex in the same directory produced its own transcript and distinct saved ID. A third interactive Codex in a split pane also received its own ID. |
| Crash detection | SIGKILL targeted only the verified second test TUI. Its saved record became `crashed = true`; the first pane's ID stayed unchanged. |
| Crash restore | After Cmd+Q and relaunch, the healthy session ran automatically. The killed session had a typed, unexecuted command and a visible crash notice. Enter restored its prior conversation; the crash flag cleared after binding. |
| Split restore | Three distinct records survived quit/relaunch across two tabs, one split. Both split histories were visibly correct. `codex-before-split-relaunch.toml`. |
| Type only through settings | Selected Type only, saved, quit, and relaunched. Each split pane contained its own resume command without executing it. |
| Off through settings | Selected Off, saved, quit, and relaunched. Tabs and split layout remained, with empty shell prompts and no agent running. |
| Startup messages | Renderer screenshots verified the grouped invalid/missing-file toast, duplicate toast, and crash toast. The persistent crash notice was separately verified in the real Codex GUI after the toast expired. |
| Real Claude capture and restore | Two real Claude 2.1.285 sessions in same-directory split panes completed local-mock turns and saved distinct IDs. Cmd+Q and relaunch restored both histories under their original IDs. The restored left conversation completed another turn. |
| Claude `/clear` | Changed only the left pane's registry and saved ID. The right pane kept its original ID. The new left session also restored after relaunch, before any further model turn. |
| Claude crash recovery | SIGKILL targeted the verified right Claude TUI. Only its record acquired `crashed = true`. On relaunch, left resumed automatically; right showed the exact prefilled command and persistent crash notice without executing. Enter restored the right conversation's history. |
| Claude clean exit | `/exit` returned to the shell and removed only the right pane's saved agent. The left Claude session remained recorded. |

The nine launch-matrix scenarios were Claude Auto, Codex Auto, Prefill, Off, crash under Auto,
duplicate records, invalid/missing records, a healthy duplicate following a crash hint, and mixed
Claude/Codex records. Valid commands ran exactly once in a working directory containing spaces
and a single quote. Invalid IDs, missing directories, missing transcripts, and an unknown agent
kind launched no CLI stand-in. These validate stdusk's launch path, not the real Claude CLI.

The three real Codex session IDs were:

- `01a0f2c7-8f71-7761-91ce-46f064676d85`: initial transcript, continued and restored.
- `01a0f2d4-9efb-7802-9b35-4a3405fc77ef`: fresh second tab, killed, restored by Enter.
- `01a0f2e4-f344-7e72-9d3f-4a47177589dd`: fresh split pane, restored independently.

Real Claude session IDs and retained evidence:

- `f254c03e-b079-4303-a19b-4d7ac72a5733`: left conversation, restored and continued.
- `757cc04b-506f-4adc-a7bf-22f02b5be8fb`: right conversation, restored after SIGKILL.
- `9d072b66-310c-4430-807c-eef3fdd7bc6a`: left conversation after `/clear`.
- `claude-before-relaunch.toml`, `claude-after-clear.toml`, `claude-after-crash.toml`,
  and `claude-after-clean-exit.toml` preserve the asserted state transitions.

## Limits and cleanup

### Subsequent manual acceptance

- The user confirmed Open at login registration and de-registration both work.
- The user confirmed successful app launch and conversation restore after a real macOS
  restart/login. No signing identity or new automated artifact was supplied with that result.
- These confirmations close those acceptance items. The native registration probe and
  first-registration fix are also recorded in `LEDGER.md`.

### Scope of the isolated run

- The first Claude attempt stopped at setup after an automatic approval-review rejection.
  On the requested retry, setup completed and the real-CLI checks above passed using the local
  mock provider. The suite's separate cross-shell capture tests still use process stand-ins and
  registry fixtures; real Claude GUI testing here used zsh only.
- UI automation temporarily ignored input, disconnected, and reported missing windows. A stack
  sample showed an idle app event loop, not a demonstrated application hang. Rebinding and
  retrying restored control; the Codex scenarios above then completed.
- This run did not exercise a real macOS reboot/login, login-item registration, a live update
  handoff with real agent conversations, or a live `/new` ambiguity scenario. Existing unit/PTY
  coverage and earlier acceptance notes must not be confused with fresh GUI acceptance here.
- The test app process trees were confirmed gone and the local mock server stopped. Artifacts
  were retained. No production source was changed, and no commits or pushes were made.
- This is validation documentation only; README and showcase claims need no change.
