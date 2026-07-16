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

## 0. Design system (shared components - USE THESE, don't hand-roll)

Every surface/input/button must come from a shared primitive in `ui.rs`, so the whole app reads
as one consistent system. **If you're about to style a `Frame`/`TextEdit`/`Button` inline, stop
and use (or extend) the primitive instead.** When two places need the same thing (the find bar
and the rename dialog are the same input + surface), they MUST call the same helper - never
re-style a second copy by hand (that's how the rename dialog drifted ugly).

Current primitives (`ui.rs`):

- `overlay_frame() -> Frame` - the floating surface for every popover/dialog (find bar, rename):
  elevated fill, hairline border, `corner_radius 12`, soft shadow, `Margin::symmetric(12,8)`.
- `text_field(ui, &mut String, hint, width, color) -> Response` - the one text input: uniform
  15pt font, theme-colored field bg, `Margin::symmetric(8,6)`. `color` tints typed text.
- `action_button(ui, label, primary) -> Response` - dialog buttons; `primary` fills accent.
- `icon_button(ui, icon, tip)` / `icon_toggle(ui, icon, active, tip)` - tab-bar/toolbar icons
  (fixed `ICON_BTN_W`, glyph painted centered).
- `color_swatch(ui, color, selected)` - filled-circle color picker swatch.
- `style_menu(ui)` - context-menu/popup padding + min width; call at the top of every menu AND
  submenu.

**DO** add a new primitive here (with a doc comment) the first time a second call site needs a
styled widget; **DON'T** copy-paste egui styling. Keep sizes/paddings as the single source of
truth (consts or the helper body) so tweaks are one-liners and can't diverge.

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
- **Focus & input capture (three hard rules, each cost a bug):**
  1. Request focus on a text field ONCE (a one-shot flag), never every frame - re-requesting
     stops egui from ever reporting the Enter-triggered `lost_focus`, so Enter never submits.
  2. The focused terminal pane requests focus every frame, so it will steal focus from any open
     text field. Gate that (and pty input) on `input_captured = search.is_some() ||
     renaming.is_some()` - every modal text field must be in that expression.
  3. Sample `input_captured` BEFORE the modals run this frame. Sampling after lets the key that
     closes a modal (Enter committing a rename) leak to the shell once the modal clears its state.
- **Keybinds:** `Modifiers::command` (Cmd on macOS, Ctrl elsewhere) for app binds
  (Cmd+T/W/1..9); raw `ctrl` only where terminal semantics demand it (Ctrl-C = SIGINT).
  Cmd+C/V arrive as `Event::Copy`/`Event::Paste`, NOT key presses - watch the events.
- **Theming:** parse config once into a `Theme`, apply with `set_visuals`; don't re-parse
  per frame. Keep ANSI palette + chrome colors in one module so they stay consistent.
- **Toolbar alignment (learned the hard way).** A row of controls (tab bar) is ONE
  `ui.horizontal` = a single `left_to_right(Align::Center)` layout with a fixed
  `set_min_height`. **Never nest opposing layouts** (a `right_to_left` wrapping a
  `left_to_right`) to pin something to the right - the two layouts' vertical centres drift
  apart whenever a child's height changes, and the right-pinned item (the gear) misaligns.
  Right-pin by a computed spacer instead: `ui.add_space((ui.available_width() - W).max(0.0))`.
  Keep control sizes as shared consts (`ICON_BTN_W`) so the spacer math can't rot.
- **Center glyphs by painting, not layout.** Phosphor ink sits high in the line box, so a
  bare `ui.label(icon)` floats. Paint icons at `rect.center()` with `Align2::CENTER_CENTER`
  in an `allocate_exact_size` box (see `icon_button`), and force a uniform pill font with
  `style.override_font_id` rather than per-widget `.size()`/`.font()`.

## Checklist for any UI change

- [ ] State classed persistent / derived / ephemeral, in the right place.
- [ ] Any math (mapping/clamp/format/parse) extracted to a pure `ui.rs` fn + tested first.
- [ ] Mutation during iteration → deferred `Action` enum + tested reducer.
- [ ] Overflow → foreground layer, not an inflated rect.
- [ ] Animation/IO → `request_repaint_after` + `input().time`, never `std::time`.
- [ ] No per-cell/per-frame allocation on the hot path.
- [ ] Toolbar = one center-aligned row + fixed height; right-pin via spacer, never nested
      opposing layouts; icons painted centered (not `ui.label`).
- [ ] Surfaces/inputs/buttons come from a `ui.rs` design-system primitive (`overlay_frame`,
      `text_field`, `action_button`, …) - no hand-rolled `Frame`/`TextEdit`/`Button` styling; a
      text field that captures the keyboard gates pty input + focus (`input_captured`).
