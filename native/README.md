# stdusk

> the machine speaks back

A quake-style terminal with a **real GUI tab bar** - the thing text-grid terminals
(tmux, kitty, ghostty) physically can't do. Built in Rust for the efficiency an
Electron app can't match.

The name: `std*` (as in `stdin`/`stdout`/`stderr`) meets *dusk* - a terminal
stream at the faded end of the day. Revachol energy, no direct ripoff.

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
| First-party AI agent | `reqwest` → Anthropic Messages API (`claude-opus-4-8`) |

## Status

See [PLAN.md](./PLAN.md) for the full architecture, test strategy, and milestone breakdown.

- [x] **M0** - chrome: borderless quake window, chunky OneHalfDark tab bar, `+`/switch
- [x] **M1** - PTY: spawn the shell, render its grid, accept input
- [ ] **M1.5** - progress on tabs (%-regex + OSC 9;4) + cwd tracking
- [ ] **M2** - colored cell renderer + cursor
- [ ] **M3** - quake: configurable global hotkey (default Ctrl+\`) + hide-on-focus-loss
- [ ] **M4** - theming + config.toml (Tabby-default parity)
- [ ] **M5** - tab management: context menu, color coding, rename, reorder
- [ ] **M6** - resize + scrollback + copy/paste
- [ ] **M7** - scrollback search
- [ ] **M8** - split panes
- [ ] **M9** - shell integration (OSC 133) + exit-code state dot
- [ ] **M10** - first-party AI agent (read terminal, propose + run commands, gated)
- [ ] **M11** - polish + settings GUI

## Run

```
cd native
cargo run
```
