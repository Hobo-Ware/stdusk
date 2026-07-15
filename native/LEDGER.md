# stdusk - implementation ledger

Living record of what's built, what's next, and the hard-won facts an agent needs to
resume without rediscovering them. **Every agent updates this file after each work session
or milestone.** Keep it truthful - if a test is red or a step was skipped, say so.

- Project: a native Rust quake terminal with a real GUI tab bar + first-party AI agent.
- Repo: `Hobo-Ware/stdusk` (a hard fork of Eugeny/tabby - Electron Tabby lives on `master`
  as reference; the Rust rewrite lives on branch `rust`, crate in `native/`).
- Full design: [PLAN.md](./PLAN.md). This ledger is the *state*; the plan is the *intent*.

## Resume protocol
1. Read PLAN.md (intent) + this ledger (state).
2. Build/test to confirm the ledger matches reality (commands below).
3. Do the "Next up" work. Update the milestone table + "Done details" + "Decisions" as you go.
4. Never mark a milestone done without its exit-criteria tests green.

## Build / run / test
```
cd native
cargo build            # NOTE: check the real exit code, not `| tail` (see Gotchas)
cargo run              # opens the GUI (needs a display; can't run headless in CI sandbox)
cargo test             # unit + headless integration
```

## Milestone status (mirror of PLAN §6)
| Phase | What | Status |
|------:|------|--------|
| M0 | Chrome: quake window + chunky tab bar | ✅ done, compiles |
| M1 | pty + text render + input | ✅ done, human-verified |
| M1.5 | Progress (%-regex + OSC 9;4) + OSC scanner (cwd) | ✅ done, human-verified (Tabby-style tabs + top progress) |
| M2 | Colored cell renderer + cursor | ✅ done, human-verified (real colors + cursor) |
| M2.5 | Clickable links | todo |
| M3 | Quake: configurable global hotkey (default Ctrl+`) | ✅ done, human-verified (toggle + hide/show + first-run sizing) |
| M4 | Theming + config.toml (Tabby-default parity) | ✅ done, human-verified (themes + opacity + hotkey + font/height/progress) |
| M5 | Tab mgmt: context menu, color, rename, reorder, keybinds, cwd | 🟡 code done, builds + 17 tests green, pending human verify |
| M6 | Resize + scrollback + paste + OSC52 + bracketed-paste | 🟡 code done, builds + 17 tests green, pending human verify |
| M6.5 | Mouse text selection + Cmd+C copy | todo (split from M6 - real work: drag + alacritty Selection + highlight) |
| M7 | Scrollback search | todo |
| M8 | Split panes | todo |
| M9 | Shell integration (OSC 133) + exit state dot | todo |
| M10 | First-party AI agent | todo |
| M11 | Polish + settings GUI | todo |

## Done details
### M0 - chrome (`src/main.rs`)
- eframe/egui 0.35 app. Borderless (`with_decorations(false)`), transparent, top-left,
  1200x500. OneHalfDark `palette` module. Chunky tab bar via `egui::Panel::top`, clickable
  `draw_tab` pills (rounded corners), active tab in accent, `+` adds tabs.
- Verified: compiles clean + **window renders (human-confirmed 2026-07-15)**. Note: GUI needs
  the user's aqua session - a detached background launch exits immediately; run foreground.

### M1 - pty + shell (`src/terminal.rs`, wired into `main.rs`)
- `PtyTerm::spawn(cols, rows, ctx)`: `portable-pty` spawns `$SHELL`; a reader thread feeds
  bytes through `alacritty_terminal::vte::ansi::Processor` into `Term<EventProxy>` behind
  `Arc<FairMutex>`; `ctx.request_repaint()` on output.
- `snapshot()` returns visible grid lines (plain text, no color yet).
- `main.rs`: each `Tab` owns a `PtyTerm`; central panel renders the active tab's snapshot as
  monospace; `collect_input` maps key/text events → pty bytes (Enter/Backspace/Tab/Esc/arrows/
  Ctrl+letter). Fixed 80x24, no color, no cursor, no resize, no scrollback yet.
- Verified: compiles clean + **live shell + tabs confirmed working (human run 2026-07-15)**.

### M1.5 - progress + OSC scanners (`src/progress.rs`, `src/osc.rs`)
- `progress.rs`: Tabby's exact `%`-regex `(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)`, 0<pct<=100,
  alt-screen guarded, per-chunk decision (Tabby semantics) + a trailing-digit carry so a
  `%` split across reads still matches. `Progress { None|Normal|Error|Indeterminate|Paused }`.
- `osc.rs`: `OscScanner` frames `ESC ] ... (BEL|ST)` across chunks (Tabby oscProcessing algo);
  emits `Cwd` (OSC 7 file-url + OSC 1337 CurrentDir=, `~` expanded), `Clipboard` (OSC 52 raw
  b64, decode deferred to M6), `Progress` (OSC 9;4 states 0-4).
- `terminal.rs`: reader thread runs both scanners per chunk; reads `term.mode()` for the
  alt-screen flag AFTER `parser.advance`; OSC 9;4 wins over %-scrape; writes `TabState{progress,cwd}`
  (`Arc<Mutex>`). `PtyTerm::progress()` / `cwd()` accessors.
- `main.rs`: **Tabby-style flat tabs** (user feedback - filled pills were too heavy). Each
  tab = dark flat bg (ELEVATED when active + bold white text), and the progress bar as a 2px
  line on the TOP edge: green=normal(pct), yellow=paused(pct), red=error(full),
  accent=indeterminate(full).
- **Tab color is opt-in** (user feedback - Tabby has NO color by default): `Tab.color:
  Option<Color32>` starts `None` → no underline. The M5 right-click Color submenu (No color +
  `palette::TAB_COLORS` swatches) sets/clears it. `TAB_COLORS` kept `#[allow(dead_code)]` until M5.
- Tests: 13 green (progress golden table incl. split-read/alt-screen/out-of-range; OSC
  framing incl. partial-chunk buffering + 7/1337/9;4). Live bar NOT yet human-verified.
- Known: `term.mode()` + `TermMode::ALT_SCREEN` exist in alacritty_terminal 0.26 (confirmed).
  `cwd()` + `OscEvent::Clipboard` payload are parsed but not yet consumed (warnings) - land in M5/M6.

### M2 - colored cell renderer (`src/colors.rs`, renderer in `main.rs`)
- `colors.rs`: `to_color32(alacritty Color)` - OneHalfDark ANSI 0-15, 256-cube + grayscale
  for Indexed, truecolor for Spec. `is_default_bg()` → render transparent so window opacity
  shows through. (Separate from main.rs's inline `mod palette` for UI chrome - name clash
  avoided; file is `colors.rs` not `palette.rs`.)
- `terminal.rs`: `grid_snapshot() -> GridSnap { cols, rows, cells: Vec<CellSnap{c,fg,bg:Option}>,
  cursor:(row,col) }`. Handles INVERSE flag (swap fg/bg). Cursor from `grid.cursor.point`.
- `main.rs` `render_grid`: per-cell bg rect + fg glyph via `painter.text`, beam cursor. Cell
  metrics measured with `painter.layout_no_wrap("M")` (FontsView::glyph_width needs &mut and
  `ui.fonts()` only gives &, so layout-a-galley is the way).
- Fixed 80x24 still; bold/italic font variants + cursor styles deferred (M9). Colors + cursor
  build + 13 tests green; live colors NOT yet human-verified.

### M3 - quake (`main.rs`)
- `global-hotkey` 0.8: `GlobalHotKeyManager::new()` + `.register(HotKey::new(Some(Modifiers::
  CONTROL), Code::Backquote))`. Manager stored in `Stdusk._hotkey` (drop = unregister).
- Events: a thread blocks on `GlobalHotKeyEvent::receiver().recv()`; on `HotKeyState::Pressed`
  it sets an `Arc<AtomicBool>` toggle + `ctx.request_repaint()` (this wakes eframe **even while
  the window is hidden** - the key to show-from-hidden working).
- `ui()` consumes the toggle → flips `visible` → `apply_visibility()` sends `ViewportCommand::
  {OuterPosition(0,0), InnerSize(monitor_w, monitor_h*0.5), Visible, Focus}`.
- Hide-on-focus-loss: armed only after the window first gains focus (`was_focused` starts
  false) so a window that launches unfocused doesn't vanish instantly.
- **macOS gotcha (SOLVED, human-verified)**: `Visible(false)` OR moving fully off-screen lets
  the OS occlude + App-Nap the process → run loop throttles → the global hotkey stops firing,
  so it can't be brought back. Fix: hide by parking the window mostly below the screen with a
  **2px sliver still on-screen** (stays un-occluded) + `request_repaint_after(120ms)` while
  hidden. Do NOT use `Visible(false)` for quake hide.
- First-run sizing: `monitor_size` is None on frame 0, so apply full quake sizing on the first
  frame where it's known (guarded by a `sized` flag, retry via request_repaint until then).
- Deferred to polish: drop animation, proper native hide (NSPanel orderOut via objc2 - replaces
  the sliver hack), config-driven hotkey (M4).

### M4 - theming + config (`src/config.rs`, `src/colors.rs` rewrite)
- `colors.rs` is now the SINGLE color module: a `Theme { bg, fg, cursor, ansi[16] }` set once
  at startup via `colors::init()` (global `OnceLock`). All color reads go through fns
  (`bg()/fg()/dim()/accent()/red()/green()/yellow()/panel()/elevated()/cursor()/to_color32()`).
  Chrome colors are DERIVED from the theme (panel=darker bg, elevated=lighter bg, dim=ansi[8],
  accent=ansi[4], etc.). **The inline `mod palette` in main.rs is gone** - use `colors::*`.
- Built-in themes: `one_half_dark` (default), `dracula`, `tokyo_night`; `by_name(&str)`.
- `config.rs`: `Config { appearance{theme,opacity,font_size}, quake{hotkey,height_pct,
  hide_on_focus_loss}, terminal{detect_progress} }`, serde + `#[serde(default)]`, loaded from
  `~/.config/stdusk/config.toml` (missing file/fields → defaults). `parse_hotkey()` →
  (Option<Modifiers>, Code) via keyboard_types `Code::from_str` on a normalized W3C name.
- Wired: theme → all colors; opacity → `clear_color`; font_size → `render_grid`; hotkey →
  registration; quake height_pct + hide_on_focus_loss; detect_progress → ProgressScanner.
- `config.example.toml` shipped in the repo for reference.
- 17 tests green (added config defaults, partial-TOML merge, hotkey parse table, garbage
  fallback). Theme switch NOT yet human-verified.
- Deferred: blur (needs window vibrancy, not just opacity), custom font family (needs font
  file loading), keybind config (M5), live hot-reload (M4.5, `notify`).

### M5 - tab management (`main.rs`, `terminal.rs`)
- Right-click tab → egui `response.context_menu` (native popup): New tab, Rename…, Color ▶
  (No color + `colors::tab_colors()` swatches), Move left/right, Close. Menu sets a deferred
  `TabAction`; all structural mutations applied AFTER the panel `.show()` (avoids borrowing
  self mutably mid-iteration). egui auto-closes the menu on button click.
- Keybinds (`i.modifiers.command`): Cmd+T new-in-cwd, Cmd+W close active, Cmd+1..9 switch.
- Rename: `self.renaming: Option<(usize,String)>` → centered `egui::Window` with a focused
  text field (Enter/OK commit, Esc/Cancel abort). Sets `Tab.renamed=true` so cwd auto-title stops.
- **cwd auto-title + new-tab-in-cwd** (low-hanging, uses the OSC 7/1337 cwd from M1.5): unrenamed
  tabs show `basename(cwd)`; `PtyTerm::spawn` takes an optional starting cwd and sets
  `CommandBuilder.cwd`. NOTE: only works if the shell emits OSC 7/1337 - macOS default
  `/etc/zshrc` adds an `update_terminal_cwd` precmd hook that does, so it usually works; a shell
  that doesn't emit it leaves the title as "zsh" and new tabs inherit the process cwd.
- close_tab never leaves zero tabs (spawns a fresh one); active index clamped.
- 17 tests green (no new unit tests - this is UI-heavy; verified by human run).

### M6 - resize + scrollback + paste + clipboard (`terminal.rs`, `main.rs`)
- **Resize**: `PtyTerm` now stores the pty `master: Box<dyn MasterPty + Send>` (was dropped!).
  `resize(cols,rows)` → `master.resize(PtySize)` + `term.resize(Dims)` (Term::resize<S:Dimensions>).
  main computes cols/rows each frame from `ui.available_size() / cell metrics`; no longer fixed 80x24.
- **Scrollback**: comes free from `Config::default().scrolling_history` (10k) - no Dims history
  needed. `grid_snapshot` renders via `grid.display_iter()` (row-major over the visible viewport,
  honors scroll offset). Wheel → `term.scroll(Scroll::Delta(lines))` from `smooth_scroll_delta.y`
  (NOT `raw_scroll_delta` - doesn't exist in egui 0.35). Typing/paste → `scroll_to_bottom`.
  Cursor is `Option` now - `None` while scrolled into history (hidden).
- **Paste**: egui emits `Event::Paste(String)` on Cmd+V; `term.paste()` wraps in `\x1b[200~..\x1b[201~`
  when `TermMode::BRACKETED_PASTE` is set.
- **OSC 52**: reader decodes base64 (`base64` crate) → `TabState.clipboard`; UI takes it →
  `ctx.copy_text()`. Copy-FROM-selection (Cmd+C) is M6.5.
- **Selection + Cmd+C copy deferred to M6.5** - needs mouse drag tracking + alacritty `Selection`
  + highlight rendering + cell hit-testing. Real work, kept out of M6 to stay shippable.
- **Scrollbar** (user ask): right-edge draggable thumb, shown when `history_size>0`; position
  from `scroll_state()`, drag maps pointer y → `scroll_to_offset`. Dim, alpha 90/180 on hover.
- **Rounded window corners** (user ask): paint one rounded bg rect (radius 10) at `ui.max_rect()`,
  panels made TRANSPARENT so it shows through; the transparent OS window rounds the corners.
  Tab bar no longer has its own panel tint (uses the bg) - `colors::panel()` removed.

## Gotchas / facts learned (don't rediscover these)
- **`cargo build 2>&1 | tail` masks the real exit code** - the pipe returns tail's 0 even
  when cargo failed. Use `cargo build 2>build.log; echo $?` and grep build.log for `^error`.
- **eframe 0.35 changed the App trait**: implement `fn ui(&mut self, ui: &mut egui::Ui, frame)`
  - NOT `update(ctx)`. Panels use `.show(ui, ...)` (root Ui), not `.show(ctx, ...)`.
- **egui 0.35 unified panels**: `egui::Panel::top("id")` replaces `TopBottomPanel::top`.
  `SidePanel`/`TopBottomPanel` are gone; there's one `Panel` + `CentralPanel`.
- **egui 0.35 misc**: `Frame::new()` (not `none()`), `.corner_radius(CornerRadius::same())`
  (rounding was renamed), `Margin::symmetric(i8, i8)`.
- **alacritty_terminal 0.26 `TermSize` is test-only** (`term::test::TermSize`, behind a
  `pub mod test`). Implement `alacritty_terminal::grid::Dimensions` yourself
  (`total_lines`/`screen_lines`/`columns`) - see `terminal.rs` `Dims`.
- **`vte::ansi::Processor` needs its default type param pinned**: `let mut p: Processor = Processor::new();`
- **Progress is NOT OSC 9;4 in Tabby** - it's a %-regex scrape gated on alt-screen. Mirror
  that (see PLAN §4b) as primary; add OSC 9;4 as an enhancement.
- **No official Anthropic Rust SDK** - the AI agent (M10) uses raw `reqwest` → `POST
  /v1/messages`, model `claude-opus-4-8`, adaptive thinking, streaming SSE. Pin
  `anthropic-version: 2023-06-01`.
- **Tabby has ~zero tests** - reuse its *spec* (exact progress regex, OSC framing, config
  defaults from `tabby-terminal/src/config.ts`) as golden fixtures, plus xterm.js/esctest
  vectors. See PLAN §5.

## Decisions log
- Splits (M8) + scrollback search (M7) are v1 must-haves (user).
- Quake hotkey is configurable; default `Ctrl+\``; user overrides to `F13` in config.
- Quake uses `global-hotkey` (Carbon) - no macOS Accessibility grant (the skhd route was a
  dead end; Ghostty can't do native tabs in its quick terminal; kitty looked too plain).
- First-party AI agent is the "better than Tabby" differentiator (§4i).
- Electron Tabby stays on `master` untouched as the reference implementation.
- **Functional-first**: deep UX/visual polish (spacing, animations, quake drop anim, tab
  separators, font tuning) is deferred to a dedicated pass (~M11). Ship behavior, then beauty.
- Tab bar look confirmed: flat tabs + per-tab colored underline + top-edge progress (Tabby-style).

## Next up
**M3 - quake mode** (configurable global hotkey, default `Ctrl+\``).
- Add `global-hotkey` crate; register the hotkey (Carbon API on macOS → no Accessibility
  prompt). Poll `GlobalHotKeyEvent` each frame in `App::ui` / via a channel.
- Toggle window: `ctx.send_viewport_cmd(ViewportCommand::Visible/Focus/OuterPosition/InnerSize)`.
  Drop from top edge, full monitor width; lerp height ~120ms. Hide on focus-loss (configurable).
- Hardcode `Ctrl+\`` for now; the config-driven parse lands in M4. See PLAN §4c.
- **Theming note for M4**: colors are currently hardcoded in `colors.rs` + inline `palette`;
  M4 refactors both to read a `Theme` loaded from config.toml (ship OneHalfDark + a few more).
