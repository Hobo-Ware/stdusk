# stdusk

> the machine speaks back

A quake-style terminal with a **real GUI tab bar** - the thing text-grid terminals
(tmux, kitty, ghostty) physically can't do. Built in Rust for the efficiency an
Electron app can't match.

The name: `std*` (as in `stdin`/`stdout`/`stderr`) meets *dusk* - a terminal
stream at the faded end of the day. Revachol energy, no direct ripoff.

This is the developer-facing doc. For the pitch, see the [repo README](../README.md).

## Why

The forked-from Electron Tabby (kept in this repo's `master` branch as reference)
nails sexy + quake but pays for it in RAM. Text-grid emulators are efficient but
render tabs as text, so they never look like a GUI. stdusk aims for both:

- **Sexy GUI tabs** - `egui` draws real pixel widgets, chunky, colored per-tab
- **Quake drop-down** - borderless `winit` window, top edge, global hotkey
- **Efficient** - `alacritty_terminal` engine + GPU render, no Chromium

## Stack

| Concern | Crate |
|---------|-------|
| Window + event loop | `eframe` (winit + wgpu) |
| GUI / tab bar | `egui` |
| Terminal engine (grid, PTY, VTE) | `alacritty_terminal` |
| Global hotkey (quake toggle) | `global-hotkey` |
| AI-CLI awareness (tab badges) | `sysinfo` (process-tree scan) |

## Status

See [PLAN.md](./PLAN.md) for the full architecture, test strategy, and milestone breakdown,
and [LEDGER.md](./LEDGER.md) for the running build state.

- [x] **M0** - chrome: borderless quake window, chunky OneHalfDark tab bar, `+`/switch
- [x] **M1** - PTY: spawn the shell, render its grid, accept input
- [x] **M1.5** - progress on tabs (%-regex + OSC 9;4) + cwd tracking
- [x] **M2** - colored cell renderer + cursor
- [x] **M3** - quake: configurable global hotkey (default Ctrl+\`) + hide-on-focus-loss
- [x] **M4** - theming + config.toml (Tabby-default parity)
- [x] **M5** - tab management: context menu, color coding, rename, reorder
- [x] **M6** - resize + scrollback + copy/paste + OSC 52
- [x] **M6.5** - mouse selection + Cmd+C copy
- [x] **M7** - scrollback search (regex/case/whole-word)
- [x] **M8** - split panes (tree, focus, drag-resize, per-pane pty)
- [x] **M9** - shell integration (OSC 133) + bell + cursor styles
- [x] **M10** - ambient AI-CLI awareness: badge tabs running claude/codex/gemini/copilot/aider/cursor/ollama
- [ ] **M11** - polish + settings GUI

> **Note on M10:** originally scoped as a first-party chat agent; dropped in favor of
> *awareness* (the CLIs' own UX is already supreme). `procwatch.rs` scans the process tree
> under each tab's shell (~1 Hz) and badges the tab with the brand color of any known AI CLI
> it finds. Toggle with `terminal.detect_clis`.

## Install

```sh
brew install hobo-ware/tap/stdusk
```

## Run from source

```sh
cd native
cargo run          # opens the GUI
cargo test         # unit + headless integration
cargo clippy --all-targets -- -D warnings
```

## Release

Tag `stdusk-v<version>` and push - CI builds a universal macOS binary, cuts the GitHub
Release, and generates the Homebrew formula. See [`packaging/README.md`](./packaging/README.md).
