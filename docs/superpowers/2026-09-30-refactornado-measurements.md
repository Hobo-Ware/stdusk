# Refactornado measurements - 2026-09-30

Measured the simple allocation and locking changes against the original functions at
`1f746f6` on this macOS host, using Rust 1.98.1 optimized scratch harnesses. No dependencies
were added. These are isolated CPU microbenchmarks, not complete frames, a GUI profile or FPS
measurements. Other work was running on the host; absolute timings are illustrative.

## Results

Medians per operation or viewport conversion:

| Workload | Before | After |
| --- | ---: | ---: |
| Search visibility, 100 matches | 0.372 us | 0.262 us |
| Search visibility, 10,000 matches | 7.835 us | 4.192 us |
| Search visibility, 100,000 matches | 70.563 us | 39.339 us |
| Progress/command aggregation, 1 pane | 27.3 ns | 1.9 ns |
| Progress/command aggregation, 4 panes | 38.1 ns | 6.8 ns |
| Progress/command aggregation, 16 panes | 50.4 ns | 15.9 ns |
| Default-color conversion, 80 x 24 | 32.136 us | 2.678 us |
| Mixed-color conversion, 80 x 24 | 13.664 us | 4.327 us |
| Default-color conversion, 160 x 60 | 145.054 us | 12.775 us |
| Mixed-color conversion, 160 x 60 | 69.273 us | 21.628 us |

The changes are small and simplify ownership: borrow search matches, reduce status iterators
directly, and pass one copied theme through color conversion. The status savings are tiny in
absolute terms; the main benefit is removing temporary vectors. No broad renderer rewrite is
justified by these measurements.

## Method and equivalence

- Search/status/quoting harness extracted old implementations from git HEAD and new functions
  from the working tree; compiled with `rustc +1.98.1 -O --edition 2024`. Inputs/outputs used
  `black_box`. Five samples, old then new in each sample. Search measured clone plus the real
  `visible_matches` filtering versus borrowed filtering for 24 visible rows. It used 10,000
  iterations/sample for 100 and 10,000 matches, and 1,000 for 100,000 matches.
- Status measured 100,000 iterations/sample over all-normal progress and successful commands,
  forcing full scans. It excludes leaf collection and PTY mutex acquisition. The real reducers
  can now stop reading after an error/failure, preserving first-error and first-tie behavior.
- Equivalence passed for 11,111 progress sequences and 1,364 focused-command/sequence cases
  (lengths 0-4 across representative variants), plus 4,681 quoted strings (lengths 0-4 across
  ASCII, spaces, apostrophes, newline, dollar, backtick, backslash and Unicode). The shared
  quoting helper matched both previous implementations.
- Color harness compiled verbatim old/new color modules against existing release dependency
  rlibs. Nine alternating old/new rounds, 300 viewport conversions each, black-boxed inputs and
  outputs. The new measurement includes one theme snapshot per viewport. Default foreground
  uses transparent default background; mixed input includes named, indexed and truecolor plus
  bold. It excludes grid traversal, selection, font layout, GPU work and lock contention.
- Color equivalence passed 5,272 comparisons: four built-in themes, all 256 indexed colors,
  all 29 named colors, 64 RGB edge/midpoint values, both bold states and query indices 0-270.
  Permanent tests additionally cover explicit-theme mapping, shell argument round-trips,
  progress ties, and constructed terminal dimensions/history/selection/event routing.
- Releasing the terminal guard before encoding the already-owned screen copy was retained as
  a simpler lock boundary; that change was not separately timed. Spawn/adopt construction and
  settings reapplication are behavior-preserving extractions, not performance claims.

Scratch sources/results remain under `/tmp/stdusk-ui-bench*` and `/tmp/stdusk-colors-*` on
the measurement host; those paths are temporary and are not portable repository artifacts.
This receipt preserves the measurements, workload and limits. No parser-buffer or ligature
buffer changes were made; broader rendering work remains deferred until profiling warrants it.

## Final validation

Rust 1.98.1 formatting, Clippy (`--all-targets -- -D warnings`), offline build and full tests
passed: 589 passed, 4 opt-in tests ignored, versus 586/4 before the changes. A separate source
review found no concrete behavior regression. Terminal and Settings screenshots rendered and
were inspected in isolated state directories; they are smoke checks, not interactive acceptance
or performance measurements. Validation completed before committing.
