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
| M1 | pty + text render + input | ✅ done, compiles |
| M1.5 | Progress (%-regex + OSC 9;4) + OSC scanner (cwd) | ⏳ NEXT |
| M2 | Colored cell renderer + cursor | todo |
| M2.5 | Clickable links | todo |
| M3 | Quake: configurable global hotkey (default Ctrl+`) | todo |
| M4 | Theming + config.toml (Tabby-default parity) | todo |
| M5 | Tab mgmt: context menu, color, rename, reorder | todo |
| M6 | Resize + scrollback + copy/paste | todo |
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
- Verified: compiles clean (`cargo build` exit 0). GUI not visually verified in this env.

### M1 - pty + shell (`src/terminal.rs`, wired into `main.rs`)
- `PtyTerm::spawn(cols, rows, ctx)`: `portable-pty` spawns `$SHELL`; a reader thread feeds
  bytes through `alacritty_terminal::vte::ansi::Processor` into `Term<EventProxy>` behind
  `Arc<FairMutex>`; `ctx.request_repaint()` on output.
- `snapshot()` returns visible grid lines (plain text, no color yet).
- `main.rs`: each `Tab` owns a `PtyTerm`; central panel renders the active tab's snapshot as
  monospace; `collect_input` maps key/text events → pty bytes (Enter/Backspace/Tab/Esc/arrows/
  Ctrl+letter). Fixed 80x24, no color, no cursor, no resize, no scrollback yet.
- Verified: compiles clean. Live shell behavior NOT yet confirmed by a human run.

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

## Next up
**M1.5 - progress reporting.** Create `src/progress.rs` (%-regex + OSC 9;4 state machine,
alt-screen guard) and `src/osc.rs` (OSC framing: 7/1337 cwd, 52 clipboard, 133). Wire into
the reader thread → `TabState.progress` / `.cwd`. Render the progress bar in `ui/tabbar`.
Unit tests per PLAN §5 (the golden %-regex table incl. split-read + alt-screen). Exit
criteria: `cargo test` green + live bar shows on `echo 'building 42%'`.
