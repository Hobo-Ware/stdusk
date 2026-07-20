# Contributing to stdusk

stdusk is a native Rust quake terminal (egui + alacritty_terminal). The whole repo is the
crate: `Cargo.toml` and `src/` are at the root. macOS-only for now.

## Quick start

```sh
cargo run                 # launch the GUI (needs a display)
cargo test                # unit + real-pty + proptest
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

A change is mergeable when all four are green. CI enforces the last three on every PR; see
[PR gates](#pr-gates) below.

## Ground rules

- **Match the existing style.** Read the neighbouring code first. The design-system primitives
  live in `src/ui.rs` (`text_field`, `num_field`, `slider`, `chip`, `toggle_switch`,
  `scheme_dropdown`, `focus_ring`, ...) - use them, don't reinvent.
- **Keep changes surgical.** Every changed line should trace to the PR's stated goal. Don't
  refactor or reformat adjacent code in the same PR.
- **Tests are the contract.** Bug fix -> add the regression test that reproduces then passes.
  New behaviour -> unit-test the pure logic; use the headless `Context::run_ui` pattern for
  interaction and the real-pty pattern for terminal behaviour (see `.agents/rules/testing.md`).
- **Update the docs you invalidate.** `LEDGER.md` is the living state; `PARITY.md` is the
  Tabby-vs-stdusk matrix; `V1.md` is scope; `FUTURE.md` is the idea backlog. If your change
  touches what they describe, update them in the same PR.
- **Scope discipline.** `V1.md` section (d) lists explicit non-goals (SSH/serial, plugins,
  sixel, multi-window, non-macOS, ...). Check there before building something large.

## Contributing with AI

This codebase is built to be AI-navigable. `AGENTS.md` (auto-loaded core rules), `CLAUDE.md`
(area -> rule-file router), and the `.agents/rules/*.md` files are the machine-readable context;
`LEDGER.md` and `PLAN.md` are the state and the intent. Point your agent at them.

### Seed prompt

Paste this at the start of an AI coding session to hydrate it:

```text
You are working on stdusk: a native macOS quake terminal in Rust (egui/eframe 0.35 +
alacritty_terminal 0.26 + portable-pty). The whole repo is the crate - Cargo.toml and src/
are at the repo root.

Before writing code, read, in this order:
  1. AGENTS.md and CLAUDE.md         - how the rule files are organized + the area router
  2. .agents/rules/project.md, code-principles.md, implementation.md, testing.md  - baseline
  3. LEDGER.md                       - current state, recent releases, and the "Gotchas"
                                       section (hard-won constraints - read it, don't
                                       rediscover them)
  4. PLAN.md                         - architecture + module layout + roadmap
Then, for the area you're touching, read the matching domain rule via CLAUDE.md's map
(ui.md / terminal.md / performance.md / platform.md).

House rules:
  - Match surrounding style; keep the diff surgical (every line traces to the task).
  - Reuse the design-system primitives in src/ui.rs; don't reinvent widgets.
  - Bug fix => add a failing regression test first. New behaviour => unit-test pure logic,
    and use the headless Context::run_ui pattern (interactions) or the real-pty pattern
    (terminal behaviour) - both are in .agents/rules/testing.md and existing #[cfg(test)].
  - Update LEDGER.md / PARITY.md / V1.md when your change invalidates them.
  - Respect V1.md's non-goals before building anything large.

Definition of done (CI enforces the last three on every PR):
  cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
State your assumptions and the plan before large changes; if two interpretations are
plausible, surface both instead of guessing.
```

The `--screenshot` and `--screenshot-settings` harnesses (see `.agents/rules/ui.md`) let an
agent verify UI changes headlessly - use them for anything visual.

## PR gates

Every pull request runs the **native (rust)** workflow: `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, and `cargo test --all-targets`. All three must pass before a PR
can merge into `main` (branch protection makes the check required). Clippy runs with
`-D warnings` and the crate opts into `pedantic`, so CI is stricter than a bare `cargo build` -
run clippy locally before pushing.

Release builds are cut separately by the `release (stdusk)` workflow on a `stdusk-v*` tag; you
don't need to touch it for a normal PR.

## License

By contributing you agree your work is licensed under the repo's [MIT license](./LICENSE).
stdusk began as a fork of [Tabby](https://github.com/Eugeny/tabby) (also MIT).
