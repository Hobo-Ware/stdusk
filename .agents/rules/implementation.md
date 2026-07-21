---
trigger: glob
globs: '**'
description: 'Naming, visibility, module boundaries, doc comments, error policy, lints and formatting.'
applyTo: '**'
---

# Implementation Standards

## Naming

- `snake_case` functions/modules/vars, `CamelCase` types/traits, `SCREAMING_CASE` consts.
- Names carry meaning so comments don't have to. `take_clipboard` not `get_clip`.

## Visibility

**Default to `pub(crate)`, not `pub`.** This is a binary crate - nothing outside can call
these items, so bare `pub` is a lie about the API surface. Use bare `pub` only for the few
items genuinely re-exported.

**Bad** (reads as public API):
```rust
pub fn to_color32(c: Color) -> Color32 { ... }
```
**Good** (honest about scope):
```rust
pub(crate) fn to_color32(c: Color) -> Color32 { ... }
```

## Module boundaries

**A module earns its own file when it owns state + behavior other modules touch only
through a named API** (as `OscScanner` does) - not when it's "a bunch of functions."

**Split a file when it crosses ~1000 lines or mixes separable concerns.** `main.rs`
separates: the `eframe::App` loop, pure UI helpers (→ `ui.rs`), and window/hotkey plumbing.
Pure helpers move out first - they're the easiest to test and least coupled.

**No re-export barrel modules.** Don't add a module whose only job is to re-export siblings.

## Doc comments

- `//!` for the module-level "what is this file and why" header - every module has one; keep it.
- `///` for items; document the *why* and the invariant, not the mechanics.
- One tight line beats a paragraph. If the name + types say it, don't comment it.

## Error policy

**No `anyhow`/`thiserror` yet.** The crate has no public library API and almost no fallible
internal API. Introduce `anyhow` (app-glue) the moment a function returns `Result` with more
than one error source and wants `?` propagation; `thiserror` only if a library error enum
callers match on ever appears. Until then, `Option`/`None` + swallow-to-default is correct.

**Config errors swallow to defaults with a stderr note** - a terminal must start no matter what:
```rust
toml::from_str(&s).unwrap_or_else(|e| {
    eprintln!("stdusk: config parse error ({e}); using defaults");
    Self::default()
})
```

**`mutex.lock().unwrap()` is acceptable** - it panics only on poisoning (a thread panicked
mid-lock), and a poisoned terminal state is unrecoverable here. Leave a one-line
`// poisoned => reader thread died; nothing to recover` at the first such site.

## Ground work in real behavior, not assumptions

**Before implementing against an external tool / library / OS API, VERIFY its real behavior.**
Read the vendored source in `~/.cargo`, run the real CLI's `--help`, inspect a live
process/argv, read the actual crash log or the real data files on disk. Assuming the contract
and coding to it is the single biggest source of rework in this repo's history - **the wins
were grounded; the losses were assumed.**

Concrete misfires (all shipped, all had to be undone):

- `claude --resume` was assumed **global**; it is **CWD-scoped**, so a moved repo broke resume
  - the feature degraded, then was pulled entirely.
- The scheme-browser preview was assumed to apply the live contrast floor; it rendered **raw**
  theme colors, so "unreadable themes" recurred for releases until someone read the paint path.
- "Local GUI launch is impossible in this env" was assumed; the second instance was silently
  `return Ok(())`-ing on the single-instance guard - a window was launchable all along.
- egui-winit 0.35 was assumed to surface Cmd+C/X/V as key events; it **folds** them into
  `Event::{Copy,Cut,Paste}` and drops empty/image paste (found by reading `egui-winit/lib.rs`).

Wins came the same way: parsing the session id from the live process argv, reading the
egui-winit source, real-pty round-trips of the actual CLIs. When two behaviors are plausible,
go look - don't pick one silently.

## Lints & formatting

Formatting is pinned in `rustfmt.toml`; lints in `Cargo.toml` `[lints]` (crate-wide, the
edition-2021+ home - never scattered `#![deny]`). Policy:

- `clippy::all` = **deny** (real bugs). `clippy::pedantic` = **warn**, with a small,
  per-lint-justified allow-list (deliberate numeric casts, `module_name_repetitions`).
- `unsafe_code = "forbid"`.
- **Don't `deny(warnings)` locally or blanket-deny `pedantic`** - a compiler upgrade would
  break the build. CI escalates warnings to errors; local dev stays warn.

Before every PR: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.

## Quick checklist

- [ ] New items `pub(crate)` unless truly re-exported.
- [ ] File under ~1000 lines and single-concern; pure helpers live in `ui.rs`.
- [ ] `//!` header present; `///` states the why/invariant, tersely.
- [ ] External tool/lib/OS contract VERIFIED (vendored source / `--help` / live argv), not assumed.
- [ ] No new error-handling crates unless the multi-source-`Result` trigger is hit.
- [ ] `cargo fmt --check` + `cargo clippy -- -D warnings` clean.
