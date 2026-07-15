---
trigger: glob
globs: 'src/{main,ui}.rs'
description: 'egui/eframe 0.35 conventions: thin render loop, deferred actions, foreground layers, testable pure helpers, toasts/focus/keybinds.'
applyTo: 'src/{main,ui}.rs'
---

# egui / eframe UI Conventions (0.35)

egui rebuilds the whole UI every frame; the render loop is close to untestable. The
through-line: **push logic out of the loop into pure functions in `ui.rs` so it is unit-
testable, and keep the loop thin and dumb.**

Note the 0.35 API this crate uses (differs from older egui docs/examples):
`impl eframe::App { fn ui(&mut self, ui: &mut egui::Ui, frame) }` (NOT `update(ctx)`);
`egui::Panel::top("id").show(ui, ..)` + `egui::CentralPanel` (NOT `TopBottomPanel::show(ctx)`);
`Frame::new()`, `.corner_radius(..)`, `Margin::symmetric(i8,i8)`.

## 1. State architecture (no god-struct)

- **DO** classify every field: **persistent** (tabs, scrollback, config), **derived**
  (parsed theme, cached metrics, ellipsized titles), **ephemeral** (hover, drag origin,
  this-frame selection, toast). Derived state is recomputed, never hand-edited.
- **DO** group cohesive ephemeral state into small structs as the app grows (a `Toasts`
  queue, a `QuakeWindow` for focus/animation) rather than piling scalar fields on `Stdusk`.
- **DON'T** stash `Response`/`Ui`/`Context` in struct fields across frames - they're
  frame-scoped. **DON'T** thread `&mut self` into nested closures and mutate live.

## 2. Deferred-action pattern (already in use - keep + generalize)

While iterating widgets you can't mutate the collection you're iterating. Collect intents,
apply after the loop. The reducer is a **pure method with no `ui`** - unit-test it directly.

```rust
enum TabAction { New, Close(usize), Rename(usize), SetColor(usize, Option<Color32>), .. }
// ...collect during the loop...
match action { Some(TabAction::Close(i)) => self.close_tab(i, &ctx), .. }
```

Test `close_tab`/`move_tab` as pure state transitions (len, active index) - no egui in the test.

## 3. Immediate-mode idioms & pitfalls

- **Stable `Id`s.** Widget identity is tracked by `Id`, and location is not a stable source
  of identity. Seed ids from a stable key, not a mutable loop index, for anything stateful
  (drag-reorder swaps index → loses focus/scroll). `ui.interact(rect, ui.id().with(("close", idx)), ..)`.
- **Painter, not widget-per-cell.** The grid is one interactive rect + manual painting, never
  a widget per cell. `Sense::click_and_drag()` for the grid (selection), `Sense::hover()`
  for tab underlines - don't `Sense` more than you consume.
- **Escape the clip rect via a foreground layer.** A child painter is clipped to its rect ∩
  the parent's; `ui.painter()` and `ui.painter_at(rect)` both intersect it. Things that must
  overflow (tab edge strokes past the row-layout clip, overlays) go on a foreground layer.
  This crate already relies on it for tab underline/progress:
  ```rust
  let dp = ui.ctx().layer_painter(LayerId::new(Order::Foreground, Id::new("tab_deco")));
  ```
- **Animation timing uses egui's clock, never `std::time`.** Read `ctx.input(|i| i.time)`
  (f64 seconds) so tests/headless runs are deterministic. Toast expiry, cursor blink →
  `ctx.request_repaint_after(dur)` (smallest pending duration wins).
- **Repaint on demand, not continuously.** Call `ctx.request_repaint()` while the pty streams
  or an animation is live; idle terminal = idle CPU. (Screenshot harness is the one place
  that repaints every frame, to trigger capture.)

## 4. Testable pure helpers (the point) - live in `ui.rs`

Extract and unit-test these; they must NOT take `ui`/`ctx`:

```rust
pub(crate) fn pos_to_cell(rel_x: f32, rel_y: f32, cw: f32, ch: f32,
                          cols: usize, rows: usize, top_line: i32) -> (i32, usize, bool);
pub(crate) fn ellipsize(s: &str, max: usize) -> (String, bool);
pub(crate) fn key_to_bytes(key: egui::Key, mods: egui::Modifiers) -> Option<Vec<u8>>;
pub(crate) fn progress_fraction(p: Progress) -> Option<f32>;   // clamp/round here, not in paint
pub(crate) fn basename(p: &str) -> String;
pub(crate) fn toast_alpha(remaining_s: f64, fade_window_s: f64) -> f32;
```

`egui::Key`/`Modifiers` are plain data, so `key_to_bytes` is fully testable - and the
keyboard table is exactly where modifier bugs hide (Option/Cmd arrows). Use `egui_kittest`
only for the thin slice that truly needs a widget tree (focus moving on tab switch); reserve
`insta` image snapshots for a couple of pixel-critical views.

## 5. UX polish

- **Toasts:** `{ text, born|expiry }`; draw a bottom-centre pill on a foreground layer, fade
  over the last ~0.35s via `toast_alpha`, `request_repaint` while live so it self-dismisses.
- **Focus:** request focus on the terminal surface on the quake open-transition edge only -
  never every frame, or typing breaks. Rename field uses `request_focus()` once.
- **Keybinds:** `Modifiers::command` (Cmd on macOS, Ctrl elsewhere) for app binds
  (Cmd+T/W/1..9); raw `ctrl` only where terminal semantics demand it (Ctrl-C = SIGINT).
  Cmd+C/V arrive as `Event::Copy`/`Event::Paste`, NOT key presses - watch the events.
- **Theming:** parse config once into a `Theme`, apply with `set_visuals`; don't re-parse
  per frame. Keep ANSI palette + chrome colors in one module so they stay consistent.

## Checklist for any UI change

- [ ] State classed persistent / derived / ephemeral, in the right place.
- [ ] Any math (mapping/clamp/format/parse) extracted to a pure `ui.rs` fn + tested first.
- [ ] Mutation during iteration → deferred `Action` enum + tested reducer.
- [ ] Overflow → foreground layer, not an inflated rect.
- [ ] Animation/IO → `request_repaint_after` + `input().time`, never `std::time`.
- [ ] No per-cell/per-frame allocation on the hot path.
