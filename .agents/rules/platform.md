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

## Verifying window-chrome / OS-integration changes

> The `--screenshot` / `--screenshot-settings` harness runs HEADLESS and skips window
> management. It CANNOT verify quake show/hide, traffic-light / titlebar geometry, activation
> policy, Spaces behavior, or anything WindowServer draws. Reasoning from the code is
> necessary but never sufficient here.

Every window-chrome regression (traffic lights flung off-screen, quake hide never reshowing)
shipped because a blind change looked right in the diff and couldn't be *seen*. The rule:

**Verify these LIVE, never blind.** Build the binary and run it isolated with
`--state-dir <DIR>` - its own config / socket / session, and it SKIPS the single-instance
guard (`dev_isolated`) so it opens a real window *beside* a stable install - then eyeball it,
or have the user. A window IS launchable locally (`cargo run` opens the full product); the
alpha's "opens and closes" was the single-instance guard `return Ok(())`-ing the second
instance, NOT a GUI-impossible environment. Don't record "can't preview chrome here" as fact.

**NEVER guess coordinate math or window-lifecycle behavior blind and ship it.** Load-bearing
facts (both cost multiple round-trips to relearn):

- **The quake hide sliver must stay ON-SCREEN** (a live viewport) or macOS occludes + App-Naps
  the window, parking the run loop so the summon hotkey can't reshow it. This broke THREE times
  (`orderOut:`, then a fully-offscreen park - both "properly hide" ideas re-park the loop).
  Hide it VISUALLY with **alpha = 0** (`NSWindow.setAlphaValue`, a compositor property that
  keeps the window drawing/vsync), never by moving it off-screen or `Visible(false)`.
- **Traffic-light buttons are positioned relative to a captured baseline, bounded.** Capture
  the macOS-default button y ONCE (`traffic_baseline`), set `baseline - TRAFFIC_LIGHT_DROP`
  each window-mode frame (idempotent, a small nudge can't vanish them). An ABSOLUTE
  window-height origin threw them off-screen (1.4.1). The tuned `DROP` is eyeball-confirmed
  live via `--state-dir`, not derived.

## Quick checklist

- [ ] Quake hide uses the on-screen sliver + alpha=0, never `Visible(false)`/offscreen park.
- [ ] Window-chrome/lifecycle change verified LIVE via `--state-dir`, never shipped blind.
- [ ] Quake sizing gated on `monitor_size` being known.
- [ ] `GlobalHotKeyManager` kept alive in the app struct.
- [ ] Hotkey parsing stays pure + table-tested.
- [ ] Keybind changes go through `key_to_bytes` with a test.
