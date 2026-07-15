---
trigger: glob
globs: 'src/{main,terminal}.rs'
description: 'Frame-budget discipline for the cell-grid renderer: virtualize, batch, reuse buffers, repaint on demand, profile before optimizing.'
applyTo: 'src/{main,terminal}.rs'
---

# Performance

The render path runs 60x/s over thousands of cells. One shape/label per cell does not scale
(community reports show fps collapsing from 300+ at ~370 cells to sluggish at ~2600). Apply
these skeptically and **profile before declaring a win** - local feel is not a measurement.

## DO

- **Virtualize.** Build shapes only for the visible row range. `grid_snapshot` already walks
  `display_iter` (the visible viewport, honoring scroll offset) - keep it that way; 100k
  lines of scrollback still paint ~50 rows.
- **Batch by run, not by cell** (future optimization). Coalesce consecutive cells sharing
  fg/bg/style into one `text`/`rect_filled`. Most terminal rows are long single-style runs.
- **Precompute cell metrics once** from the monospace font and reuse across frames. Cache
  `FontId`/`Color32` lookups; don't rebuild galleys you can keep.
- **Reuse scratch buffers** on the struct (`clear()` + refill) - a `Vec<Shape>` batch, the
  deferred-action `Vec`, a row `String` - instead of allocating per frame.
- **Repaint reactively.** `request_repaint()` only while the pty produced bytes, an animation
  is live, or the cursor blinks. Known deadlines → `request_repaint_after(dur)`.

## DON'T

- **DON'T `format!` / allocate per cell per frame** on the hot path.
- **DON'T re-parse config or rebuild the `Theme` each frame** - parse once, reload on change.
- **DON'T run a global continuous repaint loop** (the idle terminal should idle the CPU). The
  `--screenshot` harness is the sole intentional exception.
- **DON'T guess the bottleneck** - wire up `puffin` and measure; that is what actually
  resolved the community grid-perf reports.

## Current status / debt (honest)

The renderer paints one `rect_filled` + one `text` per non-blank cell today. Fine for an
80x24-ish quake window; run-batching + virtualized-row shape reuse are deferred until a
profile shows they matter. Note any bound you add (row cap, sampling) in `LEDGER.md` - silent
truncation reads as "handled everything" when it isn't.

## Quick checklist

- [ ] Only visible rows build shapes.
- [ ] No per-cell/per-frame heap allocation on the hot path.
- [ ] Config/theme parsed once, not per frame.
- [ ] Repaint gated on real change; no global continuous loop.
- [ ] Perf claim backed by a `puffin` profile, not feel.
