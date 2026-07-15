---
trigger: glob
globs: 'src/{terminal,osc,progress,colors}.rs'
description: 'Terminal core: alacritty Term, the reader-thread<->UI lock boundary, byte-stream parsers, selection, colors.'
applyTo: 'src/{terminal,osc,progress,colors}.rs'
---

# Terminal Core

`portable-pty` spawns the shell; a reader thread feeds bytes through the `vte` ANSI parser
into a shared `alacritty_terminal::Term`. The egui thread snapshots the grid to render.

## The reader-thread <-> UI boundary (keep this shape)

Three sync primitives, each chosen for a reason - do not "unify" them:

- `Arc<FairMutex<Term>>` - the live grid. Shared *because* both threads need it (reader
  advances the parser, UI snapshots). A mutex is correct; ownership can't move.
- `Arc<Mutex<TabState>>` - small scraped observations (progress, cwd, clipboard) the reader
  writes and the UI polls once per frame.
- `Arc<AtomicBool>` - the hotkey toggle. Single flag, no lock.

**DON'T switch to channels here.** An `mpsc` queue fits *event streams*, but the UI is an
immediate-mode poll loop that wants the latest state every frame - a mutex-behind-`&self`
snapshot matches that model better than draining a queue.

**DO expose `&self` methods that lock internally** as `PtyTerm`'s contract (`scroll`,
`progress`, `grid_snapshot`, `take_clipboard`). The lock is an implementation detail.

**DO keep lock scopes grab-copy-drop.** `grid_snapshot` holds `term` for the whole build -
acceptable (one reader, frame needs a consistent view), but never call UI code or nest the
`state` lock inside it. The reader loop locks `term` and `state` in *separate* scopes; keep
that order, never nest.

## Byte-stream parsers (`osc.rs`, `progress.rs`)

**All input bytes flow to the alacritty engine untouched** - the scanners only *observe* a
copy of each chunk. Never let a scanner consume or alter the stream.

**Malformed input returns `None`, never panics.** Shell output is adversarial by nature.

**Chunk-boundary carry is the subtle part.** A `%` or an OSC frame can split across two
`read()`s. `ProgressScanner.carry` (trailing digits) and `OscScanner.buf` (partial frame)
reassemble across chunks. **Any change here must preserve the split invariant** (feeding in
two pieces == feeding whole) and update the `proptest` that guards it (see `testing.md`).

**Mirror Tabby's spec exactly** where we claim parity: the progress regex
`(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)`, 0 < pct <= 100, alt-screen suppression; OSC framing
`ESC ] … (BEL | ST)`. These constants are the oracle - don't "improve" them.

## Selection (alacritty owns it)

Selection lives in `Term.selection: Option<Selection>`. Build with
`Selection::new(SelectionType::{Simple|Semantic|Lines}, Point, Side)`, extend with
`.update`, read the highlighted range via `to_range(&term)` → `SelectionRange::contains`,
copy via `term.selection_to_string()`. `grid_snapshot` tags each cell `selected` and returns
`top_line` (buffer line of viewport row 0) so the UI maps mouse coords → grid `Point`.
Simple = drag, Semantic = double-click (word), Lines = triple-click. Clear on keystroke/paste;
keep while wheel-scrolling (buffer-point highlighting stays correct).

## Colors (`colors.rs`)

One `Theme { bg, fg, cursor, ansi[16] }` set once at startup (`OnceLock`); all reads go
through accessors. Chrome colors are *derived* from the theme (elevated/titlebar/border via
`shade`) so swapping themes recolors everything. `is_default_bg` → render transparent so
window opacity shows through. Map `alacritty Color`: Named→16, Indexed→256-cube+grayscale,
Spec→truecolor.

## Quick checklist

- [ ] Scanner observes a copy; all bytes still reach the alacritty engine.
- [ ] Malformed input → `None`, never panic.
- [ ] Chunk-boundary carry preserved; `proptest` split invariant updated.
- [ ] Lock scopes grab-copy-drop; `term`/`state` never nested.
- [ ] Parity constants (progress regex, OSC framing) unchanged.
