---
trigger: glob
globs: '**'
description: 'Project overview: stack, module layout, commands, commit standards, platform targets.'
applyTo: '**'
---

# stdusk - Project Overview

Native quake-style terminal emulator in Rust. Began as a hard fork of
Eugeny/tabby; now a full Rust rewrite that IS the whole repo - the crate lives at
the root (`Cargo.toml`, `src/` at top level) on `main`. The Electron Tabby source
was removed from the tree (1.0.9); upstream Eugeny/tabby is the reference. Ports
Tabby's daily-driver experience at ~99% fidelity (progress-on-tabs, quake
drop-down, theming, tab management) plus a first-party AI agent Tabby lacks.

## Stack

- `eframe`/`egui` 0.35 (glow backend; `__screenshot` feature for the visual-test harness)
- `alacritty_terminal` 0.26 - the VTE engine + grid + scrollback
- `portable-pty` 0.9 - shell process + pty
- `global-hotkey` 0.8 - Carbon-API global hotkey (no macOS Accessibility grant)
- `serde` + `toml` - config; `regex` - progress scrape; `base64` - OSC 52
- Edition 2024, a Cargo workspace: the `stdusk` binary at the root plus two library crates.

## Module layout

Modules below that hold no egui code (`terminal`, `osc`, `progress`, `modes`, `screen`, `search`,
`links`, `paste`, `shell`, `pane`, `colors`, `themes`, `paths`, `procwatch`, `handoff`, the `agent*` files, `test_support`) live in
`crates/stdusk-core/src/`. The egui renderers and widgets (`fonts`, `widgets`, `mouse`, `key_to_bytes`,
`render_grid`, `collect_input`, `draw_tab`, `terminal_view`, `wheel`, `focus`, `macos_paste`) live in `crates/stdusk-egui/src/`. Their one-line
descriptions stay in the tree below.

```
src/
  agent_codex.rs Codex session capture, pure part: thread and process types, the timing match (`judge`), argv and env readers
  agent_codex_scan.rs Codex session capture, IO part: `CodexScanner`, rollout reads, the day-folder walk, scan memory
  agent_pane.rs  per-pane agent state: record, queued resume input, and who arms what
  agent_restore.rs restore plan: which saved sessions reopen, typed hint vs Enter, skip reasons, the toast
  agent_track.rs per-pane agent record state machine (Track::scan)
  agents.rs      agent session ids, the Claude registry parser, restore file checks
  main.rs        Stdusk app state + eframe::App loop (keybinds, quake window/hotkey, tray, session/CLI polling)
  macos.rs       objc2/AppKit glue: dock policy, unified titlebar, traffic-light centering, window alpha, Cmd+V clipboard probe, notifications
  fonts.rs       font resolution (core-text family lookup, bold face, Nerd Fonts) + egui FontDefinitions build
  tabs.rs        Tab model + spawn, tab-bar panel, tab menu, tab-management methods
  workspace.rs   central panel: pane tiling/render, input/paste routing, splitters, pane menu
  finder.rs      Cmd+F scrollback-search bar + multiline paste-confirm modal
  ui.rs          pure UI helpers extracted from the render loop (grid render, draw_tab, pos_to_cell)
  widgets.rs     design-system egui primitives (text_field, num_field, slider, chip, toggle_switch, focus_ring, icon buttons)
  settings.rs    settings view and reapply_config (runtime theme/hotkey/font; callers own edit baselines)
  tidewatch.rs   "Move tabs to Tidewatch": find the Tidewatch app, launch it with the socket, send every live pane as a `handoff::export::Export`
  handoff.rs     app half of the session handoff: launch the successor, snapshot tabs, instance-socket flag (the transport is `stdusk_core::handoff`)
  keys.rs        keyboard input encoding (key_to_bytes, ctrl/alt sequences, hotkey parse/match)
  terminal.rs    PtyTerm: pty spawn, reader thread, alacritty Term, grid snapshot, selection
  mouse.rs       SGR 1006 mouse reporting (wheel/click encoding, drag autoscroll) gated on TermMode
  terminal_view.rs (stdusk-egui) `TerminalView`: one pane's keys, pastes, wheel, pointer reports, focus lock and copy around `render_grid`; terminal input behavior goes here, never into an app
  wheel.rs       (stdusk-egui) wheel routing: scrollback, arrows (DECCKM aware, Alt forces them), SGR reports
  focus.rs       (stdusk-egui) the grid as keyboard target: Tab, arrows and Esc reach the pty
  macos_paste.rs (stdusk-egui) the Cmd+V image-paste NSEvent monitor and its request counter
  paste.rs       (stdusk-core) paste preparation: CR line breaks, control bytes dropped, Tabby trim rules
  pane.rs        binary split tree: layout, focus paths, splitters, neighbor navigation
  config.rs      TOML config + hotkey string parsing
  colors.rs      Theme + explicit-theme alacritty Color -> Color32 + derived chrome colors
  themes.rs      community XRDB color schemes (embedded pack + user files)
  progress.rs    ProgressScanner: Tabby's %-regex scrape (alt-screen guarded)
  osc.rs         OscScanner: OSC 7/1337/52/9;4 framing across chunk boundaries
  modes.rs       DEC mode 2031 tracking + dark/light theme reports (apps repaint on theme change)
  handoff.rs     (stdusk-core) live pty handover transport: wire format, `PaneMeta` + opaque `extra`, `Layout`, capture/adopt, fallback
  handoff/export.rs (stdusk-core) `Export` / `ExportPane`: the cross-app header for moving panes to another app (Tidewatch)
  screen.rs      grid -> ANSI screen dump, replayed into a handed-over pane (session handoff)
  search.rs      scrollback search: match finding + options
  links.rs       URL/path detection for clickable links
  session.rs     session save/restore (tabs, cwd, title, color)
  shell.rs       login+interactive shell launch, shell-integration injection and POSIX single_quote
  procwatch.rs   AI-CLI process detection for tab badges
  repo.rs        repo groups: git-root detection, the repo chip, repo switching (appearance.group_by_repo)
  tray.rs        macOS menu-bar status item
  test_support.rs shared test-only PTY options and isolated ShellFixture directories with Drop cleanup
```

State-of-the-work lives in `LEDGER.md` (what's built) and `PLAN.md` (architecture +
roadmap). Read both before starting; update `LEDGER.md` after every milestone.

## Commands

```
# crate is at the repo root (native/ promoted in 1.0.9)
cargo build 2>build.log; echo $?        # check the REAL exit code, never `| tail`
cargo test --workspace                  # unit + proptest
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run                               # opens the GUI (needs the user's aqua session)
cargo run -- --screenshot /tmp/x.png    # visual-test harness: demo tabs -> PNG -> exit
STDUSK_BACKGROUND=1 cargo run           # any dev run without stealing focus (--screenshot does this itself)
```

The screenshot harness (`--screenshot`) renders representative demo tabs and saves a
PNG so UI changes are self-verified without a user round-trip. See `ui.md`.

## Showcase page

`site/index.html` (deployed to stdusk.hoboware.dev on push to `main`) and the README's
"What it does" list are the public face. When work is done, check both against what shipped:

- A user-visible feature, a new default hotkey, or a changed count (color schemes, detected
  AI CLIs) gets added or corrected there, in the same branch.
- Removed or renamed behavior comes off; never leave a claim the app no longer backs.
- The nav version pill tracks `Cargo.toml`'s version on every release bump.
- Screenshots come from the `--screenshot` harness run under an isolated `--state-dir`, never a
  personal session, and are compressed per the global PNG rules before committing.
- Internal refactors, tests and fixes to things the page never mentioned need no change - say so
  in the wrap-up instead of skipping silently.

## Commit standards

- Conventional Commits, scoped `(native)`: `feat(native): ...`, `fix(native): ...`,
  `test(native): ...`, `docs(native): ...`, `chore(native): ...`, `refactor(native): ...`.
- Clean author line: NO `Co-Authored-By` trailer, NO "Generated with" footer.
- No em-dashes / en-dashes in commit messages or any GitHub prose - plain hyphens.
- Commit/push only when asked. One logical change per commit.

## Platform

macOS is the primary target (quake hide trick, Carbon hotkey, natural-editing keybinds
are macOS-tuned). Linux builds and passes CI (`check-linux`); the macOS job stays the required
check. macOS-only code lives in `src/macos.rs` (stubs elsewhere) and macOS-only crates under
`[target.'cfg(target_os = "macos")'.dependencies]`. A Linux change must keep the macOS path
byte-for-byte as it was: gate with `cfg`, never edit the macOS arm. Linux has no tray, login
item, self-update or default app shortcuts yet; `stdusk --toggle` replaces the global hotkey
where none exists (Wayland). Windows is not built.

## Quick checklist

- [ ] Read `LEDGER.md` + `PLAN.md` before coding; update `LEDGER.md` after.
- [ ] Showcase check: `site/index.html` + README feature list still match what shipped.
- [ ] New code lands in the module that owns its boundary (see layout).
- [ ] Build checked via real exit code, not a piped `tail`.
- [ ] Commit message is Conventional, scoped `(native)`, hyphens only, clean author.
