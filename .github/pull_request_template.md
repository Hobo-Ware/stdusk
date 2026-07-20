## Description

<!-- What changed and why. Link any related issue. -->

...

## Checklist

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes (added a regression test for a fix / tests for new behaviour)
- [ ] The diff is surgical - every change traces to the description above
- [ ] Updated `LEDGER.md` / `PARITY.md` / `V1.md` if this change invalidates them
- [ ] Checked `V1.md` non-goals if this adds something large

## AI Usage

Choose the level of AI involvement for this PR.

* [ ] Fully vibe coded
* [ ] AI-designed, AI-coded, manually checked
* [ ] Human-designed, AI-coded
* [ ] Human-designed, human-coded (includes AI autocompletions and boilerplate gen)

*<sub>This is not to block AI contributions but rather to speed up PR review (saves time on trying to deduce the logic behind AI hallucinations). New to the codebase? `CONTRIBUTING.md` has a seed prompt that hydrates an AI session with the project's rules and gates.</sub>*
