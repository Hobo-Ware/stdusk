---
trigger: glob
globs: '**'
description: 'Test layout, naming, table-driven + property tests, snapshots, extracting UI logic, coverage bar.'
applyTo: '**'
---

# Testing

Tests, tests, tests. Every fix ships with a test that would have caught the bug. No
regressions: the existing suite stays green and grows.

## Layout

- **Inline `#[cfg(test)] mod tests`** next to the code for pure logic (parsers, config, UI
  helpers). They reach private fns and are the right home for most of this crate.
- **Top-level `tests/`** only for genuine end-to-end flows (spawn a pty running `printf`,
  assert the grid snapshot) - those use the public surface and are slower; keep few.

## Naming

Name a test as a behavioral assertion, not a method name.

**Good:** `split_across_chunks`, `alt_screen_suppresses`, `ellipsize_marks_only_when_truncated`
**Bad:** `test_feed`, `it_works`

## Table-driven for homogeneous cases

```rust
#[test]
fn osc_9_4_state_mapping() {
    let cases = [
        (&b"\x1b]9;4;1;42\x07"[..], OscEvent::Progress(Progress::Normal(42))),
        (&b"\x1b]9;4;2;\x1b\\"[..], OscEvent::Progress(Progress::Error(0))),
    ];
    for (input, want) in cases {
        assert_eq!(OscScanner::new().feed(input), vec![want], "input {input:?}");
    }
}
```

## Property tests (`proptest`) for chunk-reassembly

The two carry/partial-frame state machines (`OscScanner`, `ProgressScanner`) are where
hand-written cases miss and fuzzing pays off. Encode the split invariant: **feeding a byte
stream in any two pieces yields the same events as feeding it whole.**

```rust
proptest! {
    #[test]
    fn osc_split_invariant(data: Vec<u8>, cut in 0usize..4096) {
        let cut = cut.min(data.len());
        let whole = OscScanner::new().feed(&data);
        let mut sc = OscScanner::new();
        let mut split = sc.feed(&data[..cut]);
        split.extend(sc.feed(&data[cut..]));
        prop_assert_eq!(whole, split);
    }
}
```

`proptest` over `quickcheck` (better shrinking, richer strategies, community default).
`[dev-dependencies]` only - never ship a test crate in the binary.

## Snapshots (`insta`) for render/color output

Assert a `GridSnap` (or the 256-color table) against a stored snapshot so visual
regressions surface in the diff; `cargo insta review` accepts intentional changes. Keep
snapshots small and reviewable - never snapshot 80x24 frames of blanks.

## Extract UI logic so it's testable

The `eframe::App` render loop is nearly untestable. Everything worth asserting is a pure
function of data - pull it out of the closure into a free fn in `ui.rs` and unit-test it.
Prime targets: `pos_to_cell` (mouse→grid, the most transposition-prone map), `ellipsize`,
`key_to_bytes` (the keyboard table - a test here would have caught modifier-arrow bugs),
`progress_fraction`, `basename`. If a helper needs `ctx` only for `input().time`, pass
`time: f64` in as a parameter instead of taking `ctx`.

```rust
#[test]
fn cmd_left_sends_line_start() {
    assert_eq!(key_to_bytes(Key::ArrowLeft, cmd()), Some(vec![0x01]));  // Ctrl-A
}
```

## Headless egui end-to-end frames (interaction regressions)

Some bugs live in egui's hit-test/focus plumbing, not in any pure helper - a drag sense
swallowing clicks, an unfocused text field black-holing keys. The sanctioned way to
regression-test those is driving REAL frames headless with `Context::run_ui`:

```rust
let ctx = egui::Context::default();
let raw = egui::RawInput {
    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
    events: vec![Event::PointerMoved(p)], // or PointerButton { .. }, Key { .. }
    focused: true,
    ..Default::default()
};
let _ = ctx.run_ui(raw, |ui| { /* rebuild the app's REAL widget structure */ });
```

Rules:

- **Reproduce the app's actual structure** (panel + `draw_tab` + grid interact +
  `collect_input`), not a simplification - hit-test bugs only reproduce with the real layering.
- **Warm-up frame first** (empty events) so layout exists; read widget rects from the
  responses, then aim synthetic pointer events at them.
- **One frame per event step** (move, press, release) - egui decides click-vs-drag across
  frames (`is_decidedly_dragging`).
- **Assert on responses / collected bytes**, not pixels.

Reference implementations: the tab click / drag-reorder / close-x and find-bar backspace
tests in `src/ui.rs`, and `sections_render_headless` in `src/settings.rs`.

## Coverage bar

Don't chase a percentage. Target **100% of parser/state-machine + config branches** and
**every extracted UI helper**; accept that egui draw code and pty plumbing are covered by a
smoke test + manual runs. Honest bar: ~70-80% line coverage with parsers at 100%. Treat
uncovered `main.rs` draw code as expected, not debt.

## Quick checklist

- [ ] The fix has a test that fails before it and passes after.
- [ ] Homogeneous cases are table-driven.
- [ ] Chunk-boundary logic touched → the `proptest` split invariant covers it.
- [ ] New UI math extracted to a pure fn in `ui.rs` and unit-tested.
- [ ] Interaction/hit-test/focus bug → a headless `Context::run_ui` end-to-end test.
- [ ] Full suite green; nothing removed to make it pass.
