Before implementing anything, identify which area you are working in and read the
corresponding rule file from `.agents/rules/` (only the core rules auto-load via AGENTS.md;
domain rules load on demand):

- UI / render loop / tab bar / grid / selection / toasts (`src/main.rs`, `src/ui.rs`): read ui.md
- Terminal core: alacritty Term, pty reader thread, OSC/progress parsers, colors
  (`src/terminal.rs`, `src/osc.rs`, `src/progress.rs`, `src/colors.rs`): read terminal.md
- Performance work: cell-grid rendering, repaint scheduling, allocation on the hot path
  (`src/main.rs`, `src/terminal.rs`): read performance.md
- Quake window, global hotkey, monitor sizing, macOS keybinds (`src/main.rs`, `src/config.rs`):
  read platform.md
- Tests (`#[cfg(test)]`, `tests/`, proptest, insta): testing.md (already loaded as core)
- All other code: project.md, code-principles.md, implementation.md (always-on baseline).

Also read `LEDGER.md` (current state) + `PLAN.md` (architecture + roadmap) at the start of a
session, and update `LEDGER.md` after each milestone.

@AGENTS.md
