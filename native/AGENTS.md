# Always-loaded core (small, project-wide)

@.agents/rules/project.md

@.agents/rules/code-principles.md

@.agents/rules/implementation.md

@.agents/rules/testing.md

# Domain rules - load on demand

Domain-specific rules are NOT auto-imported to keep baseline context small. Read them with
the Read tool when the work touches the matching area. `CLAUDE.md` routes the mapping; the
rule files live at `.agents/rules/`:

- `ui.md` - egui/eframe render loop, tab bar, grid renderer, selection, toasts (`src/{main,ui}.rs`)
- `terminal.md` - alacritty Term, pty reader thread + lock boundary, OSC/progress parsers, colors (`src/{terminal,osc,progress,colors}.rs`)
- `performance.md` - cell-grid frame budget, repaint scheduling, allocation discipline (`src/{main,terminal}.rs`)
- `platform.md` - quake window, global hotkey, monitor sizing, macOS keybinds (`src/{main,config}.rs`)

Re-read after long gaps if context was compacted.
