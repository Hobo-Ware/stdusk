# stdusk - implementation ledger

Living record of what's built, what's next, and the hard-won facts an agent needs to
resume without rediscovering them. **Every agent updates this file after each work session
or milestone.** Keep it truthful - if a test is red or a step was skipped, say so.

- Project: a native Rust quake terminal with a real GUI tab bar + first-party AI agent.
- Repo: `Hobo-Ware/stdusk` (a hard fork of Eugeny/tabby). The Rust rewrite is the default `main`
  branch, crate in `native/`. The forked Electron Tabby source stays in-tree (the `tabby-*` dirs)
  + upstream at Eugeny/tabby as the reference implementation.
- Full design: [PLAN.md](./PLAN.md). This ledger is the *state*; the plan is the *intent*.

## Visual testing (use this before asking the user about UI!)
`cargo build && timeout 25 ./target/debug/stdusk --screenshot /tmp/shot.png` renders a frame
with representative demo tabs (colored, active, long/ellipsized titles) and saves a PNG, then
exits. Then Read /tmp/shot.png to SEE the result. Iterate UI changes against the screenshot
instead of round-tripping with the user. Mechanics: eframe's built-in `__screenshot` feature
(glow backend only - `renderer: Renderer::Glow` + features `["glow","__screenshot"]`), triggered
by the `EFRAME_SCREENSHOT_TO` env var which `--screenshot` sets. Captures at cumulative pass 2.

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
| M5 | Tab mgmt: context menu, color, rename, reorder, keybinds, cwd | ✅ done; headless click / drag-reorder / close-x regression tests (0.2.3) + shipped through 0.1.0-0.5.0 daily use |
| M6 | Resize + scrollback + paste + OSC52 + bracketed-paste | ✅ done; real-pty e2e (scrollback fill/wipe, CSI-8 grid dims) + Tabby-exact paste pipeline table-tested; live clipboard round-trip in the 1.0.0-rc human shortlist |
| M6.5 | Mouse text selection + Cmd+C copy | ✅ done; hit-test/selection math unit-tested; live pasteboard copy in the 1.0.0-rc human shortlist |
| M7 | Scrollback search (Cmd+F) | ✅ done; find bar screenshot-verified, headless backspace e2e, 9 search unit tests |
| M8 | Split panes (pane tree, focus, drag-resize, per-pane pty) | ✅ done; pane math unit-tested + split layouts screenshot-verified (3-pane + broadcast shots) |
| M9 | Shell integration (OSC 133) + exit dot; bell; cursor styles | ✅ done: auto-injected shell hooks, bell flash, cursor styles (running/ok tab indicators later dropped as noise - see M10) |
| M10 | Ambient CLI awareness (tab badges for claude/gemini/...) | ✅ done; detect/classify unit-tested, badges screenshot-verified; live process detection in the 1.0.0-rc human shortlist |
| M11 | Polish + settings GUI | ✅ done (0.2.1-0.2.4): full settings view, palette, profiles, sync |

**M10 pivot (user):** the original M10 "first-party AI chat agent" was built then **dropped** -
a chat panel duplicated the CLIs' own supreme UX and served no purpose. What the user actually
wanted by "agent support" was *ambient awareness of AI CLIs running in a tab*. Commit a7e1af82
(the chat agent) was reset out of history; M10 is now the CLI-awareness badge feature below.

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

### M6.5 - mouse selection + Cmd+C copy (`terminal.rs`, `main.rs`, `colors.rs`)
- **Selection lives in `Term`**: `Term.selection: Option<Selection>` (public field). `Selection::new(
  SelectionType::Simple, Point, Side)` on press, `.update(Point, Side)` on drag; `to_range(&term)
  -> Option<SelectionRange>`; `SelectionRange::contains(Point) -> bool`; `term.selection_to_string()`
  for the copied text. All in `alacritty_terminal::{index, selection}`.
- `PtyTerm` methods (all `&self`, lock internally): `start_selection/update_selection(line:i32,
  col:usize,right:bool)`, `clear_selection()`, `selection_text() -> Option<String>` (filters empty).
- `grid_snapshot` now computes the selection range once, tags each `CellSnap { selected: bool }`,
  and returns `top_line: i32` (buffer line of viewport row 0, taken from the first `display_iter`
  point) so the UI can map mouse coords -> grid `Point`.
- `render_grid` senses `click_and_drag`: `drag_started` -> start, `dragged` -> update, `clicked`
  -> clear. `hit(pos)` maps pointer -> (line, col, right-half) using `top_line` + cell metrics.
  Selected cells get a translucent `colors::selection()` (accent @ alpha 90) overlay under the glyph.
- **Double-click = word (`SelectionType::Semantic`), triple-click = line (`Lines`)** via
  `select_word`/`select_line`; single drag = `Simple`. egui `Response::{double,triple}_clicked()`
  checked before `drag_started`/`clicked` (else-if chain, they're mutually exclusive per frame).
- **GOTCHA - Cmd+C is NOT a key event**: egui folds Cmd+C/X/V into `Event::{Copy,Cut,Paste}`
  (same as Cmd+V paste). Checking `key_pressed(Key::C)` never fired (first-cut bug). Fix: watch
  for `egui::Event::Copy` in the frame's events, then `ctx.copy_text(selection_text())`. Ctrl+C
  stays SIGINT (collect_input maps `modifiers.ctrl` only). Selection cleared on keystroke + paste,
  kept while wheel-scrolling (buffer-point highlighting stays correct).
- **"Copied" toast**: `Stdusk.toast: Option<(String, f64-expiry)>` using egui's `input().time`
  clock (no `std::time` needed). `draw_toast()` paints a bottom-center pill that fades over the
  last 0.35s; `request_repaint` while active so it self-dismisses. A copy sets it to now+1.4s.
- **macOS natural-editing keys** (`collect_input`): Option+←/→ -> `ESC b`/`ESC f` (readline word
  back/fwd), Cmd+←/→ -> `Ctrl-A`/`Ctrl-E` (line start/end), Option+Backspace -> `ESC DEL` (word
  delete), Cmd+Backspace -> `Ctrl-U` (delete to line start). (First cut sent plain `ESC[C/D`
  regardless of modifiers - the "moves one-by-one" bug.)
- Builds + 17 tests green. Toast verified via a forced screenshot; drag/word/line select + Cmd+C
  + word-nav keys are live-interaction, **pending human verify**.

### M9 (in progress) - shell integration (`osc.rs`, `terminal.rs`, `ui.rs`)
- **OSC 133 → tab exit-state dot** (done). `osc.rs` parses `133;A|C|D[;code]` → `OscEvent::Shell(
  ShellEvent{PromptStart|CommandStart|CommandEnd(Option<i32>)})`; reader thread maps to
  `TabState.cmd: CmdState {Idle|Running|Ok|Fail}` (CommandStart→Running, CommandEnd 0/none→Ok else
  Fail, PromptStart keeps last result). `draw_tab` shows a small dot before the number:
  yellow=running, green=ok, red=fail, none=idle. Focused pane's state (aggregation deferred).
  6 OSC-133 parse cases tested.
- **NOTE: needs the shell to emit OSC 133.** zsh/bash don't by default; degrades gracefully (no
  dot). TODO: ship an opt-in shell-integration snippet (precmd/preexec hooks) + auto-source it.
- **Shell integration auto-inject** (`shell.rs`, done) - so the dot works without user setup.
  `integrate(cmd, shell)`: zsh → set `ZDOTDIR` to `~/.config/stdusk/shell/` (its `.zshenv`/`.zshrc`
  source the user's real `$STDUSK_REAL_ZDOTDIR` config, then add `preexec`/`precmd` OSC 133 hooks);
  bash → `--rcfile` our `bashrc` (sources `~/.bashrc` + a `PROMPT_COMMAND` hook); other shells
  untouched. Config `terminal.shell_integration` (default true). 2 tests (shell-kind detection,
  scripts emit 133 + source real rc).
- **Cursor styles** (done) - config `terminal.cursor` = block(default)/underline/beam via tested
  `ui::cursor_style`; `render_grid` draws each (block redraws the glyph in bg). 1 test.
- **Bell** (done) - alacritty `Event::Bell` → `EventProxy` flags shared `TabState.bell`; UI flashes
  a brief translucent overlay. Config `terminal.bell` = visual(default)/off.
- **Also fixed this milestone**: Shift+Tab → back-tab `\x1b[Z` (`key_to_bytes`, +test); broad
  monochrome font fallbacks: vendored full **NotoEmoji-Regular** (assets/, monochrome glyf - covers
  SMP emoji 😀💰 the egui-bundled subset misses) + macOS Arial Unicode + Apple Symbols for
  arrows/box-drawing/powerline. All appended to both font families. **Known limit**: COLOR emoji
  still can't render (egui rasterizes monochrome outlines only); emoji show as B/W - like Tabby's
  monochrome fallback, good enough.

### M10 - ambient CLI awareness (`procwatch.rs`, `main.rs`, `ui.rs`, `terminal.rs`, `pane.rs`)
- **Goal**: at a glance, know which tabs are running an AI CLI (claude / codex / gemini / copilot
  / aider / cursor / ollama). Each such tab shows a small brand-colored pill (e.g. clay "claude",
  blue "gemini") next to its title. NOT a chat agent - that was the dropped a7e1af82.
- **`procwatch.rs` (pure + adapter)**: `Cli` enum (label + brand color); `detect(&[Proc], root)`
  walks the process tree from a tab's shell pid and returns the highest-priority known CLI among
  its **descendants** (never classifies the shell/root itself). `classify(name, cmd)` scans path
  segments of the process name + every argv entry, extension-stripped, matching a `TABLE` of
  `(Cli, primary, aliases)` where a segment counts if it `== primary`, starts with `primary-`/
  `primary_` (so the `claude-code` node package dir matches), or hits an alias (`gh-copilot`,
  `cursor-agent`). This catches node/python CLIs whose process name is `node`/`python` but whose
  argv path contains the tool. `scan(&System, root)` is the thin sysinfo bridge. 6 unit tests.
- **sysinfo 0.38** (`default-features=false, features=["system"]`). API: `refresh_processes_specifics(
  ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet))`,
  `sys.processes()` -> `HashMap<Pid, Process>`, `Process::{name,cmd,pid,parent}`, `Pid::as_u32`.
  cmd (argv) is NOT in the default refresh - must opt in with `with_cmd`, else node CLIs are invisible.
- **Wiring**: `PtyTerm::shell_pid()` (captured from the spawned child's `process_id()`). `Pane::leaves()`
  aggregates a tab's panes. `Stdusk` holds one `sysinfo::System` + `next_cli_scan: f64`; `ui()` refreshes
  + rescans every tab **~1 Hz** (throttled on `input().time`, `request_repaint_after(1100ms)` to keep
  the cadence when idle; skipped in the screenshot harness). One refresh serves all tabs. `Tab.cli:
  Option<Cli>` -> `draw_tab` -> `draw_cli_badge` (tinted fill + brand-color label + hover tip).
  Config `terminal.detect_clis` (default true).
- **Tab exit-state indicator cut to just Fail** (user: the running/ok marquee lit up for any idle
  REPL - e.g. an open Claude CLI - = pure noise). `draw_tab` draws nothing for idle/running/ok and
  only a very subtle short dim-red left-edge line for `CmdState::Fail`. Progress reporting (the
  top-edge %-bar) stays the primary activity signal; OSC 133 is still parsed. Marquee + its
  per-frame `request_repaint` are gone.
- **`--version`/`-V`** flag prints and exits before creating a window (brew test + scripting).

### M8 - split panes (`pane.rs`, `main.rs`, `ui.rs`)
- **Pure `pane.rs`**: generic binary tree `Pane<T> { Leaf(T) | Split{dir, ratio, a, b} }` (`T =
  PtyTerm` in the app, `u32` in tests). A leaf's identity is its `path: Vec<Side>` (A/B from the
  root); focus is a path. Ops are **consuming + recursive** (rebuild the tree) to avoid unsafe
  in-place surgery: `split(path,dir,new) -> (tree, Some(new_path))`, `close(path) -> (Option<tree>,
  focus)` (collapses the parent into the sibling; `None` when the last leaf closed → close tab),
  `set_ratio` (in-place, `at_mut`), `layout(area) -> [(path, rect)]`, `splitters(area) ->
  [(path, dir, handle_rect, parent_rect)]`, `ratio_from_pointer`. `SplitDir::Row` = side by side,
  `Column` = stacked. 8 unit tests (split/close/collapse/refocus/layout/clamp).
- **`Tab` now holds `root: Option<Pane<PtyTerm>>` + `focused: Vec<Side>`** (Option so whole-tree
  transforms can `take()` it - `Pane` isn't `Default`). Accessors `focused_term[_mut]`, `root[_mut]`.
- **Keybinds**: Cmd+D split Row, Cmd+Shift+D split Column (new pane inherits focused cwd, gets
  focus); Cmd+W closes the focused pane, or the tab on its last pane.
- **Render**: `render_grid` reworked to paint ONE leaf at an arbitrary `rect` via `painter_at`
  (was `allocate_painter`), taking the pane path as its egui `Id`, drawing a per-pane scrollbar.
  Focus is shown Tabby-style by **fading the UNFOCUSED panes' content** (`dimmed` arg →
  `Color32::gamma_multiply(0.5)` on each glyph/bg/cursor), NOT an opaque scrim: blank cells stay
  transparent at the window's global opacity, so the glass reads uniform and only content recedes. The central panel tiles `root.layout(area)`: resize
  each pane's pty to its rect, wheel-scroll the pane under the pointer, route keys/paste/Cmd+C to
  the focused pane, set focus on click/drag. Draggable splitters drawn in the gutters (accent on
  hover, resize cursor). Verified via a forced 3-pane screenshot (row+column, focus border correct).
- **Mini-layout tab glyph** (user ask): each tab with >1 pane shows a tiny nested-rectangles
  preview of its split layout before the title (fractal - left/right → two vertical rects,
  top/bottom → two horizontal, recursing). `Pane::miniature()` returns leaf rects in a unit
  square (reuses `split_rect` with a proportional gap); `ui::draw_mini_layout` scales them into a
  15px box (fg when active, dim otherwise). `split_rect` gained a `gutter` param so the mini
  version uses a small proportional gap. 1 unit test.
- **Right-click pane menu** (user ask, useful subset of Tabby's): Copy (enabled with a selection),
  Copy current path (cwd), Split Right/Down/Left/Up, New tab, Close pane. Built via
  `resp.context_menu` → a `PaneAction` enum collected then applied after the panel (deferred, like
  `TabAction`). Left/Up use `Pane::split(.., new_first=true)` (new pane on the A side). Dropped
  Tabby items with no infra: profiles, notify-when-done/activity (needs OSC 133), focus-all/
  broadcast, export, switch-profile; Paste omitted (egui can't read the clipboard synchronously -
  Cmd+V still works). Menu is an egui popup so it doesn't show in the screenshot harness (like the
  tab menu) - pending live verify.
- Limitations (deferred): no drag-to-reorder panes, no broadcast-input, no pane zoom/maximize,
  tab bar shows only the FOCUSED pane's progress/title (not aggregated).

### M7 - scrollback search (`search.rs`, `main.rs`, `terminal.rs`)
- **Pure `search.rs`**: `find_matches(lines: &[(i32, String)], query) -> Vec<Match{line,col,len}>`,
  ASCII-case-insensitive substring, non-overlapping, top-to-bottom. 5 unit tests. `line` is the
  alacritty `Line` coord; `col` == char index (one char per cell).
- **`PtyTerm` glue**: `buffer_lines()` walks `topmost_line()..=bottommost_line()` indexing
  `grid[Line][Column].c` (trailing-trimmed); `highlight_match(m)` sets the selection range so the
  existing `grid_snapshot` selection-overlay paints it (no new render path); `scroll_to_line(l)`
  sets display offset `-l` (clamped to history) to bring the match to the top.
- **UI (`find_panel`)**: Cmd+F toggles a docked find bar (`Panel::top("findbar")` under the tab
  bar) - magnifier icon + text field + `cur/total` + caret-up/down + close-x (all Phosphor).
  Enter / Shift+Enter (and the buttons) cycle; Esc/x closes. Pty input is gated while open. The
  current match is highlighted (via selection) + scrolled into view.
- **GOTCHA - egui `Window`/`Area` does NOT render in the 2-frame screenshot harness** (needs
  extra frames to lay out). That's why rename (a `Window`) was only ever human-verified. Docked
  UI that must be screenshot-verifiable uses `Panel::top` instead - which is why the find bar is
  a panel, not a floating window. Verified via screenshot (glyphs + layout correct).
- **New Phosphor codepoints** pulled from the official `@phosphor-icons/web` CSS and confirmed
  present in the vendored subset via `fontTools` cmap: magnifying-glass E30C, caret-up E13C,
  caret-down E136. Don't guess codepoints - the font's glyph names are stripped to `uniXXXX`.
- Limitation: only the CURRENT match is highlighted (all-match highlight + regex toggle deferred).
- 34 tests green; clippy -D warnings clean.
- **Find-bar polish (user: "sexier, beat Tabby")**: the bar is now a compact right-aligned
  rounded pill (elevated fill + border + drop shadow) floating in the top strip, instead of a
  flat full-width bar - magnifier, inset field (bg = theme), `cur/total`, caret-up/down, close.
  Right-aligned via `add_space(available - PILL_W)` in a plain LTR horizontal (a `right_to_left`
  wrapper reversed the inner widget order - avoid it for this). **No-results feedback**: a
  non-empty query with zero matches turns the field text + count red, and a query change that
  yields nothing fires the "No results" toast (reuses the M6.5 toast).
- **Search toggles (user: Tabby parity)**: `SearchOpts { case_sensitive, regex, whole_word }`
  drives `find_matches`, now regex-backed (`RegexBuilder`): literal queries are `regex::escape`d,
  whole-word wraps `\b(?:..)\b`, case flips `case_insensitive`, invalid regex -> no matches (red).
  Find bar gained three `icon_toggle` buttons (accent-tinted when on): Aa (text-aa) case, `*`
  (asterisk) regex, `[ ]` (brackets-square) whole-word - codepoints fetched from official CSS +
  font-verified. Bigger input (16pt font, wider). 9 search unit tests (case/regex/whole-word/
  invalid). 38 tests total.

### Repo guidelines + supreme-ify refactor (user ask: `.agents` + Rust best practices)
- **Instruction files** mirror trakt-web's two-hop convention: `CLAUDE.md` (area router) →
  `@AGENTS.md` (imports 4 always-on core rules) → domain rules loaded on demand. Core:
  `project`, `code-principles`, `implementation`, `testing`. Domain: `ui` (egui), `terminal`
  (alacritty + parsers), `performance`, `platform` (quake/hotkey). All under `.agents/rules/`.
  Grounded in 3 parallel research passes (trakt-web recon, idiomatic Rust, egui/eframe).
- **Tooling**: `rustfmt.toml` (edition 2024, max_width 100); `Cargo.toml [lints]` deny
  `clippy::all` + warn `pedantic` with a justified allow-list; `unsafe_code = "deny"` (one
  local `#[allow]` on the edition-2024 `set_var`); `proptest` dev-dep; CI at
  `.github/workflows/native.yml` (fmt/clippy -D warnings/test on the `main` branch).
- **`ui.rs` extracted** from `main.rs` (was ~960 lines, mega-file): all pure helpers now live
  there and are unit-tested - `pos_to_cell` (mouse→grid), `ellipsize`, `key_to_bytes`
  (the whole keyboard table, incl. the modifier-arrow logic that had shipped a bug),
  `ctrl_letter`, `progress_fraction`, `toast_alpha`, `basename` - plus the egui drawing
  widgets (`draw_tab`, `icon_button`, `draw_toast`, `render_grid`, `tint`, `apply_theme`, the
  `icons` codepoints). `main.rs` is now just the `eframe::App` + window/hotkey plumbing.
- **Visibility**: every module flipped bare `pub` → `pub(crate)` (binary crate; nothing is a
  real public API). `Config`'s `Default` is now derived. `is_default_bg` takes `Color` by value.
- **Tests 17 → 29**: +10 `ui` helper unit tests, +2 `proptest` split-invariants (OSC event
  stream equal under any chunk cut; progress % survives a digit-boundary split). Clippy
  `-D warnings` clean; screenshot confirms zero visual regression.

### Tab-bar sexify (user ask)
- **Icons: vendored Phosphor font**, NOT the `egui-phosphor` crate. That crate (0.12, latest)
  only supports egui 0.34 - adding it pulls a 2nd egui and `add_to_fonts` type-mismatches. So:
  copied `Phosphor.ttf` into `assets/` (MIT), embed via `include_bytes!`, insert into the
  proportional font family, and hand-define the 4 codepoints in `mod ph` (PLUS/X/GEAR/APP_WINDOW).
  If egui-phosphor ever ships an egui-0.35 build, can switch back.
- Tab bar layout: gear pinned far right (`right_to_left`), tabs + `+` + tab-manager (`APP_WINDOW`)
  flow from the left (nested `left_to_right`). Close `X` on active/hovered tab via `ui.put`.
  `icon_button()` helper = frameless Phosphor button.

### Tab-bar polish round 2 (user: "not aligned, no hover feedback, no separation")
- **Distinct tab-bar strip**: the Panel frame fills `colors::titlebar()` (darker than body) with
  top-rounded corners; a `border()` hairline is drawn under it. Body stays `bg`. Clear separation.
- **Hover feedback**: `icon_button` is now a fixed 32x30 box drawn manually (allocate_exact_size)
  - paints a `hover()` highlight rect + brightens the glyph on hover, returns the Response.
  Close `X` on tabs got the same treatment.
- **Tab manager**: `egui::Popup::menu(&icon_button_response).show(...)` (0.35 API) - styled popup,
  not the faint `menu_button`.
- **GOTCHA - edge strokes clipped by row layout**: the nested `left_to_right` layout's clip cut
  off each tab's top/bottom 2-3px, so the colored underline + progress bar were invisible.
  `ui.painter()` AND `ui.painter_at(rect)` both intersect that clip -> still clipped. Fix: draw
  edge strokes on a foreground layer painter `ctx.layer_painter(LayerId::new(Order::Foreground,
  Id::new("tab_deco")))`. Coords stay in the tab-bar region so it doesn't overlap the body.
- Icon glyphs optically centered with a +1px y nudge (Phosphor ink sits high in the line box).

## Gotchas / facts learned (don't rediscover these)
- **Design system in `ui.rs`**: surfaces/inputs/buttons come from shared primitives -
  `overlay_frame()`, `text_field()`, `action_button()`, `icon_button`/`icon_toggle`,
  `color_swatch`, `style_menu`. Never hand-roll `Frame`/`TextEdit`/`Button` styling; two call
  sites of the same thing MUST share the helper (find bar + rename use the same input+surface).
  Rule + list in `.agents/rules/ui.md` §0.
- **A focused egui text field vs the terminal fighting for focus**: the terminal pane calls
  `request_focus()` every frame, so ANY open text field (find bar OR rename) must gate BOTH pty
  input and that focus call - `let input_captured = self.search.is_some() || self.renaming.is_some()`.
  Missing the rename case let the shell steal focus from the rename input.
- **Tab bar = ONE `left_to_right(Center)` row + fixed `set_min_height`; right-pin the gear with
  a spacer (`available_width - ICON_BTN_W`), never a nested `right_to_left`.** Nested opposing
  layouts drift vertically whenever a child's height changes (this misaligned the gear 3x). Full
  rule + the paint-icons-centered rule live in `.agents/rules/ui.md`.
- **egui buttons activate on Space/Enter when they hold keyboard focus.** The terminal grid
  isn't a normal focusable widget, so focus sat on a tab-bar button and a typed space/enter
  (`cd ~`↵) "clicked" it - the gear then ran `open config.toml`. Fix: the focused terminal pane
  calls `resp.request_focus()` each frame (when the find bar isn't open) so keystrokes can't
  reach chrome buttons. Any new focusable chrome must not break this.
- **Repo conventions live in `.agents/rules/`** - read via `CLAUDE.md`/`AGENTS.md`. Before
  touching UI read `ui.md`; terminal/parsers `terminal.md`; quake/hotkey `platform.md`.
- **CI gate is clippy `-D warnings`** (pedantic escalates in CI, warns locally). Keep
  `cargo clippy --all-targets -- -D warnings` + `cargo fmt --check` clean before pushing.
- **Pure logic goes in `ui.rs` with a test** - the render loop is untestable; extract the math.
- **`unsafe_code = "deny"`** - the only `unsafe` is `set_var` in `main` with a local `#[allow]`.
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
- **egui drag-only widgets black-hole clicks to widgets beneath them.** The hit test picks ONE
  topmost widget per interaction; a drag-only interact layered above a clickable one returns
  click:None for the whole stack (the 0.2.3 dead-tab regression). Anything that needs drag AND
  click must be a single `click_and_drag` widget, with drags gated on
  `pointer.is_decidedly_dragging()`. Rule in `.agents/rules/ui.md` §6.
- **egui `Grid` blanks the pass-2 screenshot capture** - its first-pass sizing discard means
  the harness's cumulative pass 2 catches an empty layout. Screenshot-verified views use
  fixed-width label columns instead (see `settings.rs`).
- **egui `Window`/`Area` never renders in the 2-frame screenshot harness; full panels do.**
  Corollary: build substantial UI as a docked panel / central VIEW, not a floating window -
  settings-as-view (0.2.2) beats the 0.2.1 floating settings window for verifiability.
- **Cherry-picking parallel-agent work can silently drop hunks** in 3-way merges - verify each
  rebuilt commit compiles AND its tests pass in a worktree before moving on (bit 0.2.1).
- **Headless egui end-to-end tests** (`Context::run_ui` driving real frames) are the sanctioned
  harness for interaction/hit-test/focus regressions - see the tab click/drag/close-x + find-bar
  backspace tests in `src/ui.rs` and the pattern in `.agents/rules/testing.md`.
- **Custom `[hotkeys]` binds can collide with terminal keys.** The app bind wins cleanly only
  for the combos `key_to_bytes` already reserves (the defaults are chosen that way); a rebind
  onto a terminal-bound chord (e.g. Ctrl+letter) fires the action AND the pty byte - by
  design, asserted in `rebound_terminal_chords_double_fire_by_design`. See the 0.5.0 entry.
- **eframe's screenshot capture (cumulative pass 2) always beats the pty readers** - a shot
  that needs real shell output in the grid captures blank. Set `STDUSK_SHOT_SETTLE_MS` (sleeps
  in `Stdusk::new`, BEFORE the first pass, so the demo shells' output lands first).
- **Keyboard a11y is mostly free in egui 0.35 - visibility isn't.** Every `Sense::click`
  widget is Tab/Shift+Tab AND arrow-key focusable (geometric traversal) and Space/Enter
  "clicks" it; what egui does NOT do is show that focus on custom-painted widgets. Every new
  hand-painted primitive must call `ui::focus_ring`. The ring reads `memory.has_focus`, not
  `Response::has_focus()` - the latter is viewport-focus-gated and reads false in the
  screenshot harness (and in inactive windows, where macOS keeps rings visible).
- **The eframe screenshot PNG contains PASS 1's render.** State set during pass 1's widget
  pass (e.g. `request_focus` after a widget drew) paints only in pass 2 - invisible in the
  capture. Pre-seed before the widget draws (`ui.next_auto_id()` + `memory.request_focus`,
  see `STDUSK_SHOT_FOCUS`).
- **A focused `TextEdit`'s event filter claims arrow keys**, so arrows never move egui focus
  away from a search field - that's what makes the dropdown keyboard highlight (popup state,
  not widget focus) coexist with live filtering.
- **font-kit on macOS lies about faces**: `select_best_match("Menlo", regular)` returns Menlo
  *Italic*, and `Font::properties()` reports `Italic, w400` for EVERY Menlo face. Only
  `full_name()` (and the handle's bytes + `.ttc` face index) are trustworthy - pick faces by
  name (`face_name_score`), and carry `font_index` into `egui::FontData.index`. Its `matching`
  module is private, so the scoring is ours.

## Decisions log
- Splits (M8) + scrollback search (M7) are v1 must-haves (user).
- Quake hotkey is configurable; default `Ctrl+\``; user overrides to `F13` in config.
- Quake uses `global-hotkey` (Carbon) - no macOS Accessibility grant (the skhd route was a
  dead end; Ghostty can't do native tabs in its quick terminal; kitty looked too plain).
- First-party AI agent is the "better than Tabby" differentiator (§4i).
- Electron Tabby source stays in-tree (the `tabby-*` dirs) untouched as the reference implementation.
- **Functional-first**: deep UX/visual polish (spacing, animations, quake drop anim, tab
  separators, font tuning) is deferred to a dedicated pass (~M11). Ship behavior, then beauty.
- Tab bar look confirmed: flat tabs + per-tab colored underline + top-edge progress (Tabby-style).

## 0.1.0 shipped (2026-07-17)
**stdusk 0.1.0 is released and brew-installable.** `brew install hobo-ware/tap/stdusk` verified
end-to-end (downloads the universal `.app`, installs, symlinks `stdusk`, `--version` = 0.1.0).
- **Brand icon**: dusk-sun-over-shell-prompt mark -> app-icon tile (`native/assets/stdusk-icon.png`),
  window icon (eframe `with_icon`), README logo (`stdusk-logo.png`), `.icns` for the `.app` bundle.
- **README**: root `README.md` (rust branch) rewritten in the Hobo-Ware voice ("the machine speaks
  back" / "tools for the discerning degenerate"); `native/README.md` refreshed to dev doc.
- **Release pipeline**: `.github/workflows/native-release.yml` on a `stdusk-v*` tag builds a
  universal (arm64+x86_64 lipo) binary, wraps it in `stdusk.app` (icns + Info.plist), zips via
  `ditto`, cuts the GitHub Release, and generates the Homebrew formula with the real sha256.
- **Tap**: `Hobo-Ware/homebrew-tap` created, `Formula/stdusk.rb` live.
- **Gotcha (fixed)**: Homebrew strips the single top-level dir in the zip and descends into
  `stdusk.app/`, so the formula must `(prefix/"stdusk.app").install "Contents"` (NOT
  `prefix.install "stdusk.app"` - that ENOENTs). Workflow + reference formula + tap all corrected.

## 0.2.1 - palette, drag-reorder, ligatures, profiles, settings GUI
Five parallel builder agents + integration. Cmd+Shift+P command palette (fuzzy scorer tested);
drag-to-reorder tabs (pure midpoint math tested); symbol ligatures (`ligatures`, render-only
substitution - egui can't shape OpenType); named `[[profiles]]` (shell/args/cwd~/env/color,
resolution tested) launchable from tab menu / '+' right-click / palette; settings GUI on the
gear (live-apply, Save serializes Config - round-trip tested). Gotcha: cherry-picking parallel
work can silently drop hunks in 3-way merges - verify each rebuilt commit compiles in a worktree.

## 0.2.2 - theme-derived widget visuals + Tabby-grade settings view
`apply_theme` built every widget from egui's dark base, so light themes got dark controls with
dark text (unreadable settings). Visuals now start from the matching light/dark base and derive
all widget fills/strokes from the theme (elevated/hover/border/selection/accent). Appearance
rows are honest: follow-system mode exposes light/dark pickers (the fixed theme is ignored
there - the "dracula but light" confusion), manual mode exposes the theme picker. Settings is
now a full central VIEW (`settings.rs`, like Tabby's settings tab): icon sidebar + pinned
version, grouped content pane, footer with config path + Revert/Close/Save. The scheme browser
searches all 193 schemes, renders each row on its own background with a 16-swatch palette
strip, live terminal preview follows hover, click applies instantly (follow-system aware:
writes the light or dark slot per the current OS appearance). `--screenshot-settings PATH`
renders the whole view headless; verified dark AND light. Gotcha: egui `Grid`'s first-pass
sizing discard blanks the pass-2 screenshot capture - fixed-width label columns instead.

## 0.2.3 - tab/pty input regressions fixed + settings UX batch
- **Regression A (all tab interactions dead)**: the drag-reorder overlay was a drag-only
  interact registered ON TOP of each tab; egui's hit test returns click:None when the topmost
  widget only senses drags, starving click/double-click/context-menu/close-x underneath. Fix:
  the tab is ONE `click_and_drag` widget (stable tab-id), reorder gates on
  `is_decidedly_dragging`. Headless `Context::run_ui` regression tests cover click,
  drag-reorder, and close-x (`ui.rs`).
- **Regression B (keys "not reaching the shell")**: an open-but-unfocused find bar black-holed
  all terminal keys, and the 0.2.2 settings view made an open search invisible + undismissable
  (Esc deadlock). Capture is now focus-aware (pure `pty_input_captured`, table-tested + a
  headless end-to-end backspace test); opening settings dismisses the find bar.
  Delete/Insert/Home/End/PageUp/Down now map to CSI.
- **UX batch**: always-on-top when hide-on-blur is off + `quake.unfocused_opacity`; live hotkey
  re-registration + live height/tray/dock-policy apply ("applies to new tabs" hints);
  searchable theme dropdowns (all three slots); shortcut tooltips + Cmd+, opens settings;
  action toasts (copied/pasted/theme/zoom/hotkey); live cursor-style preview; unsaved-changes
  guard (Save/Discard/Keep editing); close-busy-tab confirm (`procwatch::busy_child`, opt-out
  `warn_on_close_running`); CLI badges are compact brand-color initial chips BEFORE the title -
  structurally unable to overlap the close-x. 129 tests green, both screenshot harnesses verified.

## 1.0.3 - "Operable & readable": keyboard a11y + theming pass

### Keyboard a11y - settings fully keyboard-operable
User ask: "dropdown options should be keyboard-navigable; form controls in settings need
proper a11y." Audit outcome: egui 0.35 already gives every `Sense::click` widget Tab/Shift+Tab
+ geometric arrow-key focus traversal AND Space/Enter activation (context.rs keyboard click) -
the real gaps were (a) focus was INVISIBLE on every hand-painted primitive, (b) the searchable
dropdown popups had zero keyboard support, (c) `num_field` ignored arrows. Files: `ui.rs`,
`settings.rs` only (parallel theming agent owned colors/config/assets).
- **`ui::focus_ring(ui, &resp, radius)`** - the one accent focus indicator, painted by every
  hand-painted primitive: toggle_switch, chip, color_swatch (focus ring outranks selected/
  hover), stepper_button, icon_button/icon_toggle, action_button, slider, the searchable-
  dropdown button, dropdown/nav/link/scheme/profile rows, inline_icon. It reads
  `memory.has_focus`, NOT `Response::has_focus` - the latter is ALSO gated on viewport focus
  (macOS keeps a control's ring visible in inactive windows, and the shot harness window is
  never focused). `TextEdit` keeps egui's own accent outline; sliders nudge on arrows natively.
- **Searchable dropdowns (scheme + font)**: ArrowUp/Down move a keyboard highlight over the
  filtered rows (wrapping; `SettingsState.dropdown_hl` - popup STATE, not widget focus: the
  search field keeps focus because a TextEdit's event filter claims arrow keys, so typing
  keeps filtering mid-navigation), the list scrolls to follow, Enter commits the highlight
  (falls back to the TOP match when none - type-and-Enter picks the first hit), Esc closes
  without committing. Pure `move_highlight`/`commit_index` are table-tested. The scheme
  dropdown's keyboard highlight feeds the same live preview card as pointer hover. A query
  change resets the highlight. Enter is honored only while the search field (or nothing)
  holds focus, so Enter on a Tab-focused chip/row activates that widget, never double-picks.
- **`num_field`**: Up/Down step by `step` while the field has focus; Shift steps 10x.
- **Esc ordering** (dropdown closes first, settings second) was already correct - the view
  samples `dropdown_open`/`field_focused` BEFORE the panels run; left untouched.
- **`STDUSK_SHOT_FOCUS=1` + `--screenshot-settings`**: pre-seeds keyboard focus on the active
  nav row so the ring is screenshot-capturable. GOTCHA (new): the eframe capture contains
  PASS 1's render - focus requested AFTER a widget draws paints its ring only in pass 2,
  which the PNG misses. The knob requests focus on `ui.next_auto_id()` BEFORE drawing the row.
- Tests +9: ui `space_and_enter_toggle_a_focused_switch` /
  `tab_moves_focus_from_a_text_field_to_the_next_primitive` /
  `arrow_keys_move_focus_along_a_chip_row` / `arrows_step_a_focused_num_field` /
  `arrow_keys_nudge_a_focused_slider`; settings `highlight_moves_and_wraps` /
  `commit_falls_back_to_the_top_match` /
  `dropdown_arrows_move_the_highlight_and_enter_commits` /
  `dropdown_typing_keeps_filtering_and_esc_closes_without_committing`. Suite green, clippy
  -D warnings + fmt clean, all settings-section screenshots re-verified (+ the focus-ring shot).

### Theming - min-contrast default, scheme data heal, light-pack expansion
- **`terminal.minimum_contrast` default 1.0 -> 4.0** (Tabby parity; user report: dark-palette
  CELL text illegible on some schemes - the 0.5.0 dim floor covered chrome only). Serde fills
  only ABSENT fields, so a config that explicitly set 1.0 keeps exact-theme cells (asserted:
  `explicit_minimum_contrast_survives_the_default_bump`); any config ever saved via settings
  has the field pinned. config.example rewritten to match.
- **The 4 audit-critical schemes patched in the DATA** instead of dropped (fg nudged with the
  real `ensure_contrast(fg, bg, 4.5)` outputs, provenance comments in the files): C64
  #7869c4->#aea5dc (2.26->4.51), Royal #514968->#7d778e (2.34->4.59), Shaman #405555->#708080
  (2.44->4.69), CrayonPonyFish #68525a->#86757b (2.76->4.55). Asserted:
  `audit_critical_schemes_were_patched_to_aa`.
- **Melange Dark never parsed** - color1-15 lines were missing the `:` separator, so the
  scheme was silently absent since vendoring. Fixed in the asset.
- **Dupe audit** (normalized: lowercase, strip space/-/_): `Parasio Dark` was an IDENTICAL
  typo-dupe of `Paraiso Dark` - dropped, with a rename alias in `colors::by_name`
  ("parasio-dark" -> paraiso-dark) so saved configs keep resolving (asserted:
  `parasio_dark_alias_resolves_to_paraiso`; unknown-name fallback unchanged). Same-normalized
  but DIFFERENT palettes kept as variants: one-half-dark/OneHalfDark,
  one-half-light/OneHalfLight, tokyo-night/TokyoNight, dracula/pack-Dracula. Known quirk: the
  by_name built-in arms shadow pack `onehalflight`/`tokyonight`, so those two rows apply the
  built-in - pre-existing, tracked for a follow-up.
- **15 hand-vendored light schemes** (no network; pack XRDB format, upstream+license header
  per file, all MIT except Tango = public domain): Gruvbox Light, Gruvbox Material Light,
  Catppuccin Latte, Everforest Light, PaperColor Light, Selenized Light, Selenized White,
  Dayfox, Iceberg Light, Flexoki Light, Alabaster, GitHub Light, Edge Light, Tango Light,
  Zenbones Light. Every one: fg/bg >= 4.5, dim floor passes, `theme_is_dark` = light
  (asserted: `vendored_light_schemes_classify_light_and_meet_aa`). Pack split 24/169 ->
  39 light / 169 dark; browsable total 193 -> 208 (README + config.example + themes.rs
  counts updated).
- Verified in an isolated worktree while the a11y pass ran in the main tree, then combined:
  `--screenshot` with `theme = "gruvbox-light"` via temp-HOME config (light bg +
  light-derived chrome).

## 1.0.2 - "Answer the terminal": query reporting, stuck-tab heal, rename clearing
Three user-reported bugs, root-caused against real-pty captures of the actual CLIs (gemini,
copilot 1.0.71). 216 tests, clippy -D warnings, fmt, both screenshot harnesses verified.
- **Bug 1 - CLIs render dark-theme colors on a light theme** (`terminal.rs`, `colors.rs`).
  Root cause: `EventProxy` handled ONLY `Event::Bell`; alacritty answers OSC 10/11/12 + OSC 4;n
  color queries by emitting `Event::ColorRequest(index, formatter)` and DA1/DA2/DSR/DECRQM/
  CSI 18t reports as `Event::PtyWrite` - all dropped, so every query went silent. A real-pty
  capture shows gemini sends `OSC 11;?` at startup (copilot sends OSC 10/11 + all 16 OSC 4
  queries + `CSI ?u` + `DECRQM 12`); unanswered, these CLIs assume a DARK terminal and paint
  light text on the light bg. Fix: `send_event` queues `PtyWrite`/`ColorRequest` into a
  `Reply` vec (it runs inside the term lock - no IO there); the reader thread drains it right
  after `parser.advance`, resolving colors as app-set `term.colors()[i]` override first, else
  `colors::query_color(i)` from the LIVE theme (0-255 palette, 256/257/258 = fg/bg/cursor),
  and writes the replies through the now-`Arc<Mutex<_>>`-shared pty writer. Belt-and-braces:
  `COLORFGBG` ("0;15" light / "15;0" dark, `colors::colorfgbg()`) set at spawn. Deliberately
  NOT answered: kitty `CSI ?u` (we don't encode CSI-u keys - staying silent makes apps
  correctly fall back to legacy input; replying would advertise support we don't have) and
  `TextAreaSizeRequest` (CSI 14t pixel size - we'd have to invent cell pixel metrics; the
  chars variant CSI 18t IS answered via PtyWrite).
- **Bug 2 - tab "stuck" after Ctrl+C kills copilot** (`terminal.rs`). Two real leaks found
  (copilot 1.0.71 itself exits cleanly on double-^C - captured: full rmcup/cnorm/mouse-off
  teardown; no orphan processes; procwatch clean):
  1. *Stale title*: copilot sets its title via `OSC 0` but RESTORES it via the xterm title
     stack (`CSI 22;0t` push / `CSI 23;0t` pop). Only alacritty's `Event::Title`/`ResetTitle`
     see the stack; our OSC-scanner-only path left "GitHub Copilot" on the tab forever. Fix:
     titles now flow through the Term's events (scanner's `OscEvent::Title` ignored - one
     source of truth); pop restores the pre-app title.
  2. *Abnormal-death mode leak* (the general "frozen pane" case: SIGKILL/crash skips
     cleanup): leaked ALT_SCREEN + hidden cursor make the pane look dead. Heal trigger is
     IN-BAND: the next OSC 133;A prompt mark proves the shell owns the pty again (ordering is
     exact in the byte stream - no fg-pgrp polling race); if the term is still on the alt
     screen -> `swap_alt()` back + send Ctrl-L so the shell repaints the prompt it may have
     drawn on the abandoned alt grid; if the cursor is hidden -> `set_private_mode(ShowCursor)`.
     Justified NOT reset: bracketed paste (zsh legitimately arms it at every prompt - a reset
     would race it; leaks self-heal at the next zsh prompt), DECCKM/kitty/modifyOtherKeys
     (`key_to_bytes` is a static table that never consults term modes - immune), mouse modes
     (we send no reports). Bonus fix: `grid_snapshot` now honors SHOW_CURSOR (DECTCEM `?25l`)
     - we used to paint a cursor over TUIs that hid theirs.
- **Bug 3 - clearing a rename leaves a broken title** (`ui.rs`, `tabs.rs`, `main.rs`).
  The rename dialog set `renamed = true` even for an empty/whitespace buffer, freezing the
  old title forever; session restore trusted any persisted title the same way. Fix:
  `ui::commit_rename` (trimmed; empty -> `None` = un-rename) shared by the dialog commit and
  session restore, so auto-titling (OSC title > cwd basename) reasserts.
- Tests: 11 new - `query_color` mapping (both themes) + `colorfgbg` (colors.rs);
  `commit_rename` clearing (ui.rs); real-pty e2e for the OSC 11 reply (asserts the live-theme
  bg goes over the wire; script uses `stty raw` - canonical mode holds the reply hostage),
  DA1+DSR replies, title-stack pop, hidden-cursor snapshot, alt+cursor heal on prompt mark,
  heal no-op on a healthy prompt, and vim + less enter/exit mode-cleanliness sweeps.
- Gotcha (new): e2e scripts that READ a query reply must `stty raw -echo` first - the pty is
  canonical by default, so `head -c N` blocks until a newline that never comes, and echo
  feeds our own reply back through the parser.

## 1.0.1 - "Right-side relevance": tab trailing slot, real bold faces, pre-filtered theme dropdowns
Three user-requested items. 205 tests, clippy -D warnings, fmt, screenshot harnesses verified.
- **Tab trailing slot** (user: "things always on the left waste space, worse UX"): `draw_tab`'s
  LEADING slot is gone - no space is ever reserved by default and the title gets the full
  width. A TRAILING (right-edge) slot exists only while relevant: the CLI brand badge while an
  AI CLI runs in the tab, swapped for the close-x while the tab is hovered (close wins). The
  pinned push-pin shifts just left of the slot when it's shown. Slot presence feeds the width
  math via LAST frame's hover (stored in ctx temp data, `tab_iid.with("hovered")`) - this
  frame's rect isn't allocated yet and a predicted rect oscillates in dynamic width mode; one
  frame of lag, invisible (egui repaints on pointer movement). Fixed-mode tabs keep their
  width (title just truncates a hair more while the slot shows); dynamic-mode tabs grow by the
  slot while hovered/CLI-active - accepted per the ask (never permanently reserved).
  Drag/reorder + context menu + tab-first-interact ordering (x registered after, wins its
  clicks) all unchanged; headless tests updated for the trailing geometry + a new
  `close_x_replaces_the_badge_while_hovered` (badge tab: hover swap, close beats focus).
- **Real bold font faces** (the last unshipped V1 P1; 0.3.1 deferral closed): `build_fonts`
  now takes a second `Option<ResolvedFont>` and, when the user's family resolves an upright
  Bold sibling, registers `FontFamily::Name("term-bold")` = [bold face, then the whole
  Monospace stack] so a glyph missing from the bold file degrades to regular, never tofu.
  Resolution mirrors the regular face's name-scoring (`bold_face_name_score`: must say "bold",
  slants disqualify, Semi/Extra/width variants rank behind plain Bold - core-text properties
  still lie). `CellSnap.bold` carries the raw SGR BOLD flag (independent of `bold_bright`,
  which stands unchanged); `render_grid` switches family per glyph when `bold_font` is passed
  (workspace gates on `Stdusk.bold_font_ready`, kept in sync by startup + `reapply_font`, so
  the settings live-apply path carries it). Cell metrics stay derived from the regular face
  (a bold glyph may run a hair wider - Tabby-equivalent). The BUNDLED default has no bold
  sibling in assets/ - the bold family only exists for user fonts that ship one. Pixel-proof:
  temp HOME + `font="Menlo"` + `bold_bright=false` + a $SHELL script printing the same line
  plain and under `\e[1m` - 2196 px differ, all inside the bold line's bbox (heavier strokes,
  same columns). Needed a new harness knob: **`STDUSK_SHOT_SETTLE_MS`** sleeps in `Stdusk::new`
  before the first pass, because eframe captures at cumulative pass 2 which always beats the
  pty readers (an empty grid otherwise - the 0.3.1 "prompt glyphs" diff wouldn't reproduce).
- **Appearance theme dropdowns pre-filter by slot brightness** (user ask): `scheme_dropdown`
  takes its target `SchemeSlot`; the popup opens pre-filtered via pure `slot_bright_filter`
  (Light slot -> light schemes, Dark -> dark, manual fixed Theme -> All) with an All/Light/
  Dark chip row inside the popup as the escape hatch (`SettingsState.dropdown_bright`, reset
  on every open); search combines with the filter. Same `theme_is_dark` + `bright_allows`
  machinery as the scheme-browser chips. Screenshot plumbing: `STDUSK_SHOT_DROPDOWN=<id_salt>`
  force-opens a dropdown in the settings shot (`theme_light`/`theme_dark` also flip
  follow_system on with both slots pinned) - but the popup is an `egui::Area`, which the
  2-frame harness never renders (known gotcha), so the open state is plumbed + the filter
  logic is unit-tested rather than captured.


Ships everything from the 1.0.0-rc prep section below plus the post-0.5.0 addenda
(scheme-brightness filter chips + auto pre-filter, a11y dim-text 3:1 floor across all
194 schemes). Released UNSIGNED: the signing/notarization scaffold is live in CI but
dormant until the five Apple Developer secrets exist (see packaging/README.md); the
cask's quarantine-strip postflight remains the stopgap and drops automatically on the
first signed release. 199 tests, clippy -D warnings, fmt, all 10 screenshot harnesses
green at tag time. Remaining work is the human-verify shortlist (below) - none of it
blocks the tag; regressions there would have been caught by the automated layers.

## 1.0.0-rc prep - adversarial sweep, signing scaffold, verify pass (version stays 0.5.0)
V1.md's final milestone, minus the tag: the 1.0.0 bump is a coordinator decision. Version
stays 0.5.0; everything below is on the tree ready for the release commit.
- **Adversarial sweep of 0.4.0-0.5.0** (right-click press tracking, broadcast fan-out,
  pin/move boundary math, hotkey matcher, autosync, scrollback wipe, aggregation fns).
  Three confirmed bugs, fixed with regression tests; everything else survived scrutiny:
  1. **Cmd+C/X/V `[hotkeys]` binds were silently dead** - egui-winit folds ANY Cmd-modified
     C/X/V press into `Event::Copy/Cut/Paste` and returns before pushing the Key event
     (the M6.5 gotcha, now load-bearing), so such a bind could never fire - exactly the
     "silently dead bind" the Hotkeys struct design promises never happens.
     `ui::parse_hotkey_spec` now REJECTS them (red field + "Invalid hotkey" toast);
     `cmd_clipboard_chords_are_rejected_as_unmatchable`.
  2. **Cmd+K / "Clear Terminal" on the alt screen wiped the app's grid + mailed it a `^L`**
     (a literal insert in vim's insert mode). `PtyTerm::clear_all` now returns `bool` and
     refuses on `ALT_SCREEN` (the grid there belongs to the app; there's no scrollback to
     drop); callers send Ctrl-L only on `true`. `real_pty_clear_all_is_refused_on_the_alt_screen`.
  3. **The launch-autosync pull could clobber concurrent local changes**: a slow
     `git fetch + reset --hard` can land minutes after launch; the handler blindly
     `Config::load()`d + rebaselined, discarding a Save (already hard-reset on disk) or
     live settings edits. The launch pull now snapshots the config TOML at spawn
     (`Stdusk.launch_pull_cfg`); a result arriving after the config changed is skipped
     (`sync::pull_is_stale`, tested) and the LOCAL version is written back to disk (the
     reset already replaced it) with a "Sync pull skipped (local changes)" toast. Manual
     Pull passes no baseline - overwriting local stays its whole point.
  - Verified-clean (no code change): right-press path tracking (stale press dropped on
    off-pane release), broadcast exit-on-switch sweep + confirmed-paste fan-out,
    `pin_target`/`moved_index` boundary math, first-match-wins hotkey precedence,
    aggregation fns, wipe-before-Ctrl-L ordering, push-failure toast path. Known accepted
    edges: a rebind onto a FIXED chord (Cmd+1-9, Ctrl+Tab) double-fires app-side (cousin of
    the documented terminal-chord collision); the scheme-filter + a11y-dim addenda reviewed
    clean.
- **Signing/notarization scaffold** (`native-release.yml`): a new optional "Sign & notarize"
  step runs when all five secrets exist (`MACOS_CERT_P12`/`MACOS_CERT_PASSWORD`/
  `NOTARY_KEY_ID`/`NOTARY_ISSUER`/`NOTARY_KEY`): throwaway keychain, `codesign --deep
  --options runtime --timestamp`, `notarytool submit --wait` (API key), `stapler staple`,
  `spctl --assess`; missing secrets = a log line + the exact old ad-hoc behavior. The zip +
  sha256 moved AFTER signing (the sha must cover the stapled bits). The generated cask
  drops the quarantine-strip `postflight` when signed (gated on the step's `signed`
  output; both variants dry-run verified). Full cert-export + App Store Connect API key
  setup documented in packaging/README.md.
- **Verify sweep (all green)**: 199 tests (196 + the 3 regression tests above); clippy -D
  warnings; fmt; `--version` = 0.5.0; all 10 screenshot harnesses exit 0 and were visually
  checked (default incl. pin glyph + badges, broadcast borders, every STDUSK_SHOT_SECTION
  incl. profiles/hotkeys + the new scheme-brightness chips); fresh HOME-override e2e:
  corrupt config.toml -> defaults + clean launch, corrupt session.toml -> clean launch,
  complete user XRDB scheme -> applied w/ adaptive chrome (NOTE: `parse_xrdb` requires
  color0-7 - a partial scheme file falls back by design).
- **Human-verify shortlist for the 1.0.0 tag** (everything else is automated): live quake
  toggling (global hotkey + hide-on-blur), notifications (notify-when-done / on-activity
  osascript), clipboard round-trips (Cmd+C/V pasteboard, OSC 52, middle-click,
  copy-on-select), live AI-CLI badge detection (real `claude`/`gemini`), and - once the
  Apple Developer secrets exist - one signed-build `brew install` (no quarantine strip).
- **1.0 gate**: Developer ID signing is the ONLY remaining external blocker; it needs the
  user's Apple Developer account ($99/yr). The workflow + cask + docs are ready - add the
  five secrets, tag, done.

## 0.5.0 - "Make it yours": profiles editor GUI, hotkey remapping, autosync (V1 P1s)
- **Profiles editor (Settings > Profiles**, sidebar between Terminal and Quake, Phosphor
  identification-badge E6F6): list of configured profiles (color dot + name + shell summary,
  trailing Launch/Duplicate/Delete icons via `inline_icon` - row interacted FIRST so the
  icons, registered after, win their clicks, same ordering as the tab close-x), Add profile,
  and a click-to-edit inline panel: name/shell/cwd (plain `text_field`s; Option fields use a
  per-frame buffer, `"" = None`), args as ONE line parsed by pure `settings::split_args`
  (whitespace splits, '/" quote, backslash escapes; `join_args` renders back, round-trip
  tested), env as key=value `text_field` rows + Add/Remove (blank keys dropped by pure
  `env_rows_to_map`, tested), color = "No color" chip + the `color_swatch` palette (2x6).
  Args/env edit through `SettingsState` buffers (half-typed quotes and blank rows must
  survive re-render) that write into `cfg.profiles` on every change; buffers reload on
  selection change AND after Revert/Discard/sync-pull (`profile_loaded = None` in those
  paths - stale buffers were the failure mode). Launch = `TabAction::NewWithProfile` fx from
  the section (the new tab is visible in the tab bar above the settings view) + toast.
- **`Profile.env` is now a `BTreeMap`** (was HashMap): deterministic iteration = stable TOML
  serialization, so `config_dirty` and Save diffs can't flap on map order. Round-trip incl.
  args/env re-verified (`config_to_toml_round_trips`).
- **Hotkey remapping (`[hotkeys]`)**: a `config::Hotkeys` STRUCT (15 String fields with
  per-field defaults via struct `Default` + `#[serde(default)]`, not a map - a typoed action
  name is an ignored unknown field, never a silently dead bind; empty = unbound) for new_tab
  close reopen toggle_last_tab find palette settings broadcast split_right split_down
  select_all clear zoom_in zoom_out zoom_reset. Pure egui-side matcher in `ui.rs`:
  `parse_hotkey_spec` (own name table incl. punctuation literals "," "=" "-"; "+"/"=" both
  mean `Equals` - shared physical key, pressed `Plus` normalized too, so "Cmd+=" zooms on
  either report; bare/shift-only single keys REJECTED - they'd shadow typing - only F-keys
  bind bare) + `hotkey_matches` (EXACT modifier match: Cmd+T never fires on Cmd+Shift+T;
  garbage/empty never matches). Table-tested incl. garbage, precedence, plus/equals.
- **The main.rs collection block** iterates `i.events` `Event::Key` presses against the map;
  first match wins per event (a user binding two actions to one chord fires only the earlier
  action). palette/settings toggles stay live outside TEXT modals exactly as before; all
  other actions still obey `hard_modal`; the `input_captured` gate on terminal-input actions
  (select_all/clear/scroll) is unchanged. Pane nav/resize (Cmd+Alt/Cmd+Ctrl), tab cycle,
  Cmd+1-9, move-tab, scroll keys stay FIXED (not remappable). Menu hints/tooltips (tab menu
  New tab/Close, Tabs-popup palette row, +/gear tooltips) read the configured chords
  (`ui::shortcut_tip` hides unbound ones).
- **Settings > Hotkeys** (Phosphor keyboard E2D8): grouped rows (Tabs/Panes/Terminal/App),
  each an editable chord field with the default in the row description - red text while the
  spec doesn't parse, "Invalid hotkey" toast on blur (`HotkeysFx.invalid`), Reset-to-defaults
  button. Live capture widget deliberately skipped (stretch goal, >30min).
- **Reserved-combo integrity (READ THIS before touching binds)**: the DEFAULT chords are
  chosen so `key_to_bytes` sends nothing for them (Cmd+letter combos are unmapped; Cmd+O/
  Cmd+K etc. produce no pty bytes) - no double-fire out of the box. A USER rebind onto a
  terminal-bound chord (e.g. Ctrl+K) fires the app action AND still sends the control byte
  to the pty: the key_to_bytes reservations cover only the exact default modifier
  combinations already handled there, and the collect path does not consult the hotkey map.
  Documented in config.example + the settings intro; asserted in
  `ui::tests::rebound_terminal_chords_double_fire_by_design` so it can't drift silently.
- **Autosync (`[sync] auto`, default false, user addendum)**: with a repo set, ONE background
  pull on launch (spawned in `Stdusk::new` via the existing `sync::spawn`/`SyncSlot`; the
  per-frame sync_done handler applies it like a manual Pull - config reload + theme/hotkey/
  font re-apply; a failed pull toasts once and never blocks startup) and a push after every
  successful settings Save (`save_settings` is the only disk write, so it's THE push hook).
  Pure `sync::should_autosync(auto, repo_set, busy)` (tested) is the gate; `sync_busy` doubles
  as the debounce - rapid saves collapse into the in-flight push, and the manual Push button
  skips its own spawn when Save already kicked one. Design choice: Tabby's config-sync polls
  on a 60s loop; pull-on-launch + push-on-save is the leaner git-appropriate equivalent
  (documented here on purpose). Settings > Session > "Auto sync" toggle, enabled only with a
  repo, hint "Pull on launch, push on save".
- **Scheme browser brightness filter (user ask)**: All / Light / Dark chips next to the
  search field. Classification = `colors::theme_is_dark(&Theme)` (per-theme version of the
  chrome `is_dark` rule; the pack splits ~170 dark / 24 light). Default is the AUTO
  pre-filter: following the system, a pick can only land in the current appearance's slot,
  so the list opens filtered to that brightness (`settings::default_bright_filter`, tested);
  manual mode opens on All. The user can override any time (`SettingsState.scheme_bright`,
  `Option` so None = auto; reset on section re-entry). `bright_allows` (tested) applies over
  the search results; a chip click applies the same frame.
- **A11y audit of all 194 schemes (Haiku agent, WCAG contrast over the XRDB pack +
  built-ins)**: it's a DATA problem - 37% of the pack ships `ansi[8]` (the chrome "dim"
  role) nearly invisible against bg (many at ratio ~1.0: Solarized Dark, Tomorrow Night
  Bright, ...); 64% have SOME ansi color at ~1.0 vs bg; 13 schemes fail fg-vs-bg AA (<4.5),
  4 critically (<3.0: C64 2.26, Royal 2.34, Shaman 2.44, CrayonPonyFish 2.76); built-ins are
  fine except tokyo-night's dim at 1.91. Fixes shipped: **`colors::dim()` now has a 3:1 WCAG
  floor** (pure `legible_dim` = `ensure_contrast(ansi[8], bg, 3.0)`, a no-op when already
  passing - chrome text can never vanish again; `dim_text_meets_the_floor_on_every_scheme`
  asserts it over the whole pack) and **scheme-row names** draw `ensure_contrast(fg, bg,
  3.0)` so the 4 low-fg schemes stay browsable. Terminal CELLS stay theme-exact on purpose -
  per-cell fidelity remains the `terminal.minimum_contrast` opt-in (Tabby ships 4); no
  scheme data was rewritten.
- **Screenshot harness**: `STDUSK_SHOT_SECTION=profiles|hotkeys` added; the profiles shot
  injects two demo profiles (only when the user config has none) and expands the editor
  (`SettingsState::select_profile`). Both verified + the default shot unchanged; the scheme
  browser re-verified with the chips.
- 196 tests green (+17); clippy -D warnings + fmt clean. New tests: config
  `hotkeys_default_to_the_shipped_binds` / `partial_hotkeys_table_keeps_other_defaults` /
  `hotkeys_round_trip_through_toml`; ui `hotkey_matches_exact_chords_only` /
  `hotkey_plus_and_equals_share_the_key` / `garbage_hotkey_specs_never_match` /
  `bare_single_keys_are_rejected_but_fkeys_pass` /
  `rebound_terminal_chords_double_fire_by_design` / `shortcut_tip_hides_unbound_chords`;
  settings `args_split_handles_quotes_and_escapes` / `args_join_round_trips_through_split` /
  `env_rows_drop_blank_keys_and_last_write_wins` /
  `bright_filter_defaults_to_the_slot_being_set` / `bright_filter_partitions_light_and_dark`
  (+ `sections_render_headless` now drives the profiles editor + hotkey rows); colors
  `dim_text_meets_the_floor_on_every_scheme` / `theme_darkness_classifies_builtins`; sync
  `autosync_needs_opt_in_repo_and_an_idle_worker`.

## 0.4.1 - "Panes & tabs polish": broadcast input, aggregated tab state, menu polish (V1 P1s)
- **Broadcast input (Tabby `pane-focus-all`)**: Cmd+Shift+I (Tabby's exact default binding) or
  palette "Broadcast Input" toggles `Tab.broadcast` on the CURRENT tab - keystrokes AND pastes
  (incl. a confirmed multiline paste) fan out to EVERY pane via new `Pane::leaves_mut()`
  (tested). Visual: every pane wears a 1.5px accent border and the unfocused fade is dropped
  (Tabby `_allFocusMode` marks all panes focused, splitTab.component:950). Switching tabs exits
  the mode (an end-of-frame sweep clears `broadcast` on every non-active tab - covers click,
  keybind, palette, close). Mouse paste (middle/right-click) stays single-pane, like Tabby
  (multifocus taps `frontend.input$`, not mouse paste). Cmd+I produces no pty bytes, so no
  `key_to_bytes` reservation needed. **focus-all-TABS (multi-tab broadcast, Cmd+Alt+Shift+I)
  skipped on purpose** - PARITY row says so. Broadcast shot: `STDUSK_SHOT_BROADCAST=1
  --screenshot` splits the demo tab + forces the mode (border screenshot-verified).
- **Aggregated tab progress + cmd state**: the tab bar now folds ALL panes, not just the
  focused one. `tabs::aggregate_progress` (pure, table-tested): an `Error` anywhere wins
  outright; otherwise the active progress with the max fill fraction (Indeterminate counts
  as 1.0; ties keep leaf order). `tabs::aggregate_cmd` (tested): `Fail` on ANY pane shows the
  red mark, else the focused pane's state. Title stays the focused pane's on purpose (cwd/OSC
  of the pane you're in). CLI badge already aggregated via pids.
- **"Running: <name>" tab-menu row** (Tabby tabContextMenu's disabled "Current process" row):
  a disabled first row + separator, fed by `Tab.proc` - cached in the existing ~1 Hz CLI scan
  (never a synchronous scan on menu open; needs `terminal.detect_clis` on, its loop). The scan
  now snapshots the process table ONCE per tick (`procwatch::snapshot` exposed) and runs the
  pure `detect`/`busy_child` per tab on it; the per-pid `scan()` wrapper is gone.
- **Notify on activity** (0.3.0 rider, Tabby's checkbox row): per-tab menu toggle (check glyph
  in the shortcut slot), NOT persisted. The reader thread flags `TabState.activity` on every
  output chunk (`take_activity`, real-pty e2e'd); while the tab is unviewed (not active, or
  window hidden) the first output posts ONE notification via the shared `notify()` osascript
  path (refactored out of notify_done), then re-arms only when the tab is viewed
  (`ui::activity_notification`, pure decision, table-tested). Flags consumed with `|=` per
  pane, never `any()` - a short-circuit would leave stale flags that mis-fire on enable.
- 179 tests green (+5); clippy -D warnings + fmt clean; all three screenshot harnesses
  verified (default unchanged, broadcast border, 0.4.1 in the settings footer). No new config.

## 0.4.0 - "Input & scroll parity": alt+scroll, right-click modes, wipe, tab leftovers (V1 P1s)
- **Alt+scroll -> arrow keys**: the workspace wheel handler sends SS3 arrows (`ESC O A`/`B`,
  one per wheel line) instead of scrolling while Alt is held. Tabby's exact gate is **Alt
  alone** (baseTerminalTab mousewheel handler - NO alt-screen check, despite V1's sketch
  saying otherwise); mirrored exactly. Pure `ui::alt_scroll_bytes` (tested).
- **Line-step scroll**: Ctrl+Shift+Up/Down scroll one line (Tabby's default `scroll-up`/
  `scroll-down` binding). Free bind: `key_to_bytes`' ctrl branch already maps arrows to None,
  so nothing leaks to the pty (regression-tested).
- **`terminal.right_click` = "menu" (default) | "paste" | "clipboard"** (Tabby-exact,
  baseTerminalTab:647-676): menu pops the pane context menu; paste/clipboard act on a quick
  tap - paste pastes (same immediate pipeline as middle-click, no multiline modal), clipboard
  copies-the-selection-else-pastes - and BOTH fall back to the menu on a >=250ms hold.
  Decision table is pure + tested (`ui::right_click_action`). Mechanics: raw Secondary
  press/release tracking (`Stdusk.right_press` (path, time); egui wouldn't report a long hold
  as a click at all), and the pane menu is now `egui::Popup::menu(&resp).open_memory(cmd)
  .at_pointer_fixed()` opened on OUR decision instead of `resp.context_menu` (which hardwires
  egui's secondary-click). Settings > Terminal > Mouse chips row.
- **`terminal.focus_follows_mouse`** (default false = Tabby default): pointer MOVEMENT over an
  unfocused pane focuses it (Tabby splitTab attaches a mousemove handler; hover alone doesn't
  refocus). Suppressed while any button is down so selection/splitter drags crossing panes
  can't steal focus mid-gesture. Settings > Terminal > Mouse toggle.
- **True scrollback wipe**: Cmd+K (and palette "Clear Terminal") now wipes viewport + history
  via `PtyTerm::clear_all` (`grid_mut().reset_region(..)` + `clear_history()`) BEFORE sending
  Ctrl-L. Order matters: alacritty's `ESC[2J` handler (`clear_viewport`) scrolls occupied
  viewport lines INTO history, so a wipe after the shell's redraw would resurrect a screenful.
  New palette command **"Clear Scrollback"** (`clear_scrollback`) drops history only, screen
  kept. Both real-pty e2e'd (`real_pty_clear_all_wipes_history_and_viewport`,
  `real_pty_clear_scrollback_keeps_the_screen`).
- **toggle-last-tab**: Cmd+O + palette "Toggle Last Tab". `Stdusk.prev_active` is maintained by
  an end-of-frame diff in `ui()` (so every switch path - click, Cmd+N, cycle, palette, close -
  counts without per-site bookkeeping). Index-based exactly like Tabby's `lastTabIndex`
  (stale-after-close clamps to tab 1: `ui::toggle_last_target`, tested). **Tabby ships this
  hotkey UNBOUND on every platform** - Cmd+O is stdusk's conflict-free choice (documented in
  TESTING; Cmd+O produces no pty bytes, so no key_to_bytes reservation needed).
- **pin-tab**: context-menu Pin/Unpin. Tabby-exact placement (`tabs::pin_target`, table-tested
  against app.service pinTab/unpinTab): pin moves the tab to the END of the pinned group,
  unpin to the START of the unpinned group; the active index follows via `ui::moved_index`
  (tested). Reorder never crosses the pinned boundary (guard in `move_tab`, which both drag
  and Cmd+Shift+arrows route through - Tabby's swapTabs refuses the same way). Closing a
  pinned tab ALWAYS confirms ("This tab is pinned.", even with warn_on_close_running off) -
  deliberate deviation: Tabby hard-refuses the close; a confirm keeps it reachable.
  `pending_close` now carries the prompt message (`ui::close_confirm_message`, tested);
  close-others/right/left skip the guard (scope-noted). Pin glyph = Phosphor push-pin
  **E3E2** (official CSS + cmap-verified) at the tab's right edge, title budget shrinks by its
  width. Persisted as `SavedTab.pinned` in session.toml; Restart keeps the pin, Duplicate
  doesn't copy it.
- **Tabs 10-20 skipped on purpose**: Tabby ships `tab-10`..`tab-20` UNBOUND by default
  (configDefaults yaml) and Cmd+0 is zoom-reset here - nothing to mirror; PARITY row says so.
- 174 tests green (+10); clippy -D warnings + fmt clean; both screenshot harnesses verified
  (pin glyph on the demo tab, 0.4.0 in the settings footer). NOTE: the new Mouse settings rows
  sit below the 760px settings-harness fold (macOS clamps taller windows); they execute in
  `sections_render_headless` and reuse the proven row/chip/toggle primitives.

## 0.3.2 - "Render right": wide chars + min contrast + all-match search + brand badges (V1 P0-4 + P1s)
- **Wide-char rendering (P0-4)**: `grid_snapshot` now honors alacritty's cell flags via pure
  `terminal::snap_glyph(c, flags)` - a `WIDE_CHAR` cell marks `CellSnap.wide`; `WIDE_CHAR_SPACER`
  / `LEADING_WIDE_CHAR_SPACER` cells emit `'\0'` (no glyph; bg/selection stay per-cell).
  `render_grid` draws a wide glyph horizontally centered across its 2 cells (`CENTER_TOP` at
  `pos.x + cw` - top-aligned like its neighbors), and the block cursor covers both cells over a
  wide glyph (glyph redrawn centered in bg color). Selection/link column math untouched -
  alacritty's grid already counts the spacer as a cell. Real-pty e2e: printf'd 你好 + 😀 land as
  wide+spacer pairs in the snapshot (`real_pty_snapshot_marks_cjk_and_emoji_wide`).
- **`terminal.minimum_contrast`** (default 1.0 = OFF - existing users keep their exact theme;
  Tabby ships 4): pure `colors::ensure_contrast(fg, bg, ratio)` nudges a cell's fg toward black
  or white (whichever side of the bg has more WCAG headroom) until the contrast ratio is met.
  Stepped blend, NOT a bisection - contrast isn't monotonic when the blend crosses the bg's
  luminance. Applied in `render_grid` per glyph vs its effective bg (theme bg when the cell has
  none), before the unfocused-pane fade; free when off. Settings > Appearance > Text slider
  1.0-21.0 ("1 = off"). `colors::contrast_ratio` tested on known pairs (#767676-on-white ≈ 4.54).
- **All-match search highlight**: every find-bar hit gets a dim accent wash
  (`colors::search_match()`, accent @ alpha 45) painted over the glyphs; the CURRENT match keeps
  its brighter selection fill, so it stands out. Pure `search::visible_matches(matches, top_line,
  rows, cols)` maps buffer lines -> viewport rows, drops off-screen/off-grid hits, clamps at the
  right edge (tested). `render_grid` takes the marks as a slice; the workspace passes the find
  state's matches to the FOCUSED pane only (`&[]` elsewhere).
- **Real brand icons for CLI badges** (replacing initial-letter chips where possible): official
  monochrome SVGs vendored from Simple Icons (CC0-1.0, simpleicons.org) into
  `assets/icons/{anthropic,googlegemini,githubcopilot,ollama,cursor}.svg`. **Codex + Aider keep
  the letter chip** - OpenAI's icon was removed from Simple Icons upstream (404) and aider never
  had a slug. Rasterized at runtime via **resvg** (default-features off - paths only): parsed
  once per (cli, px) into a WHITE glyph (`rasterize_white` - RGB forced white, alpha from the
  render) so `painter.image(tint = cli.color())` brand-colors it; 2x pixel size for retina
  crispness; cached as a `TextureHandle` in egui's per-context temp data (`cli_icon_texture`) so
  headless tests and the app never share GPU handles. Any parse/render failure falls back to the
  chip. Asset-validity test rasterizes all five vendored SVGs (solid pixels must be pure white).
- Screenshot-verified dark AND light (Anthropic mark in clay on the claude tab, Gemini spark in
  blue) + both settings harnesses. 164 tests green.

## 0.3.1 - "Your font": custom font family + line padding (V1 P0-2)
- **`appearance.font`** (default "" = bundled default): a font FAMILY name ("Menlo",
  "JetBrainsMono Nerd Font") resolved to file bytes via **font-kit** (core-text source) and
  inserted as the TOP font of the **Monospace family only** - chrome text stays bundled, and
  every fallback (NotoEmoji + Arial Unicode + Apple Symbols) stays behind it so emoji/symbols/
  Nerd-Font-missing glyphs keep rendering. `appearance.line_padding` (0-8 px, default 0) pads
  the grid cell height via pure `ui::padded_cell_height` (workspace.rs metrics; pty rows follow).
- **Font resolution is hand-rolled per face** (`main.rs resolve_font` + `face_name_score`):
  font-kit's `select_best_match` returned Menlo *Italic* for "Menlo" on macOS, and core-text
  `Font::properties()` is broken too (every Menlo face reports `Italic, w400`) - so we
  `select_family_by_name` and pick the face whose `full_name()` scores closest to upright
  regular (keyword penalties: italic/oblique >> bold/black/thin > medium). The resolved
  `Handle` carries a `.ttc` face index -> `egui::FontData.index`. Regression-tested against
  the real system: "Menlo" -> "Menlo Regular", "NoSuchFontXyz" -> None (#[cfg(macos)] tests).
- **Startup + live-apply share `build_fonts(Option<ResolvedFont>)`**: `Stdusk::reapply_font`
  rebuilds + `ctx.set_fonts` when `appearance.font` differs from `applied_font` (commit on
  field blur / dropdown pick / Save / Revert / Discard / sync pull). Unresolvable name = "Font
  not found: <name>" toast, current fonts kept, never a crash (same path at startup).
- **Settings > Appearance > Text**: Font text field (applies on lost focus), "Installed fonts"
  searchable dropdown (font-kit `all_families()`, sorted; leading "Default (bundled)" reset
  row), Line padding slider. The scheme + font dropdowns now share one `searchable_dropdown`
  scaffold + `dropdown_row` painter (settings.rs); `filter_names` is the case-insensitive
  name filter (tested).
- **Bold faces deferred**: egui has ONE face per family - a real Bold face would need a second
  family + per-cell font switching in render_grid. `bold_bright` color treatment stands.
- Pixel-proof: `--screenshot` under a temp HOME with `font="Menlo"` differs from the default
  capture by 6748 px (prompt glyphs only, verified upright); `line_padding=6` differs by 192 px
  (taller cursor cell). 156 tests green; both screenshot harnesses exit 0.

## 0.3.0 - "No dead ends": shell-exit behavior + OSC 0/2 dynamic titles (V1 P0-1 + P0-3)
- **Shell exit is observed, not ignored** (the dead-frozen-tab bug): the pty child handle now
  moves into the reader thread (it was dropped at spawn); on read EOF/err the thread reaps the
  REAL exit code (`child.wait()` returns promptly once the fd closed) into
  `TabState.exited = ExitInfo { code, uptime_secs }` and repaints.
- `terminal.on_exit = "close" (default) | "keep" | "restart"` (Settings > Terminal > Behavior
  chips). Decision logic is pure + table-tested (`terminal::exit_action`/`on_exit_mode`):
  close = the PANE closes (tab on its last pane; the last tab respawns fresh via `close_tab` -
  never a zombie); keep = dim "[process exited: code]" overlay (`ui::draw_exit_overlay`), Enter
  (focused) or click respawns in the same cwd; restart = in-place respawn with a crash-loop
  guard - two consecutive deaths within `RAPID_EXIT_SECS` (2s) of spawn fall back to keep
  (`PtyTerm.rapid_exits` carried across respawns by `tabs::respawn_term`).
- `Stdusk::handle_shell_exits` applies ONE structural action per frame (a close invalidates the
  other collected paths; the queued repaint drains the rest). `pane::leaf_paths()` added (tested).
- **OSC 0/2 dynamic tab titles**: `OscScanner` emits `OscEvent::Title` (OSC 1 icon-only + bare
  `]0` ignored; empty title = reset); the reader stores `TabState.title_osc`; auto-titling is
  user rename > OSC title > cwd basename (`ui::auto_title`, tested) behind
  `terminal.dynamic_title` (default true; live Settings toggle - gated at consumption, so no
  respawn needed). The chunk-split proptest invariant covers the new event for free.
- Real-pty e2e (inline `#[cfg(test)]`, real `/bin/sh` - a tests/ dir can't reach a binary
  crate's internals): `sh -c 'exit 3'` reports code 3 + a sane uptime; a printf'd OSC 0 title
  propagates to `title_osc`. 148 tests green; both screenshot harnesses verified.

## 0.2.5 - color-support env, settings redistribution, tab-bar rework
- TERM=xterm-256color + COLORTERM=truecolor + TERM_PROGRAM=stdusk on every spawn (Finder launches
  had NO TERM -> child programs disabled ANSI colors entirely). Profile env still wins (map insert).
- Settings regrouped by concern (Appearance owns all visuals incl. cursor/ligature previews +
  unfocused-opacity; Terminal = behavior only); ui::num_field/slider replace DragValue; theme
  dropdowns hover-preview; scrollbar pinned (ScrollStyle::thin; solid() collapses here).
- Tab bar: Panel::top sizes fill/clip at its own estimate -> pin with exact_size or the fill stops
  short (the underline dead-band root cause). appearance.tab_width fixed|dynamic (default fixed).
  Close-x hidden by default; hover swaps the leading CLI chip for the x (overlap impossible).
- Adversarial pass: claims verified; a hover-order test rewritten + 2 interaction tests added.
- V1.md is the roadmap to 1.0 (P0: shell-exit dead tab, font family/Nerd Font, OSC 0/2 titles,
  wide chars, signing). 141 tests.

## 0.2.4 - settings sync via git + polish
`[sync] repo` (Settings > Session > Sync) points at your own (private) git repo; Push/Pull
moves `~/.config/stdusk` (config.toml + custom schemes) with the SYSTEM git + your existing
credentials - no OAuth, no stored tokens. The git command plan is pure and unit-tested
(`sync.rs`: bootstrap/commit/push, fetch/hard-reset pull, tolerant steps); execution runs on a
background thread reporting through a polled slot; Pull reloads + re-applies config live
(theme, hotkey). session.toml + generated shell hooks are gitignored - they never leave the
machine. Polish: tab Color menu previews the hovered swatch live on the tab;
`colors::hover_elevated()` fixes invisible hover fills on elevated surfaces;
`--screenshot-settings` can target any section via `STDUSK_SHOT_SECTION`. 135 tests green.

## Supreme pass (0.2.0) - Tabby-parity input/paste/render suite + session restore + themes
Multi-agent pass: Tabby re-audit (exact paste/copy semantics extracted from
baseTerminalTab.component.ts) + code-audit agent (found the grid-dims bugs below) + theme-import
builder agent; implementation + integration here.
- **Bug fixes (regression-tested where pure)**: `grid_snapshot`/`buffer_lines`/`select_all` used
  `self.cols/rows`, which diverge from the REAL grid dims when an app resizes via CSI 8 -> OOB
  panics in the renderer. Grid dims (`grid.columns()/screen_lines()`) are now authoritative.
  `resize()` condition parenthesized.
- **Paste pipeline (Tabby-exact, `ui::normalize_paste`/`trim_paste`, tested)**: CRLF/LF -> CR;
  optional newlines->spaces (`replace_newlines_on_paste`); multiline + `warn_on_multiline_paste`
  + NOT alt-screen -> confirm modal (preview, Paste/Cancel, Enter/Esc; modal path skips trim);
  else trim rules (`trim_whitespace_on_paste`, default ON: end always, start only single-line).
- **Intelligent Ctrl-C** (Tabby): selection -> copy + clear; else SIGINT (`collect_input`
  intercept flag).
- **Input**: `alt_is_meta` (Option+letter -> ESC+letter, Text events suppressed while Alt held;
  tested), `word_separators` -> alacritty `semantic_escape_chars`, `scrollback_lines` ->
  `scrolling_history` (SpawnOpts struct replaced the positional spawn args).
- **Render**: cursor blink (`cursor_blink`, focused pane only, 1.06s cycle, repaint scheduled at
  phase flips; `blink_on` tested); bold->bright (`bold_bright`, `colors::cell_fg` tested).
- **Mouse**: copy-on-select (`copy_on_select`, skips whitespace-only), middle-click paste
  (`paste_on_middle_click`, arboard clipboard read, deferred apply, focuses the pane).
- **Scroll**: Shift+Home/End -> top/bottom. **Tabs**: Cmd+Shift+←/→ move tab (reserved in
  key_to_bytes, tested); Restart / Close-others/right/left menu items (`close_tabs_where`).
- **Links**: bare IPv4(:port) literals -> http:// (tested).
- **Session restore** (`session.rs`, tested round-trip): tabs' cwd/title/color + active index ->
  `~/.config/stdusk/session.toml`, saved every 3s when changed; restored on launch
  (`[session] restore`, default true).
- **Community themes** (`themes.rs` + `build.rs`): 191 Tabby XRDB schemes embedded at build time
  + user files in `~/.config/stdusk/schemes/`; `colors::by_name` falls back to
  `themes::lookup` (normalized names).

## Notify-when-done (`terminal.rs`, `main.rs`, `config.rs`)
- The reader thread times each command via OSC 133 (`Instant` on CommandStart; on CommandEnd, if
  it ran >=10s, set `TabState.done_notify = Some(exit)`). The UI drains it each frame and, when
  `terminal.notify_on_done` (default true) AND stdusk is hidden, posts a macOS notification via
  `osascript display notification`. Consumed even when visible (so it never fires late) but only
  surfaced while hidden - no nagging while you're watching the build. (notify-on-activity: TODO.)

## Dock/menu-bar modes (`main.rs`, `config.rs`) - macOS
- macOS has no "menu bar without a Dock icon" static mode: accessory = neither, regular = both.
  So pure accessory (our default) means the visible menu bar belongs to whatever other regular app
  is frontmost (looks like "Tabby" when Tabby is running) - cosmetic, not stdusk's.
- New opt-in `quake.dock_when_visible` (default false): with `hide_from_dock`, launch **regular**
  and flip to **accessory** whenever the window is hidden, so the Dock icon + a real "stdusk" menu
  bar appear only while visible. Runtime flip via `set_dock_icon` -> `NSApplication.
  setActivationPolicy` (objc2-app-kit 0.3, **safe** binding - no `unsafe`; needs the
  `NSRunningApplication` feature). Synced once/frame on change (`sync_dock`, `dock_shown` guard).
  Default off keeps the current pure-accessory behavior.

## M12b - keyboard pane resize (`pane.rs`, `main.rs`)
- Cmd+Ctrl+arrows resize the focused pane (Right/Down grow, Left/Up shrink). `Pane::resize_focused`
  (pure, tested) walks to the nearest ancestor split matching the axis and nudges its ratio,
  flipping the sign for a B-side child so "grow" always enlarges the focused pane. Cmd+Ctrl+arrows
  produce no pty bytes (ctrl swallows arrows to None), so no reservation needed. Splits group
  (nav + maximize + resize) now complete.

## M15 - tab power features (`main.rs`)
- **Next/prev tab cycle**: Ctrl+Tab / Ctrl+Shift+Tab (wraps via `rem_euclid`). Ctrl+Tab already
  returns no pty bytes (ctrl_letter(Tab)=None), so it's free.
- **Reopen closed tab**: Cmd+Shift+T. `close_tab` pushes the tab's cwd onto a `closed` stack (cap
  20); `reopen_tab` pops + spawns a tab in that cwd. Cmd+T (no shift) stays new-tab.
- **Duplicate tab**: context-menu "Duplicate" (`TabAction::Duplicate`) - new tab in the source
  tab's cwd.
- Still TODO in the tab group: move-tab hotkeys (arrow combos clash with terminal line-nav),
  tabs 10-20, pin, notify-when-done.

## M14 - follow-OS light/dark theme (`colors.rs`, `config.rs`, `main.rs`)
- The theme is now swappable at runtime: `colors` holds it in a `LazyLock<RwLock<Theme>>` (was a
  set-once `OnceLock`); `Theme` is `Copy` so accessors copy it out without holding the lock.
  `colors::set(theme)` swaps it.
- **Adaptive chrome**: `elevated`/`titlebar`/`border` now branch on `is_dark()` (bg luminance) so
  the tab strip / active tab / dividers read correctly on light themes too (the old shade factors
  were dark-only). Added a `one-half-light` built-in.
- **Follow-OS**: config `appearance.follow_system` (default true) + `theme_light`/`theme_dark`.
  `ui()` reads `input.raw.system_theme` each frame and, when the resolved theme name changes,
  calls `colors::set` + `apply_theme` + repaint. `follow_system = false` uses `appearance.theme`.
  Verified the light theme renders with correct chrome via a forced screenshot.
- Still single static theme (no XRDB-191 import) - that's the next theme slice.

## M13 - input polish + links-on-hover (`main.rs`, `terminal.rs`, `ui.rs`, `config.rs`)
- **Font zoom**: Cmd+=/Cmd+-/Cmd+0 adjust a runtime `Stdusk.zoom` multiplier (0.5-3.0); the grid
  font = `font_size * zoom`, cell metrics + pty resize follow automatically.
- **Cmd+A select-all** (`PtyTerm::select_all`, whole buffer) then Cmd+C copies.
- **Cmd+K clear**: sends Ctrl-L (shell clear). A true scrollback-wipe is still TODO.
- **Shift+PageUp/Down**: scroll the viewport by a page (`PtyTerm::rows`).
- All gated on no text modal (find/rename) owning the keyboard, except zoom (harmless).
- **Links on hover by default** (user: Tabby reacts on hover, no modifier): new config
  `terminal.link_modifier` ("none" default = plain hover/click; else cmd/ctrl/alt/shift). Caller
  computes `link_active` via `ui::link_modifier_held` (tested) and passes it to `render_grid`
  (which dropped its hardcoded Cmd check).

## M12 - keyboard pane nav + maximize (`pane.rs`, `main.rs`, `ui.rs`)
- **Directional focus**: Cmd+Alt+arrows move focus to the neighbor pane. `pane::neighbor(layout,
  from, Dir)` (pure, tested) picks the nearest pane whose center is in the direction + whose
  cross-axis span overlaps the current pane.
- **Maximize/zoom**: Cmd+Alt+Enter toggles `Tab.maximized` - the central panel then renders only
  the focused pane at full area and hides the splitters (auto-ignored when the tab has one pane).
- `key_to_bytes` now reserves Cmd+Alt+{arrows,Enter} (returns None) so these don't leak to the pty
  as word/line motion or CR (regression-tested). Prev/next + focus-by-index + keyboard resize
  still TODO.

## M11 - clickable links (`links.rs`, `ui.rs`, `config.rs`)
- Cmd+click URLs (`https?/ftp/file://`) and file paths (absolute/`~`/relative) to open them via
  `open`. Cmd-hover underlines the link + shows the pointing-hand cursor. `links::find_in_row`
  (pure, per-row, char-col spans; URLs beat overlapping paths; trailing punctuation trimmed) +
  `resolve_target` (`~`/cwd expansion) are unit-tested (6 tests); `render_grid` gained a `links`
  arg + hit-tests the hovered row. Config `terminal.clickable_links` (default true). IP-only
  literals (no scheme) not yet detected.

## Post-0.1.0 fixes
- **Menu-bar (tray) icon** (`tray.rs`, `tray-icon` crate): an accessory app has no Dock icon, so
  this is stdusk's presence + control - a monochrome template icon (`assets/stdusk-tray.png`,
  macOS auto-tints) with a Show/Hide + Quit menu. `MenuEvent::receiver()` polled each frame (same
  pattern as the global hotkey). Config `quake.menu_bar_icon` (default true). Built in
  `Stdusk::new` (graceful `None` on failure); if it ever doesn't appear, move creation to the
  first `ui()` frame (after the event loop is fully resumed).
- **Login-shell PATH fix** (`shell.rs`): GUI apps get launchd's minimal PATH, so a non-login shell
  skipped `/etc/zprofile` (path_helper) + `~/.zprofile` (brew) -> `starship` etc. "command not
  found". Now spawn login+interactive (`-l -i`) like Terminal.app, and the ZDOTDIR integration
  bridges the FULL startup set (.zshenv/.zprofile/.zlogin/.zshrc), not just .zshenv/.zshrc.
  Reproduced the exact error under a minimal PATH + verified the fix.
- **Close-x on tabs** (`ui.rs` `draw_tab`): clicking the x on a non-active tab focused it instead
  of closing, and the x flickered. Cause: the tab's click interaction was registered AFTER the x,
  so it stole the click. Fix: interact the tab FIRST (x, registered later, is on top and wins its
  clicks) and gate the x on `contains_pointer()` (stable across the whole tab rect) not `hovered()`.
- **Distribution = Homebrew cask, not formula** (user: "we need it to be findable"). A formula
  keeps the `.app` in the Cellar, so Spotlight/Launchpad never see it. The tap now ships
  `Casks/stdusk.rb`: `app "stdusk.app"` -> `/Applications` (Spotlight-findable) + `binary` stanza
  for the `stdusk` CLI. `brew install hobo-ware/tap/stdusk` auto-resolves the cask. Workflow +
  `native/packaging/stdusk.rb` regenerate the cask.
- **Gatekeeper block (macOS 26)**: the build is only ad-hoc/linker-signed (Rust default), and a
  cask sets `com.apple.quarantine`, so `spctl` = "no usable signature" and the GUI launch is
  hard-blocked ("stdusk Not Opened"). Fix: the cask's `postflight` runs `xattr -dr
  com.apple.quarantine` so the installed app launches clean. **Proper fix (deferred)**: Developer
  ID signing + `notarytool` notarization in CI (needs an Apple Developer account + secrets); then
  drop the postflight. See packaging/README.md.
- **Quake = macOS accessory app** (user: shouldn't sit in Dock/tray, just drop from the top).
  `quake.hide_from_dock` (default true) sets `ActivationPolicy::Accessory` via eframe's
  `event_loop_builder` hook (needs a direct `winit = "0.30"` dep for the macos ext trait) - no
  Dock icon, no app-switcher/menu-bar entry. `false` = normal Dock app.
- **Native icon**: rebuilt on Apple's Big Sur grid - 824/1024 continuous-corner squircle (super-
  ellipse), inset margin, soft shadow, dusk gradient (was a full-bleed plain rounded rect that
  didn't match native icons). **Known gap**: true light/dark/tinted-adaptive app icons need the
  macOS 26 Icon Composer `.icon` format (a GUI/Xcode-26 step, not scriptable here) - static .icns
  for now.

## Tab-bar QoL batch (post-0.2.4, uncommitted)
- **Flush underlines / dead-band root cause**: `egui::Panel::top` (0.35) paints its frame fill +
  clip at its own height estimate (`margin + interact_size.y` = 24px) while the content row is
  40px - the fill stopped short and the strip's lower ~16px read as a dead band, with the tab
  colors floating in it. Fix: `.exact_size(ui::TAB_H + 6.0)` pins the panel so fill/clip/content
  agree; tabs now fill the full `TAB_H` row (bar bottom margin 0) and the underline paints at
  `rect.bottom()-3` = the strip's true bottom edge, over the hairline (deco layer is above it).
- **`appearance.tab_width`** = `"fixed"` (default; equal ~200px tabs, shrink evenly on overflow,
  titles ellipsized to fit) | `"dynamic"` (content-sized). `ui::tab_width_mode` +
  `ui::fixed_tab_width` are pure + tested; settings chips under Appearance > Window.
- **Close-x hidden until hover; swaps with the CLI chip**: `draw_tab` was rebuilt as one
  manually-painted widget with a LEADING slot - the CLI chip lives there, and hovering the tab
  replaces it with the close-x (no x anywhere otherwise, active tab included), so chip/x overlap
  is structurally impossible. Tab-first interaction ordering kept (x registered after, wins its
  clicks); x id now seeds from the stable tab id. Chip tooltip was dropped (a hovered tab always
  shows the x instead of the chip).

## Next up
- **Parity gap list**: [PARITY.md](./PARITY.md) is the comprehensive, source-scanned Tabby-vs-stdusk
  audit (every hotkey/config/menu/setting, keep-defer-drop, suggested M11-M17 order). Top wants:
  clickable links (M2.5 debt), keyboard pane nav/resize/maximize, input polish (select-all/clear/
  font-zoom/copy-on-select/middle-click/scroll hotkeys), color-scheme import (191 XRDB schemes),
  tab power features, session restore, settings GUI.
- **Cut future releases**: bump `native/Cargo.toml` version, tag `stdusk-v<x>`, push; then copy the
  release's generated `stdusk.rb` into the tap's `Casks/stdusk.rb` (consider automating the tap
  push with a PAT). Signing + notarization run automatically once the five Apple secrets exist
  (see the 1.0.0-rc prep section + packaging/README.md); until then the cask strips quarantine.
- **Live-verify**: the 1.0.0-rc human shortlist (quake toggle, notifications, clipboard
  round-trips, live CLI badges) - see the 1.0.0-rc prep section.
- Backlog: M8 pane reorder (drag-to-rearrange). (Broadcast input + aggregated tab progress
  shipped 0.4.1; pane zoom shipped M12; settings GUI 0.2.1-0.2.2; all-match search highlight
  0.3.2; the headless `run_ui` harness replaced the egui_kittest idea.)
