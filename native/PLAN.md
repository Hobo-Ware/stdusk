# stdusk - architecture & migration plan

Port Tabby's **daily-driver experience** to a native Rust app at ~99% fidelity for
the features that matter, drop the long tail (SSH/serial/telnet/plugin-marketplace/
web-sync), and go **beyond** Tabby with a first-party AI agent built into the terminal.
North-star features, ranked:

1. **Progress reporting on tabs** - the crown jewel, non-negotiable
2. Quake drop-down (global hotkey, top edge)
3. Theming (config-driven palette, opacity, blur, font)
4. Cool tabs + tab management + per-tab color coding
5. **First-party AI agent** (read the terminal, propose + run commands with approval) - the "better than Tabby" bet
6. Sane default local-shell experience (colors, cursor, resize, scrollback, copy/paste, splits, search)

Decisions locked with the user: split panes **v1**, scrollback search **v1**. Quake hotkey is
**configurable** - default `Ctrl+\`` (works for everyone, no Fn/Karabiner needed), user overrides
to `F13` (or anything) in config.

---

## 1. Parity matrix - keep / defer / drop

| Tabby capability | stdusk v1 | Notes |
|------------------|-----------|-------|
| Local shell (pty) | **KEEP** | core |
| Quake / hotkey window | **KEEP** | `global-hotkey` (Carbon), no Accessibility grant |
| Tabs + management (new/close/reorder/switch) | **KEEP** | native egui |
| Per-tab color coding | **KEEP** | context menu + config |
| **Progress on tabs** | **KEEP** | %-regex scrape (Tabby-parity) + OSC 9;4 (superior), see §4b |
| Tab title from OSC 0/2/1337 | **KEEP** | via alacritty EventListener + OSC scanner |
| Theming (colors/opacity/blur/font) | **KEEP** | TOML config |
| Cursor styles, blink | **KEEP** | |
| Colors (16 + 256 + truecolor) | **KEEP** | alacritty grid |
| Scrollback | **KEEP (M6)** | |
| Scrollback search (Cmd+F) | **KEEP v1 (M7)** | user: must-have |
| Copy / paste, selection | **KEEP (M6)** | |
| Split panes | **KEEP v1 (M8)** | user: must-have; depends on resize |
| Clickable links (linkifier) | **LOW-HANGING (M2.5)** | cheap; see §10 |
| bracketed paste, paste-protection | **LOW-HANGING (M6)** | cheap; see §10 |
| cwd tracking (OSC 7 / 1337) | **LOW-HANGING (M1.5)** | scanner already parses OSC; see §10 |
| clipboard write (OSC 52) | **LOW-HANGING (M6)** | scanner handles it; see §10 |
| shell-integration exit codes (OSC 133) | **KEEP (M9)** | feeds state dot + AI agent |
| **First-party AI agent** | **NEW (M10)** | not in Tabby; the differentiator, see §4i |
| Settings GUI | **DEFER** | config file first, egui panel later |
| SSH client + profiles | **DROP** | use `ssh` in the shell |
| Serial / Telnet | **DROP** | |
| Plugin system + marketplace | **DEFER** | thin Rust hook API post-v1, see §9 |
| Web/SaaS config sync | **DROP** | |
| auto-sudo-password, UAC | **DROP** | |
| zmodem transfer | **DROP** | niche |

---

## 2. Architecture overview

Single always-running native app. One process, one quake window, N tabs; each tab is a
**pane tree** of terminals (splits). Each pane = one pty + one alacritty grid. GUI via
egui; render via wgpu (through eframe). The AI agent is an in-process module that reads
grid/scrollback/cwd/exit-codes and drives the pty through a permission gate.

```
              ┌──────────────────────────── stdusk (eframe/egui) ────────────────────────────┐
  F13 global  │  App: Vec<Tab>, active, Config, HotkeyManager, Agent                          │
 ─────────────┼─► QuakeController ── show/hide/animate/position ─► winit viewport             │
              │  Tab { title, color, root: Pane, focused, TabState(Arc) }                     │
              │        │                                   │                                  │
              │        ▼ render                            ▼                                  │
              │  TabBar (egui): pill + color + PROGRESS BAR (reads TabState.progress)          │
              │  Grid renderer (egui painter): cells, fg/bg, cursor    ┌── AI panel (egui) ──┐ │
              └────────────────────────────────────────────────────────┴─────────────────────┘ │
                        ▲ grid snapshot        ▲ TabState             │ reads grid/cwd/exit     │
   Per pane, reader thread:                                           │ writes pty (gated)      │
     pty.read() → ProgressScanner (%-regex + OSC 9;4) → TabState.progress                       │
                → OscScanner (7/1337 cwd, 52 clipboard, 133 prompt/exit) → TabState / clipboard  │
                → vte Processor.advance(&mut Term)   [colors/cursor/text]                        │
                → Term EventListener → Event::Title  → TabState.title                            │
                → ctx.request_repaint()                                                          │
```

### Module layout (`native/src/`)
```
main.rs          App, event loop, wiring
config.rs        TOML config + Theme + Keybinds (M4)
palette.rs       Theme palette + alacritty Color -> Color32 (M2)
terminal.rs      Terminal: pty spawn, reader thread, Term, snapshot, resize
progress.rs      ProgressScanner: %-regex + OSC 9;4 state machine   ← PRIORITY
osc.rs           OscScanner: OSC framing (7/1337 cwd, 52 clip, 133 shell-integration)
tabs.rs          Tab, TabState (title/progress/color/cwd/exit), tab + pane-tree ops
input.rs         key/text event -> pty bytes; app-keybind interception
ui/tabbar.rs     chunky tab pills + progress bar + egui context menu
ui/grid.rs       per-cell colored grid renderer + cursor + selection
ui/search.rs     scrollback search overlay (M7)
ui/agent.rs      AI agent side panel (M10)
quake.rs         global hotkey, show/hide/animate, focus-loss, monitor sizing
agent/mod.rs     Anthropic client (reqwest), tool-use loop, permission gate (M10)
agent/tools.rs   run_command / read_file / edit_file / read_terminal tool defs
```

---

## 3. Terminal core (M1 done, extend M6)

- `portable-pty` spawns `$SHELL`; reader thread feeds `vte::ansi::Processor` into
  `alacritty_terminal::Term<EventProxy>` behind `Arc<FairMutex<..>>`.
- `EventProxy: EventListener` captures `Event::Title`/`ResetTitle` → `TabState`.
- **Resize (M6):** on pane rect change, `pty.resize(PtySize)` + `term.resize(Dims)`;
  cols/rows = rect ÷ cell metrics.
- **Scrollback (M6):** `Dims.total_lines` gets history (e.g. 10k); render with display
  offset; wheel scrolls offset.

---

## 4. Subsystem designs

### 4a. Rendering (M2)
- Measure monospace cell (w,h) from the configured font via egui fonts.
- Custom widget: allocate `cols*w × rows*h`; per cell `rect_filled(bg)` then glyph via
  `painter.text`/galley (fg). Batch per-row runs of same style.
- `alacritty Color` → `Color32`: Named→theme 16, Indexed→256 cube, Spec→truecolor.
- Cursor: block/beam/underline from `term.grid().cursor`; blink timer.

### 4b. Progress reporting - THE feature (corrected to match Tabby's real behavior)
**Tabby does NOT use OSC 9;4.** Its actual implementation
(`tabby-terminal/src/api/baseTerminalTab.component.ts`) scrapes any percentage from the
output stream, gated on not being in the alternate screen, behind a `detectProgress`
config flag (default `true`):

```
/(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)/   →  0 < pct <= 100, ignored while alt-screen active
```

That's why it "just works" with apt/pip/npm/wget/curl - any tool that prints `N%`.
**stdusk mirrors this exactly** (Tabby-parity) AND layers on OSC 9;4 (ConEmu progress,
precise state incl. error/paused/indeterminate) for tools that emit it - superior to Tabby.

- Primary: `ProgressScanner::feed(&[u8])` runs the %-regex over decoded output text,
  suppressed when `Term.mode()` has `TermMode::ALT_SCREEN` (vim/less/htop). Config
  `detect_progress = true`.
- Enhancement: same scanner recognizes `ESC ] 9 ; 4 ; state ; pct BEL|ST` and prefers it
  when present (state 0 clear / 1 normal / 2 error / 3 indeterminate / 4 paused).
- Runs in the reader thread, updates `TabState.progress`, passes all bytes through to
  alacritty untouched (decoupled, unit-testable).

`enum Progress { None, Normal(u8), Error(u8), Indeterminate, Paused(u8) }`

Tab bar renders a 3px bar hugging the pill's bottom edge: accent/green=normal, red=error,
yellow=paused, marquee=indeterminate. For split tabs, the tab shows the worst child state.

### 4c. Quake mode (M3)
- `global-hotkey`: register the **configured** hotkey (parse `config.quake.hotkey` string →
  modifiers+key; default `Ctrl+\``, user can set `F13`, `Cmd+\``, `F12`, etc.). macOS Carbon
  hotkey API → **no Accessibility prompt** (beats the skhd dead-end). Poll `GlobalHotKeyEvent`
  each frame. Re-register live on config hot-reload (M4.5).
- Toggle: `ViewportCommand::Visible`, `Focus`, `OuterPosition`, `InnerSize`; drop = lerp
  height ~120ms. Position: top edge, full monitor width. Hide-on-focus-loss configurable.

### 4d. Tabs & management (M5)
- `TabState { title, color: Option<Color32>, progress, activity, cwd, last_exit }` in
  `Arc<Mutex>` (reader writes, UI reads).
- Ops: new (Cmd+T), close (Cmd+W), switch (Cmd+1..9, click), reorder (Cmd+Shift+←/→, drag),
  rename. **Context menu** = egui `response.context_menu(..)` (native popup, the thing that
  was hell in tmux): Rename, Color ▶, Close, Move, New.
- **Tabs are colorless by default** (like Tabby) - `Tab.color: Option<Color32>` starts `None`,
  no underline. The Color ▶ submenu offers `No color` + the `palette::TAB_COLORS` swatches;
  picking one sets the underline, `No color` clears it. Never auto-assigned.
- State dot (running/done/fail) from OSC 133 exit codes is separate from the manual color.

### 4e. Theming & config (M4)
`~/.config/stdusk/config.toml`: replicate Tabby's exact defaults as the golden baseline
(see §5 config tests):
```toml
[appearance]
theme = "one-half-dark"   opacity = 0.85   blur = 20
font = "JetBrains Mono"   font_size = 14   font_weight = 400   line_padding = 0
[terminal]
cursor = "block"          cursor_blink = true   bell = "off"
bracketed_paste = true    copy_on_select = false   right_click = "menu"
paste_on_middle_click = true   scroll_on_input = true   detect_progress = true
word_separators = " ()[]{}'\""
[quake]
hotkey = "Ctrl+Grave"   # default; override to "F13", "Cmd+Grave", "F12", ...
edge = "top"   height = "50%"   hide_on_focus_loss = true
[keys]  # browser-style defaults
new_tab = "Cmd+T"  close_tab = "Cmd+W"  split_v = "Cmd+D"  split_h = "Cmd+Shift+D"  find = "Cmd+F"
[agent]
enabled = true   model = "claude-opus-4-8"   # ANTHROPIC_API_KEY / ant profile for auth
```
Ship built-in themes (OneHalfDark default). Hot-reload via `notify` (M4.5, nice-to-have).

### 4f. Input (M1 done, extend)
- key+mods → bytes (control codes, arrows, fn keys, alt-as-ESC).
- Keybind layer intercepts app shortcuts before pty forwarding.

### 4g. Split panes (M8, v1)
- Tab owns a pane tree: `enum Pane { Leaf(Terminal) | Split{dir, ratio, a, b} }`.
- Recursively split the tab rect by `ratio`; each Leaf renders its grid; draggable
  splitters adjust ratio live. **Hard-depends on M6 resize** (per-pane pty sizing).
- Focus: one focused pane/tab; click or Cmd+Alt+arrows; input routes to focused pane.
- Ops: Cmd+D vertical, Cmd+Shift+D horizontal, Cmd+W closes focused (last → closes tab).

### 4h. Scrollback search (M7, v1)
- Depends on scrollback (M6). Overlay (Cmd+F): query + match count; scan grid+history;
  highlight cells; Enter/Shift+Enter cycle + scroll offset to match; case-insensitive +
  regex toggle.

### 4i. First-party AI agent (M10) - the "better than Tabby" bet
Built-in, not a plugin. A side panel (egui) where an agent reads the terminal and helps:
explain the last error, translate natural language → a command, or run an agentic
loop (run → observe → fix) behind a permission gate.

**Client:** there is **no official Anthropic Rust SDK**, so raw HTTP via `reqwest` against
`POST https://api.anthropic.com/v1/messages` (headers `x-api-key`, `anthropic-version:
2023-06-01`). Auth resolves `ANTHROPIC_API_KEY`, else an `ant auth` OAuth profile
(`Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`).

**Model & params:** `claude-opus-4-8`, `thinking: {type:"adaptive"}`,
`output_config:{effort:"high"}`, **streaming** (SSE) so long turns don't hit HTTP timeouts
- accumulate deltas into the panel.

**Tool-use loop** (Messages API `tools` + manual agentic loop; stop on `end_turn`, handle
`pause_turn`):
| Tool | Effect | Gate |
|------|--------|------|
| `read_terminal` | returns visible grid + recent scrollback + cwd + last exit code | none (read-only) |
| `run_command` | writes a command to the focused pane's pty | **confirm** (hard-to-reverse) |
| `read_file` / `edit_file` | cwd-scoped file ops | read none; edit **confirm** |

Context the agent gets for free from the reader-thread scanners: grid snapshot, scrollback,
`cwd` (OSC 7/1337), `last_exit` (OSC 133). Safety: `run_command`/`edit_file` require an
explicit approval click (Tabby has no equivalent; this is the differentiator done right).
Destructive commands never auto-run. Future: MCP client so the agent can use external tools
(§9).

---

## 5. Test strategy & test points (supreme - grounded in Tabby's spec + external corpora)

**Finding: Tabby has essentially zero automated tests** - no `*.spec.ts`/`*.test.ts`, CI
only lints and builds. So "reuse Tabby's tests" is impossible literally. Instead we do two
things that are stronger:

**(A) Port Tabby's behavioral spec - as encoded in its source - into Rust test fixtures.**
The behaviors ARE the oracle; we lift the exact constants/regex/algorithms and assert
stdusk reproduces them:
- **Progress** (`baseTerminalTab.component.ts`): port the exact regex
  `/(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)/` and its rules (0<pct<=100, alt-screen suppression,
  `detectProgress` flag) as a golden table: `"Installing... 42%"`→42, `"100%"`→100,
  `"0%"`→None, `"downloading 3.5%"`→3 (int) / 3.5 (float branch), alt-screen active→None,
  split reads across chunk boundary→correct.
- **OSC framing** (`middleware/oscProcessing.ts`): port the prefix `\x1b]` / suffixes
  `\x07`|`\x1b\\`, the partial-OSC buffering across chunks, OSC 1337 `CurrentDir=` (+ `~`
  homedir expansion), OSC 52 `c;<base64>` clipboard. Same inputs → same parsed events.
- **Config defaults** (`config.ts`): assert stdusk's `Config::default()` matches Tabby's
  ~20 documented defaults verbatim (cursor=block, bell=off, bracketed_paste=true,
  copy_on_select=false, right_click=menu, scroll_on_input=true, word_separators, …).
- **Keybind defaults**: assert the browser-style map matches.

**(B) Inherit the real terminal-conformance corpora** - the escape-sequence test suites we
get for free because we build on well-tested crates + the wider ecosystem:
- `alacritty_terminal`'s own test suite (the grid/VTE engine we embed) - already green.
- `vte` 0.15 parser tests.
- **xterm.js** (what Tabby actually renders with) ships an extensive escape-sequence test
  corpus - reuse its vectors as golden inputs for our renderer mapping.
- **esctest** / **vttest** conformance sequences as integration fixtures.

### Unit (fast, CI) - `cargo test`
- `progress.rs`: the full golden table above incl. split-read + alt-screen + OSC 9;4 states.
- `osc.rs`: framing, partial-chunk buffering, 7/1337/52/133 parse; malformed → ignored,
  bytes still pass through.
- `input.rs`: key+mods → byte table (Enter, Backspace, Ctrl-C, arrows, alt-as-ESC).
- `palette.rs`: Named/Indexed/Spec → Color32, 256-cube boundaries.
- `config.rs`: TOML → Config; Tabby-default assertions; missing keys → defaults; invalid → error.
  Hotkey string → (modifiers, key) table: `"Ctrl+Grave"`, `"F13"`, `"Cmd+Grave"`, `"F12"` parse;
  garbage → error, falls back to default.
- `agent/`: request-body shape (model, adaptive thinking, tools), SSE delta accumulation,
  tool-use loop transitions, permission-gate logic (mock the HTTP layer - no live calls in CI).

### Integration (headless: pty + parser, no window) - CI-able
- spawn `printf 'hello'` → `snapshot()` first line == "hello".
- spawn a script emitting `printf '\x1b]9;4;1;42\x07'` → progress == Normal(42).
- spawn `echo 'building 73%'` (not alt-screen) → progress == Normal(73); inside `vim` → None.
- OSC 7/1337 → cwd updates; OSC 133 → last_exit updates.
- resize: write cols/rows, `tput cols` matches.
- large-output stress (`cat bigfile`) → no panic, grid consistent.

### Golden / conformance
- feed xterm.js + esctest vectors → assert grid cells (colors, cursor moves, wrapping).

### GUI manual checklist (per release)
- F13 toggle; hide-on-blur; monitor width. Progress bar per state. Context menu ops.
- Theme/opacity/blur/font from config. Copy/paste, selection, cursor blink.
- Splits: split/close/navigate/resize. Search: find/highlight/cycle.
- AI panel: explain-error, nl→command with approval, agentic loop stop/deny.

---

## 6. Migration roadmap (phased, each phase ships + tests green)

| Phase | Deliverable | Exit criteria (tested) | Status |
|------:|-------------|------------------------|--------|
| **M0** | Chrome: quake window + chunky tab bar | builds, window opens, tabs switch | ✅ done |
| **M1** | pty + text render + input | shell runs, typing works | ✅ done |
| **M1.5** | **Progress** (%-regex + OSC 9;4) + OSC scanner (cwd) + tab bar | progress.rs/osc.rs unit tests green; live bar | ⏳ next |
| **M2** | Colored cell renderer + cursor | truecolor/256 render; cursor visible | |
| **M2.5** | Low-hanging: clickable links | click opens URL | |
| **M3** | Quake: configurable global hotkey (default Ctrl+\`), drop anim, hide-on-blur, monitor width | toggle works, hotkey parsed from config, no Accessibility prompt | |
| **M4** | Theming + config.toml (Tabby-default parity) | config tests green; palette/opacity/blur/font | |
| **M5** | Tab mgmt: context menu, color coding, rename, reorder, keybinds | menu ops + colors verified | |
| **M6** | Resize + scrollback + copy/paste + bracketed-paste + OSC 52 | `tput cols` matches; wheel scrolls; copy works | |
| **M7** | Scrollback search (Cmd+F) | find/highlight/cycle | |
| **M8** | Split panes (pane tree, focus, drag-resize, per-pane pty) | split/close/navigate; each pane sizes | |
| **M9** | Shell integration (OSC 133) → exit-code state dot; bell; cursor styles | dot flips on exit; checklist green | |
| **M10** | **First-party AI agent** (reqwest client, tool-use loop, permission gate) | explain-error + nl→command + gated run verified | |
| **M11** | Polish + Settings GUI panel | checklist green | |

Dependency notes: M1.5 depends only on M1 (prioritized). M8 (splits) **hard-depends on M6**
(resize). M7 depends on M6 (scrollback). M10 depends on M9 (OSC 133 gives the agent exit
codes) and M6 (scrollback context). Splits + agent sequenced late so the engine/renderer/
resize are solid first.

---

## 7. Risks & open questions
- egui per-cell render perf at large grids → per-row batching, dirty rects later.
- Split panes + per-pane pty resize is the biggest v1 lift (layout math + focus + N ptys).
- No Rust Anthropic SDK → raw HTTP; must track API drift (adaptive thinking, `output_config`,
  streaming SSE shape) by hand. Pin `anthropic-version: 2023-06-01`.
- OSC 133 shell integration requires the user's shell to emit prompt marks (zsh/bash hooks);
  ship an opt-in snippet, degrade gracefully when absent.
- CJK/wide-glyph + emoji cell-width handling deferred to M2/M6.

## 8. Current status
- Repo `Hobo-Ware/stdusk`, branch `rust`, crate in `native/`. Electron Tabby on `master` as reference.
- M0 + M1 implemented + compiling (eframe/egui 0.35, alacritty_terminal 0.26, portable-pty 0.9).

---

## 9. Missing features / future work (annotated inventory)

Full Tabby surface, with disposition. `DROP` = never; `DEFER` = post-v1; `FUTURE` = nice idea.

| Tabby / terminal feature | Disposition | Rationale |
|--------------------------|-------------|-----------|
| SSH client + saved profiles (`tabby-ssh`) | **DROP** | run `ssh` in the shell; huge surface, own auth/UI |
| SFTP browser | DROP | pairs with SSH |
| Serial port (`tabby-serial`) | DROP | niche hardware use |
| Telnet | DROP | legacy |
| Plugin system + marketplace (`tabby-plugin-manager`) | **DEFER → FUTURE** | offer a thin Rust trait-based hook API (on-output, on-tab, custom tool) post-v1; no npm marketplace |
| Web/SaaS config sync (`tabby-web`) | DROP | local config.toml + git is enough |
| Auto-sudo-password, UAC elevation | DROP | security smell |
| zmodem file transfer | DROP | niche |
| Settings GUI | **DEFER (M11)** | config.toml first; egui settings panel later |
| Profiles / multiple shells per launcher | **FUTURE** | config could define named profiles (shell, cwd, env, color) |
| Community color-scheme import (iTerm/base16) | **FUTURE** | parse `.itermcolors` / base16 YAML into a theme |
| Ligatures | **FUTURE** | needs a shaping pass (harfbuzz/rustybuzz); default off like Tabby |
| Broadcast input to all panes | **FUTURE** | trivial once splits land |
| Session restore (reopen tabs/cwd on launch) | **FUTURE** | persist TabState + cwd |
| Command palette (fuzzy actions) | **FUTURE** | egui + a fuzzy matcher |
| Notifications on long-command completion | **FUTURE** | OSC 133 exit + `notify-rust` |
| Image/sixel/kitty-graphics protocol | **FUTURE** | alacritty grid doesn't model images; large effort |
| AI: MCP client (agent uses external tools) | **FUTURE** | after M10; agent already speaks tool-use |
| AI: inline command explanation on hover | **FUTURE** | cheap once agent client exists |
| AI: `send_to_user` verbatim delivery in agentic runs | **FUTURE** | pattern from the Claude API guide |

Anything marked FUTURE gets a tracking issue when v1 lands; nothing here blocks the north-star set.

---

## 10. Low-hanging fruit (implement from the get-go, not deferred)

Cheap wins that ride on infrastructure we're already building - fold them into their
nearest milestone rather than treating them as extras:

- **cwd tracking (OSC 7 / OSC 1337 `CurrentDir=`)** - the OSC scanner (M1.5) already parses
  OSC framing; wiring `cwd` into `TabState` is ~10 lines. Powers: new-tab-in-same-dir,
  tab title = basename(cwd), and free context for the AI agent. **Do in M1.5.**
- **Clickable links** - regex URLs in the grid render, Cmd/Ctrl+click opens via `open`
  crate. No new infra beyond the renderer. **M2.5.**
- **bracketed paste + paste protection** - Tabby defaults; alacritty exposes bracketed-paste
  mode; multiline-paste confirm is a small egui modal. **M6 (with copy/paste).**
- **OSC 52 clipboard write** - scanner already frames OSC; decode base64 → set clipboard
  via `arboard`. **M6.**
- **new-tab-in-cwd** - once cwd is tracked, Cmd+T inherits the focused pane's cwd (Tabby +
  kitty behavior). **M5.**
- **`open`-on-path** - Cmd+click a file path (not just URLs) opens it. Extends the linkifier.
  **M2.5.**

These are explicitly in-scope from the start so v1 doesn't ship feeling thinner than Tabby
on the small stuff.
