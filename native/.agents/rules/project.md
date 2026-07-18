---
trigger: glob
globs: '**'
description: 'Project overview: stack, module layout, commands, commit standards, platform targets.'
applyTo: '**'
---

# stdusk - Project Overview

Native quake-style terminal emulator in Rust. A hard fork of Eugeny/tabby: the
Electron original lives on `master` as reference; this Rust rewrite lives on
branch `rust`, crate in `native/`. Ports Tabby's daily-driver experience at ~99%
fidelity (progress-on-tabs, quake drop-down, theming, tab management) plus a
first-party AI agent Tabby lacks.

## Stack

- `eframe`/`egui` 0.35 (glow backend; `__screenshot` feature for the visual-test harness)
- `alacritty_terminal` 0.26 - the VTE engine + grid + scrollback
- `portable-pty` 0.9 - shell process + pty
- `global-hotkey` 0.8 - Carbon-API global hotkey (no macOS Accessibility grant)
- `serde` + `toml` - config; `regex` - progress scrape; `base64` - OSC 52
- Edition 2024, single binary crate.

## Module layout

```
native/src/
  main.rs        Stdusk app state + eframe::App loop (keybinds, quake window/hotkey, tray, session/CLI polling)
  tabs.rs        Tab model + spawn, tab-bar panel, tab menu, tab-management methods
  workspace.rs   central panel: pane tiling/render, input/paste routing, splitters, pane menu
  finder.rs      Cmd+F scrollback-search bar + multiline paste-confirm modal
  ui.rs          pure UI helpers extracted from the render loop (testable)
  terminal.rs    PtyTerm: pty spawn, reader thread, alacritty Term, grid snapshot, selection
  pane.rs        binary split tree: layout, focus paths, splitters, neighbor navigation
  config.rs      TOML config + hotkey string parsing
  colors.rs      Theme + alacritty Color -> Color32 + derived chrome colors
  themes.rs      community XRDB color schemes (embedded pack + user files)
  progress.rs    ProgressScanner: Tabby's %-regex scrape (alt-screen guarded)
  osc.rs         OscScanner: OSC 7/1337/52/9;4 framing across chunk boundaries
  search.rs      scrollback search: match finding + options
  links.rs       URL/path detection for clickable links
  session.rs     session save/restore (tabs, cwd, title, color)
  shell.rs       login+interactive shell launch + shell-integration injection
  procwatch.rs   AI-CLI process detection for tab badges
  tray.rs        macOS menu-bar status item
```

State-of-the-work lives in `LEDGER.md` (what's built) and `PLAN.md` (architecture +
roadmap). Read both before starting; update `LEDGER.md` after every milestone.

## Commands

```
cd native
cargo build 2>build.log; echo $?        # check the REAL exit code, never `| tail`
cargo test                              # unit + proptest
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run                               # opens the GUI (needs the user's aqua session)
cargo run -- --screenshot /tmp/x.png    # visual-test harness: demo tabs -> PNG -> exit
```

The screenshot harness (`--screenshot`) renders representative demo tabs and saves a
PNG so UI changes are self-verified without a user round-trip. See `ui.md`.

## Commit standards

- Conventional Commits, scoped `(native)`: `feat(native): ...`, `fix(native): ...`,
  `test(native): ...`, `docs(native): ...`, `chore(native): ...`, `refactor(native): ...`.
- Clean author line: NO `Co-Authored-By` trailer, NO "Generated with" footer.
- No em-dashes / en-dashes in commit messages or any GitHub prose - plain hyphens.
- Commit/push only when asked. One logical change per commit.

## Platform

macOS is the primary target (quake hide trick, Carbon hotkey, natural-editing keybinds
are macOS-tuned). Keep Linux/Windows paths compiling but don't block on their polish.

## Quick checklist

- [ ] Read `LEDGER.md` + `PLAN.md` before coding; update `LEDGER.md` after.
- [ ] New code lands in the module that owns its boundary (see layout).
- [ ] Build checked via real exit code, not a piped `tail`.
- [ ] Commit message is Conventional, scoped `(native)`, hyphens only, clean author.
