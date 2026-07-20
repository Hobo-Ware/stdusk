# stdusk - post-Tabby feature ideas

Written 2026-07-20 as the **final mine of the Tabby (Electron) source before it's deleted from this
repo**. This is the curated list of capabilities that still live only in Tabby's tree
(`tabby-terminal`, `tabby-core`, `tabby-local`, `tabby-linkifier`, `tabby-settings`,
`tabby-community-color-schemes`) and that stdusk does *not* yet have.

Companion to [PARITY.md](./PARITY.md) (the done/partial/todo matrix) and [V1.md](./V1.md)
(§b ranked gaps, §d non-goals). Everything already tracked there is deliberately not repeated;
this file only adds what those two missed, plus a conscious record of what we skipped and why.

Sizes: S = hours · M = a day-ish · L = multi-day.

---

## Low-hanging fruit

Small, high-value, fits egui + alacritty_terminal, not a declared non-goal.

### 1. OSC 8 explicit hyperlinks
**What:** honor terminal-emitted hyperlinks (`ESC]8;;URI ST text ESC]8;; ST`), not just regex-detected
URLs. **Why it matters:** `ls --hyperlink`, `gcc`/`clang` diagnostics, `gh`, `eza`, jira/CI tools all
emit real OSC 8 links whose display text differs from the target (e.g. "PR #123" -> a URL). stdusk's
`links.rs` is regex-only, so those either aren't clickable or open the wrong thing. Cross-ref PARITY
*Links* (currently only "URL + IP + file paths").
**Sketch:** `alacritty_terminal 0.26` **already parses OSC 8** and stores it on the cell -
`cell.hyperlink() -> Option<Hyperlink>` (verified in the vendored crate's `term/cell.rs`), so the PTY
side is free.
- `terminal.rs` `grid_snapshot`: carry `cell.hyperlink().map(|h| h.uri().to_owned())` per cell (or per
  run) into the snapshot struct.
- `ui.rs` (the existing hover/underline/click path that draws regex `links::Link`s): if the hovered
  cell has an explicit hyperlink, prefer it over the regex match; open with the existing
  `links::open`-style `Command::new("open")`. Respect `link_modifier` the same way.
- No new regex, no PTY changes. **Size: S-M.**

### 2. Audible terminal bell
**What:** a third bell mode alongside stdusk's `visual`/`off`. **Why it matters:** Tabby ships
`off | visual | audible`; some users (IRC, `read -p`, long builds) genuinely want the sound. Cross-ref
PARITY *Rendering* / bell.
**Sketch:** widen `terminal.bell` in `config.rs` to accept `"audible"`; in the BEL handler that already
drives the visual flash (`terminal.rs`/`ui.rs`), branch on the mode and ring the system beep -
`objc2-app-kit` `NSBeep()` (already in the objc2 stack) or `Command::new("afplay").arg("/System/Library/Sounds/Funk.aiff")`.
One config enum value + one branch. **Size: S.**

### 3. Save-as-profile from a running tab
**What:** context-menu "Save as profile" that snapshots the live tab (cwd + shell/args/env + color)
into a new named `[[profiles]]` entry. **Why it matters:** Tabby has it
(`tabContextMenu.ts::SaveAsProfileContextMenu`, captures `getWorkingDirectory()`); PARITY marks it
`⛔ depends on profiles (deferred)` - **but the profiles editor shipped in 0.5.0, so the dependency is
gone.** Fastest path from "I set up a nice shell here" to a reusable profile.
**Sketch:** add a menu item in `tabs.rs` tab context menu; build a `config::Profile` from the pane's
tracked cwd (procwatch/OSC 7 cwd already available) + active profile fields; append to `Config.profiles`;
persist via the existing `config_to_toml` Save path and fire autosync. Reuse the profiles-editor
round-trip. **Size: S.**

### 4. Per-profile color scheme + dynamic-title override
**What:** let a profile carry its own terminal color scheme and a `disableDynamicTitle` flag.
**Why it matters:** Tabby profiles have `terminalColorScheme`, `disableDynamicTitle`, and `icon`
(`localProfileSettings` Colors tab, `profileDefaults.ssh.disableDynamicTitle`). stdusk's `Profile` only
carries a tab-dot `color`, so "prod = red dracula, no title override" isn't expressible. Cross-ref
PARITY *Profiles* (icon already noted ⬜).
**Sketch:** add `theme: Option<String>` and `dynamic_title: Option<bool>` to `Profile` in `config.rs`;
apply on spawn in `session.rs`/`workspace.rs` (theme overrides the global for that pane's palette;
`dynamic_title=Some(false)` pins the title to the profile/cwd); expose both in the profiles editor in
`settings.rs`. **Size: S-M.** (Profile `icon` stays ⬜ - needs an icon set.)

### 5. Backspace-key mode (Ctrl-H vs DEL)
**What:** map the Backspace key's byte to `ctrl-h (0x08) | ctrl-? (0x7f) | delete (ESC[3~)`.
**Why it matters:** Tabby's `InputProcessor` (middleware/inputProcessing.ts) rewrites a lone `0x7f`;
fixes remote hosts / TUIs / emacs users who expect `^H`. Not in PARITY at all.
**Sketch:** `terminal.backspace` config enum (default `ctrl-?`, current behavior); in the
key->PTY-byte path in `workspace.rs`, when the emitted byte is a bare `0x7f`, substitute per the mode.
Could also be per-profile. **Size: S.**

### 6. Recent-profiles MRU in the picker
**What:** float the last few launched profiles to the top of the `+`-menu / palette profile list.
**Why it matters:** Tabby's `terminal.showRecentProfiles: 3`. Small quality-of-life once you have more
than a handful of profiles. Cross-ref PARITY *Profiles* / *Command palette*.
**Sketch:** keep a short `Vec<String>` MRU of profile names in session state (persist alongside the
session file); prepend (deduped, capped at 3) when building the profile launcher list in
`tabs.rs`/palette in `main.rs`. **Size: S.**

### 7. Scroll-on-input toggle (verify first)
**What:** snap the viewport to the bottom when the user types after having scrolled up
(Tabby `scrollOnInput`, default on). **Why it matters:** avoids "typing blind" while scrolled into
history. **Verify before building:** alacritty may already do this on keypress; if so this is a no-op or
just an *opt-out* toggle. If not, add `terminal.scroll_on_input` and force `display_offset = 0` on the
input path in `workspace.rs`. **Size: S.**

### 8. Cmd+V image paste (screenshot -> Claude Code)
**What:** make `Cmd+V` paste a clipboard image the way Tabby did, matching mac muscle memory.
`Ctrl+V` already works (forwards `^V`, which Claude Code reads to ingest the clipboard image), and
right/middle-click paste are image-aware - but `Cmd+V` on an image-only clipboard is currently a
no-op. **Why it stalls:** egui-winit 0.35 (`lib.rs` ~1007-1015) folds `Cmd+V` into `Event::Paste`,
reads clipboard TEXT only, emits nothing when the text is empty, and swallows the key - so no egui
event fires and there's nothing to intercept from `workspace.rs`/`ui.rs`. **Fix paths:** (a) a
raw-winit `WindowEvent::KeyboardInput` hook that sees `Cmd+V` before egui-winit and injects `^V`
when `arboard::get_image()` is Some - but eframe's `run_native` exposes no per-event hook, so this
needs leaving eframe's loop or a custom integration; or (b) patch/vendor egui-winit to emit an
event on an empty/image paste. **Size: M** (blocked on the eframe/egui-winit seam). PARITY *Paste*.

---

## Future ideas

Bigger, or need design thought - but genuinely interesting for a daily local-shell user.

### iTerm2 `.itermcolors` + base16 scheme import
Already ⬜ in PARITY *Themes*. `.itermcolors` is an XML plist with 0-1 float RGB components; base16 is a
small YAML palette. Value is moderate (the 191 embedded XRDB schemes already cover most tastes).
**Risk/unknown:** a plist parser dependency + float->8bit mapping; base16's 16-slot mapping convention.
Port `themes.rs::parse_xrdb` alongside two new parsers behind the same `Theme` output.

### 256-color palette generation ("generate" / "harmonious")
Port `generatePalette.ts` (LAB-space interpolation filling indices 16-255 from the base-16 colors, with
a light-theme inversion that "harmonious" disables). Makes 16-color schemes render correctly for apps
that reach into the 6x6x6 cube / grayscale ramp. Tabby exposes it as `paletteGenerate`/`paletteHarmonious`
toggles. **Risk/unknown:** the pure math port is easy (it's already standalone); the design work is
wiring it into the live palette build in `palette.rs`/`colors.rs` and deciding default on/off. Niche.

### Copy-as-HTML (rich clipboard)
Tabby default-on (`copyAsHTML`): copy selection with per-cell fg/bg as an HTML fragment so paste into
docs/chat keeps colors. **Risk/unknown:** `arboard` has no first-class HTML flavor on macOS; needs
`NSPasteboard` `public.html` writing next to the plain-text flavor. Niche; PARITY already lists it ⬜.

### toggle-fullscreen
Tabby binds `Ctrl+Cmd+F`. Listed ⬜ in PARITY *Tabs*. **Risk/unknown:** semantically odd for a quake
drop-down and collides with the height-% / monitor-sizing model; only meaningful in normal-dock mode.
Small in winit terms (`set_fullscreen`) but low value - park it. **LE (1.0.9):** `quake.mode = "window"`
now ships that normal-window mode, so fullscreen would finally be coherent there - revisit as a
window-mode-only bind if asked.

### Pane/tab reshaping: explode/combine tabs, rearrange-panes label mode, drag-tab-into-split
Already in PARITY / V1 P2. Power-user layout surgery (`explode-tab`, `combine-tabs`, `rearrange-panes`
overlay, split drop-zones). Genuinely nice but each is real interaction + hit-testing work in
`workspace.rs`/`tabs.rs`. **Risk/unknown:** drop-zone hit-testing and focus bookkeeping.

### Per-profile input/output newline conversion + input mode
Tabby's streamProcessing: CR/LF/CRLF translation in both directions plus line-vs-raw input modes.
Mostly a serial/telnet nicety - rarely needed for a local shell, so lowest priority of the batch.

---

## Deliberately excluded

Matches a v1 non-goal (V1.md §d) or an already-recorded PARITY drop. Listed so the skip is on the record
after the Tabby source is gone.

- **Sixel / kitty inline images** - alacritty grid has no image model (V1 §d; PARITY *Rendering*).
- **True OpenType ligature shaping + color emoji** - egui rasterizes monochrome outlines, no shaping;
  symbol-substitution ligatures are the documented limit (V1 §d).
- **SSH / serial / telnet** and their `reconnect-tab` / `disconnect-tab` hotkeys - run `ssh` in the shell
  (V1 §d).
- **Plugin system / marketplace** - out; a thin Rust hook API stays a maybe (V1 §d / PLAN §9).
- **`debug-save/load/copy/paste-state` + `-output` hotkeys** - niche debug tooling (PARITY *Session*).
- **SaaS / web config sync** - git-based `sync.rs` is the answer (V1 §d).
- **Vault / encrypted secrets, zmodem, auto-sudo-password / UAC (`runAsAdministrator`), login scripts /
  expect-send, analytics, auto-update** - out; brew owns updates (V1 §d; PARITY *Dropped*).
- **Custom CSS (`appearance.css`), welcome tab** - out (V1 §d).
- **Window-chrome long tail** - `tabsLocation`, `flexTabs`, `frame` modes, `vibrancy`/blur/background
  image, `dockScreen`/dock-edge/`dockFill`/`dockSpace`/`dockAlwaysOnTop`, `hideTabIndex`/
  `hideCloseButton`/`hideTabOptionsButton`/`showTabProfileIcon`/`tabsInFullscreen` (V1 §d; PARITY
  *Quake/window*).
- **Multi-window, non-macOS ports, keybinding GUI editor** - config-file remapping only (V1 §d).
- **`tab-10`..`tab-20` hotkeys** - Tabby ships them unbound; nothing to mirror (PARITY *Tabs*).
- **`focus-all-tabs` (cross-tab input broadcast)** - intentionally skipped for a quake terminal; per-pane
  broadcast shipped (PARITY *Panes*).
- **Windows-only: ConPTY, `setComSpec`, `windowsRefreshEnvironment`, `useConPTY`** - non-macOS.
