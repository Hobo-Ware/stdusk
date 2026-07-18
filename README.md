<p align="center">
  <img src="native/assets/stdusk-logo.png" alt="stdusk" width="132" height="132">
</p>

<h1 align="center">stdusk</h1>

<p align="center"><em>the machine speaks back</em></p>

<p align="center">
A quake-style terminal with a <strong>real GUI tab bar</strong>. Native Rust, no Chromium, no apologies.<br>
It drops from the top edge on a keystroke, shows you what your machine is actually doing, and gets out of the way.
</p>

<p align="center">
By <a href="https://github.com/Hobo-Ware">Hobo-Ware</a> - tools for the discerning degenerate.
</p>

---

## The case

Text-grid terminals (tmux, kitty, ghostty) are efficient, but they render tabs as *text* - they can never look like a GUI. Electron terminals (the [Tabby](https://github.com/Eugeny/tabby) we forked) look gorgeous but bill you a few hundred megabytes of RAM for the privilege.

stdusk refuses the tradeoff. `egui` paints real pixel-perfect tabs. `alacritty_terminal` drives the grid on the GPU. The whole thing is one native binary that starts instantly and sits quiet until you summon it.

## Install

```sh
brew install hobo-ware/tap/stdusk
```

Lands in `/Applications` (Spotlight and Launchpad find it) and puts the `stdusk` CLI on your PATH. Then hit **Ctrl+`** to summon it - it drops from the top edge, no Dock icon, no clutter. Configurable - set it to F13 if you're fancy.

## What it does

- **Progress on tabs** - the crown jewel. apt, pip, npm, curl, your 3am migration script: if it prints `N%`, the tab wears a progress bar. You don't babysit it, you glance at it.
- **Ambient CLI awareness** - got a `claude`, a `gemini`, a `codex` running somewhere in your seven tabs? Each tab tells you which, in its brand colors. Know which one is the one thinking.
- **Real GUI tabs** - colored, renameable, reorderable, split-aware. Pixels, not ASCII art.
- **Quake drop-down** - borderless, top-edge, global hotkey, hide-on-blur. There when you call, gone when you don't.
- **Splits** - panes, drag to resize, a tiny live map of the layout drawn right on the tab.
- **Scrollback search** - Cmd+F, with regex, case, and whole-word toggles.
- **Command palette** - Cmd+Shift+P, fuzzy-searched, every action two keystrokes away.
- **A real settings GUI** - Cmd+, opens a full settings view: browse 193 color schemes with live preview, tweak everything, watch it apply before you save.
- **Profiles** - named launchers with their own shell, args, cwd, env, and tab color. One right-click away.
- **Settings sync** - push your config to your own private git repo and pull it anywhere. Your credentials, your repo, no OAuth middleman.
- **Supreme defaults** - truecolor, mouse selection and copy, cwd-aware new tabs, bracketed paste, OSC 52 clipboard, shell-integration exit signals, cursor styles, ligatures, session restore.

## The name

`std*` - as in `stdin`, `stdout`, `stderr` - meets *dusk*. A terminal stream at the faded end of the day. Revachol energy, no direct ripoff. The machine speaks back.

## Configure

Hit **Cmd+,** for the settings view, or edit `~/.config/stdusk/config.toml` by hand - same thing, the GUI just saves it for you. Missing file, sane defaults. See [`native/config.example.toml`](./native/config.example.toml) for the full set (theme, opacity, hotkey, cursor, bell, profiles, progress detection, CLI badges, sync).

## Build from source

```sh
cd native
cargo run
```

Rust 2024 edition. Architecture and roadmap in [`native/PLAN.md`](./native/PLAN.md); current state in [`native/LEDGER.md`](./native/LEDGER.md).

## Lineage

stdusk is a hard fork of [Tabby](https://github.com/Eugeny/tabby) (MIT). The Rust rewrite lives in `native/` on the default `main` branch; the original Electron Tabby source stays in-tree (the `tabby-*` dirs) and upstream at [Eugeny/tabby](https://github.com/Eugeny/tabby) as a reference. Credit where it's due - Tabby nailed the vibe, we chased the efficiency.

## License

MIT. You made the clicks. The terminal is yours.
