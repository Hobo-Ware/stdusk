//! macOS platform primitives: the objc2/AppKit window + Dock + titlebar tweaks, the Cmd+V
//! image-paste NSEvent hook, and desktop notifications. Every entry point carries a
//! `#[cfg(not(target_os = "macos"))]` no-op stub so the rest of the crate calls them
//! unconditionally. Split out of `main.rs`; the quake show/hide policy (`apply_visibility`)
//! stays there and calls into these.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use eframe::egui;

/// Show/hide the Dock icon (+ menu bar) at runtime by flipping the macOS activation policy.
/// Used only in the dynamic `dock_when_visible` mode. No-op off macOS.
#[cfg(target_os = "macos")]
pub(crate) fn set_dock_icon(visible: bool) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_dock_icon(_visible: bool) {}

/// Set the quake window's Space/full-screen collection behavior. `all_spaces` = true makes it
/// join every Space (`CanJoinAllSpaces`) and drop over full-screen apps (`FullScreenAuxiliary`)
/// so summoning it lands on whatever desktop is active; false restores the default (pinned to
/// its origin Space). Applied to every app window (the app has one viewport). No-op off macOS.
#[cfg(target_os = "macos")]
pub(crate) fn set_space_behavior(all_spaces: bool) {
    use objc2_app_kit::{NSApplication, NSWindowCollectionBehavior};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let behavior = if all_spaces {
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
        } else {
            NSWindowCollectionBehavior::Default
        };
        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for i in 0..windows.count() {
            windows.objectAtIndex(i).setCollectionBehavior(behavior);
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_space_behavior(_all_spaces: bool) {}

/// Unified titlebar (window mode): make the OS title bar transparent + extend the content view
/// under it (`FullSizeContentView`) so the tab strip fills the top row and the traffic-light
/// buttons float over it. `enabled=false` restores the standard stacked title bar. No-op off
/// macOS.
///
/// NOTE: deliberately does NOT set `movableByWindowBackground`. winit 0.30 doesn't override
/// `mouseDownCanMoveWindow`, so enabling it could let AppKit start a window-drag on mouse-down
/// before egui sees the event - hijacking terminal text selection / tab clicks in window mode.
/// Losing drag-by-tab-bar is the safer trade.
#[cfg(target_os = "macos")]
pub(crate) fn set_unified_titlebar(enabled: bool) {
    use objc2_app_kit::{NSApplication, NSWindowStyleMask, NSWindowTitleVisibility};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for i in 0..windows.count() {
            let w = windows.objectAtIndex(i);
            w.setTitlebarAppearsTransparent(enabled);
            w.setTitleVisibility(if enabled {
                NSWindowTitleVisibility::Hidden
            } else {
                NSWindowTitleVisibility::Visible
            });
            let mut mask = w.styleMask();
            mask.set(NSWindowStyleMask::FullSizeContentView, enabled);
            w.setStyleMask(mask);
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_unified_titlebar(_enabled: bool) {}

/// How far (points) to lower the traffic lights from their macOS default position so their row
/// centres on the taller tab strip. Dialed in by eye against a real window-mode window (the
/// headless harness can't render OS buttons). Unflipped coords: down = subtract.
const TRAFFIC_LIGHT_DROP: f64 = 5.0;

/// The absolute y to place a traffic-light button at, capturing the macOS-default y into `baseline`
/// on the FIRST call and returning `baseline - DROP` on every call thereafter. Making it ABSOLUTE
/// off a once-captured baseline (rather than nudging the current position) is what keeps re-applying
/// idempotent: no per-frame drift, and it can never fling the buttons off-screen the way relative
/// window-height math did (the 1.4.1 vanished-traffic-lights bug). Pure so the idempotence is tested.
pub(crate) fn traffic_light_y(baseline: &mut Option<f64>, current: f64) -> f64 {
    *baseline.get_or_insert(current) - TRAFFIC_LIGHT_DROP
}

/// Re-anchor the three standard window buttons onto the tab row. On the FIRST call `baseline`
/// captures their macOS-default y (before we ever move them); every call then sets an ABSOLUTE
/// `baseline - DROP`, so re-applying is idempotent (no per-frame drift) and can't fling them
/// off-screen the way the 1.4.1 window-height math did. Called each window-mode frame because
/// macOS re-lays the buttons out on resize/fullscreen/key changes. The three buttons share a row
/// (same y), so one baseline covers all; their x is left untouched.
#[cfg(target_os = "macos")]
pub(crate) fn center_window_buttons(baseline: &mut Option<f64>) {
    use objc2_app_kit::{NSApplication, NSWindowButton};
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    for i in 0..windows.count() {
        let w = windows.objectAtIndex(i);
        for b in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(btn) = w.standardWindowButton(b) {
                let mut o = btn.frame().origin;
                o.y = traffic_light_y(baseline, o.y);
                btn.setFrameOrigin(o);
            }
        }
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn center_window_buttons(_baseline: &mut Option<f64>) {}

/// Set every app window's alpha (0.0 = invisible, 1.0 = opaque). Used to make the parked quake
/// sliver invisible while hidden WITHOUT ordering the window out or moving it off-screen (either
/// of which parks eframe's run loop so the hotkey can't reshow it - see `apply_visibility`). An
/// alpha-0 window still occupies its rect on-screen and keeps drawing, so the loop stays warm.
#[cfg(target_os = "macos")]
pub(crate) fn set_window_alpha(alpha: f64) {
    use objc2_app_kit::NSApplication;
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    for i in 0..windows.count() {
        windows.objectAtIndex(i).setAlphaValue(alpha);
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_window_alpha(_alpha: f64) {}

/// Whether the whole app is the active (frontmost) macOS app. This stays TRUE when a *system*
/// panel (the emoji/character viewer, Ctrl+Cmd+Space) takes the key window - unlike winit's
/// per-window `focused`, which drops. Used to gate hide-on-blur so the emoji picker doesn't
/// dismiss the quake window; it only drops to false when another real app is activated. Off
/// macOS there's no such panel, so we report false and let winit focus drive hiding as before.
#[cfg(target_os = "macos")]
pub(crate) fn app_is_active() -> bool {
    objc2::MainThreadMarker::new()
        .is_some_and(|mtm| objc2_app_kit::NSApplication::sharedApplication(mtm).isActive())
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn app_is_active() -> bool {
    false
}

/// Whether a Cmd+V keystroke over an image-only clipboard should be swallowed (and an image paste
/// injected). Pure decision so the seam is unit-testable; the impure clipboard probe lives in
/// `clipboard_image_only`.
pub(crate) fn decide_cmd_v_image_paste(command_down: bool, is_v: bool, image_only: bool) -> bool {
    command_down && is_v && image_only
}

/// True when the system clipboard holds an image and NO usable text. Text always wins (mirrors
/// the mouse-paste decision in `workspace.rs`), so a Cmd+V with text on the clipboard is left to
/// egui's normal text-paste path.
#[cfg(target_os = "macos")]
fn clipboard_image_only() -> bool {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return false;
    };
    let has_text = cb.get_text().ok().is_some_and(|t| !t.is_empty());
    !has_text && cb.get_image().is_ok()
}

/// Install a macOS NSEvent LOCAL key-down monitor for the Cmd+V image-paste hook. It runs on the
/// main thread inside `[NSApplication sendEvent:]`, BEFORE egui-winit sees the key: for Cmd+V over
/// an image-only clipboard it bumps `paste_req` + wakes the UI and returns nil (swallowing the
/// event so egui doesn't also handle it); every other key is returned unchanged.
#[cfg(target_os = "macos")]
pub(crate) fn install_cmd_v_image_monitor(ctx: egui::Context, paste_req: Arc<AtomicUsize>) {
    use std::ptr::NonNull;

    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

    let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands us a live NSEvent for the duration of this callback.
        #[allow(unsafe_code)]
        let ev = unsafe { event.as_ref() };
        let command_down = ev.modifierFlags().contains(NSEventModifierFlags::Command);
        let is_v = ev.charactersIgnoringModifiers().is_some_and(|s| s.to_string() == "v");
        // Only probe the clipboard for the Cmd+V combo (cheap on every other key).
        let image_only = command_down && is_v && clipboard_image_only();
        if decide_cmd_v_image_paste(command_down, is_v, image_only) {
            paste_req.fetch_add(1, Ordering::SeqCst);
            ctx.request_repaint();
            std::ptr::null_mut() // swallow: egui-winit must not also process this Cmd+V
        } else {
            event.as_ptr() // pass through unchanged (normal text Cmd+V still works)
        }
    });
    // SAFETY: the handler returns a valid NSEvent pointer or null, per the monitor contract.
    #[allow(unsafe_code)]
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };
    // The monitor + block must live for the app's lifetime; leak both (removed only at exit).
    std::mem::forget(monitor);
    std::mem::forget(block);
}

/// Post a desktop notification (macOS `osascript`); `body` is the visible line. Shared by
/// notify-when-done and notify-on-activity so the osascript plumbing can't drift.
pub(crate) fn notify(body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {body:?} with title \"stdusk\"");
        let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = body;
}

/// Notify that a long command finished (exit-code aware body).
pub(crate) fn notify_done(title: &str, code: i32) {
    let status = if code == 0 { "finished".to_owned() } else { format!("failed (exit {code})") };
    notify(&format!("{title}: command {status}"));
}

#[cfg(test)]
mod tests {
    use super::{decide_cmd_v_image_paste, traffic_light_y};

    #[test]
    fn traffic_light_baseline_captured_once_and_reapply_is_idempotent() {
        // First call captures the macOS-default y as the baseline and drops from it.
        let mut baseline = None;
        let placed = traffic_light_y(&mut baseline, 30.0);
        assert_eq!(baseline, Some(30.0));
        assert_eq!(placed, 30.0 - super::TRAFFIC_LIGHT_DROP);

        // Re-applying (macOS re-lays buttons out each frame) feeds back the ALREADY-MOVED position;
        // the absolute-off-baseline math must ignore it and return the same y - no per-frame drift,
        // and never a compounding subtraction that flings the buttons off-screen (the 1.4.1 bug).
        for _ in 0..100 {
            let again = traffic_light_y(&mut baseline, placed);
            assert_eq!(again, placed, "re-apply must be idempotent, no drift");
            assert_eq!(baseline, Some(30.0), "baseline stays the once-captured default");
        }
    }

    #[test]
    fn cmd_v_image_paste_only_swallows_command_v_over_an_image() {
        // The intended case: Cmd held, "v", image-only clipboard -> swallow + inject.
        assert!(decide_cmd_v_image_paste(true, true, true));
        // No image on the clipboard: let egui's normal (text) Cmd+V through.
        assert!(!decide_cmd_v_image_paste(true, true, false));
        // Not the V key, or Command not held: never our concern.
        assert!(!decide_cmd_v_image_paste(true, false, true));
        assert!(!decide_cmd_v_image_paste(false, true, true));
        assert!(!decide_cmd_v_image_paste(false, false, false));
    }
}
