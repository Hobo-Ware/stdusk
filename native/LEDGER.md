# stdusk - implementation ledger

Living record of what's built, what's next, and the hard-won facts an agent needs to
resume without rediscovering them. **Every agent updates this file after each work session
or milestone.** Keep it truthful - if a test is red or a step was skipped, say so.

- Project: a native Rust quake terminal with a real GUI tab bar + first-party AI agent.
- Repo: `Hobo-Ware/stdusk` (a hard fork of Eugeny/tabby - Electron Tabby lives on `master`
  as reference; the Rust rewrite lives on branch `rust`, crate in `native/`).
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
| M5 | Tab mgmt: context menu, color, rename, reorder, keybinds, cwd | 🟡 code done, builds + 17 tests green, pending human verify |
| M6 | Resize + scrollback + paste + OSC52 + bracketed-paste | 🟡 code done, builds + 17 tests green, pending human verify |
| M6.5 | Mouse text selection + Cmd+C copy | 🟡 code done, builds + 17 tests green, pending human verify |
| M7 | Scrollback search (Cmd+F) | 🟡 code done, builds + 34 tests green, pending human verify |
| M8 | Split panes (pane tree, focus, drag-resize, per-pane pty) | 🟡 code done, builds + 46 tests green, pending human verify |
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
  `.github/workflows/native.yml` (fmt/clippy -D warnings/test on the `rust` branch).
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
**M9 - shell integration (OSC 133) + exit-code state dot; bell; cursor styles.** Parse OSC 133
prompt/command marks in `osc.rs` → `TabState.last_exit`; show a running/done/fail state dot on
the tab (separate from the manual color). Add bell (visual flash / config) and block/underline
cursor styles (currently beam-only). See PLAN §4d/§9. Feeds M10 (the AI agent wants exit codes).
(Backlog: human-verify M5-M8 live - all code-done but unverified. M8 deferrals: pane reorder,
broadcast input, pane zoom, aggregated tab progress. M7: all-match highlight.)
