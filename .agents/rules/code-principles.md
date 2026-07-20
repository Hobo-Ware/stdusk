---
trigger: glob
globs: '**'
description: 'Rust discipline: immutability, illegal-states-as-enums, ownership, Option/Result over panic.'
applyTo: '**'
---

# Code Principles

Language-level discipline for every `.rs` file. These are rules, not suggestions.

## Immutability first

**Prefer `let` over `let mut`.** Introduce `mut` only where the algorithm genuinely
accumulates (parser `carry`, an events `Vec`). Clippy flags unused `mut`; keep it clean.

**Prefer immutable data flow.** Pass `&T`; compute new values rather than mutating in
place when the cost is trivial. Reserve `&mut` for real state transitions.

## Make illegal states unrepresentable

This crate's strongest habit - extend it. Model mutually-exclusive states as enums, not
loose `bool` + `Option` combinations.

**Bad:** permits nonsense like `{ active: false, pct: Some(80) }`
```rust
struct P { active: bool, error: bool, pct: Option<u8> }
```
**Good:** the type rejects the nonsense (this is what `Progress` already does)
```rust
enum Progress { None, Normal(u8), Error(u8), Indeterminate, Paused(u8) }
```

**Use newtypes when a bare pair is easy to transpose.** `start_selection(line, col)` takes
two integers in a fixed order; a `struct GridPoint { line: i32, col: usize }` turns a
transposition into a compile error. Adopt when the coordinate flows through 3+ call sites.

## Errors, not panics

**Malformed input is expected input.** Parsers return `Option`/`Result` on bad bytes -
never `panic!`/`todo!`/`unreachable!` on anything a shell or config file can produce.
`parse_osc`/`parse_code` returning `None` is the model.

**Panic only on violated startup invariants** the user cannot cause (`openpty`,
`spawn shell`). Every `expect` gets a message that reads as "this can't happen because…".

**Handle `Option`/`Result` explicitly** with `?`, combinators (`map`/`and_then`/
`unwrap_or`), or a guard clause + early return. Avoid deep nesting.

**Bad:**
```rust
if let Some(x) = a { if let Some(y) = x.b() { do(y) } }
```
**Good:**
```rust
let Some(y) = a.and_then(|x| x.b()) else { return };
do(y);
```

## Ownership & safety

**`unsafe_code = "deny"`.** The crate is unsafe-free except one isolated, single-threaded,
commented `set_var` in `main` (edition-2024 made it `unsafe`), which carries a local
`#[allow(unsafe_code)]`. Add no other `unsafe`.

**Keep lock scopes grab-copy-drop.** Lock, copy out a value, drop the guard at the `;`.
Never call UI code or hold across an `await` while locked. Never nest two locks without a
documented order.

## Quick checklist

- [ ] `let` unless accumulation genuinely needs `mut`.
- [ ] Mutually-exclusive state is an enum, not `bool` + `Option`.
- [ ] No `panic`/`unwrap`/`expect` reachable from parser or config input.
- [ ] Every `expect` has a "can't happen because…" message.
- [ ] Lock scopes are grab-copy-drop; no nested locks.
