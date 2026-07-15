---
trigger: glob
globs: 'src/{main,config}.rs'
description: 'Quake window behavior, global hotkey, monitor sizing, and macOS-natural keybinds - the OS-specific caveats with no Tabby analog.'
applyTo: 'src/{main,config}.rs'
---

# Platform (quake window, hotkey, macOS keys)

The one area with no Tabby analog and heavy OS-specific gotchas. These are hard-won; don't
rediscover them.

## Quake hide/show (macOS)

> **NEVER hide with `Visible(false)` or by moving fully off-screen.** On macOS the OS then
> occludes the window and App-Naps the process, throttling the run loop so the global hotkey
> handler never fires again - the window can't be brought back.

**DO** hide by parking the window mostly below the screen, leaving a ~2px sliver on-screen so
it stays un-occluded, plus `request_repaint_after(120ms)` while hidden so the run loop keeps
delivering the hotkey. A proper native hide (NSPanel `orderOut` via objc2) is a polish item
that would replace the sliver hack.

**DO** apply full quake sizing only once `monitor_size` is known (it's `None` on frame 0);
guard with a `sized` flag and `request_repaint` until it arrives.

**DO** arm hide-on-focus-loss only after the window has gained focus since showing
(`was_focused`), so a window that launches unfocused doesn't vanish instantly.

## Global hotkey

`global-hotkey` uses the Carbon API on macOS → **no Accessibility grant needed** (the skhd
route was a dead end; this is why we don't shell out). A background thread blocks on
`GlobalHotKeyEvent::receiver().recv()`, sets an `Arc<AtomicBool>`, and calls
`ctx.request_repaint()` - the repaint is what wakes eframe *even while hidden*. The
`GlobalHotKeyManager` must be kept alive in the app struct or the registration drops.

Hotkey is config-driven: `parse_hotkey("Ctrl+Grave" | "F13" | "Cmd+Grave" | ...)` →
`(Option<Modifiers>, Code)`. Default `Ctrl+Grave` (works everywhere, no Fn/Karabiner). Keep
`parse_hotkey` pure and table-tested.

## macOS-natural keybinds

Terminal input maps modifier+key → bytes for readline/natural editing:

- `Option+←/→` → `ESC b` / `ESC f` (word back/forward)
- `Cmd+←/→` → `Ctrl-A` (0x01) / `Ctrl-E` (0x05) (line start/end)
- `Option+Backspace` → `ESC DEL`; `Cmd+Backspace` → `Ctrl-U` (0x15)
- `Ctrl+letter` → control code; plain arrows → `ESC[A/B/C/D`

This whole table belongs in a pure `key_to_bytes(key, mods)` fn (see `ui.md`) so it's
unit-testable - a missing-modifier case is exactly the bug class that slipped through once.

## Quick checklist

- [ ] Quake hide uses the sliver park + repaint tick, never `Visible(false)`.
- [ ] Quake sizing gated on `monitor_size` being known.
- [ ] `GlobalHotKeyManager` kept alive in the app struct.
- [ ] Hotkey parsing stays pure + table-tested.
- [ ] Keybind changes go through `key_to_bytes` with a test.
