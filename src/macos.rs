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

/// Every screen's frame in AppKit space (bottom-left origin, y up); index 0 is the primary.
#[cfg(target_os = "macos")]
fn screen_frames(mtm: objc2::MainThreadMarker) -> Vec<egui::Rect> {
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    (0..screens.count()).map(|i| ns_rect(screens.objectAtIndex(i).frame())).collect()
}

#[cfg(target_os = "macos")]
fn ns_rect(r: objc2_foundation::NSRect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(r.origin.x as f32, r.origin.y as f32),
        egui::vec2(r.size.width as f32, r.size.height as f32),
    )
}

/// The screen under the mouse cursor, as a top-left-origin rect in `OuterPosition` space. The
/// quake drop targets this so the terminal appears on the monitor you are working on. `None`
/// when the cursor is on no screen or off macOS.
#[cfg(target_os = "macos")]
pub(crate) fn cursor_screen() -> Option<egui::Rect> {
    let mtm = objc2::MainThreadMarker::new()?;
    let frames = screen_frames(mtm);
    let mouse = objc2_app_kit::NSEvent::mouseLocation();
    let i = crate::ui::screen_index_at(&frames, egui::pos2(mouse.x as f32, mouse.y as f32))?;
    Some(crate::ui::cocoa_to_top_left(frames[i], frames.first()?.height()))
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn cursor_screen() -> Option<egui::Rect> {
    None
}

/// The primary (menu-bar) screen in `OuterPosition` space: the last resort when neither the cursor
/// nor the window is on a known screen, so the hidden sliver still lands on a live screen.
#[cfg(target_os = "macos")]
pub(crate) fn primary_screen() -> Option<egui::Rect> {
    let frame = *screen_frames(objc2::MainThreadMarker::new()?).first()?;
    Some(crate::ui::cocoa_to_top_left(frame, frame.height()))
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn primary_screen() -> Option<egui::Rect> {
    None
}

/// The screen the app window is on now, in the same space as `cursor_screen`. Hiding parks the
/// sliver here, so it stays on a live screen. `None` when unknown or off macOS.
#[cfg(target_os = "macos")]
pub(crate) fn window_screen() -> Option<egui::Rect> {
    use objc2_app_kit::NSApplication;
    let mtm = objc2::MainThreadMarker::new()?;
    let windows = NSApplication::sharedApplication(mtm).windows();
    // The tray's status-item window is also in this list, but it can never become key.
    let win =
        (0..windows.count()).map(|i| windows.objectAtIndex(i)).find(|w| w.canBecomeKeyWindow())?;
    let frame = ns_rect(win.screen()?.frame());
    Some(crate::ui::cocoa_to_top_left(frame, screen_frames(mtm).first()?.height()))
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn window_screen() -> Option<egui::Rect> {
    None
}

/// Is the OS appearance DARK? `None` means "no OS answer here" (non-macOS), so the caller falls
/// back to what the window system reported.
///
/// Read from the `AppleInterfaceStyle` global default, NOT `NSApp.effectiveAppearance`: the default
/// is written by the OS before our process exists and needs no main thread or finished launch, so
/// it is already correct while `Stdusk::new` picks the startup theme. It also tracks the "Auto"
/// appearance schedule - the key is present ("Dark") only during the dark half, absent for light.
#[cfg(target_os = "macos")]
// The `Option` is the CROSS-PLATFORM contract ("is there an OS answer at all"); the macOS arm
// always has one, which is exactly what clippy sees and what the stub below contradicts.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn os_dark_mode() -> Option<bool> {
    use objc2_foundation::{NSString, NSUserDefaults};
    let style = NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"));
    // Absent key = Light; macOS stores only the dark state.
    Some(style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark")))
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn os_dark_mode() -> Option<bool> {
    None
}

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
    let has_text = cb.get_text().is_ok_and(|t| !t.is_empty());
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

// --- Open at login (SMAppService) --------------------------------------------------------------

/// Where the "Open at login" item stands. `status()` is the only source of truth: the user can
/// also change the item in System Settings > General > Login Items, so stdusk stores nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginItem {
    Off,
    On,
    /// Registered, but the user must approve it in System Settings.
    NeedsApproval,
    /// macOS has no record yet; first registration can still succeed.
    NotFound,
    /// The API is unavailable or returned an unknown status.
    Unavailable,
}

/// Map `SMAppServiceStatus`' raw value (0 not registered, 1 enabled, 2 requires approval,
/// 3 not found). Not found is recoverable by registration; unknown values are unavailable.
pub(crate) fn login_item_from_raw(raw: isize) -> LoginItem {
    match raw {
        0 => LoginItem::Off,
        1 => LoginItem::On,
        2 => LoginItem::NeedsApproval,
        3 => LoginItem::NotFound,
        _ => LoginItem::Unavailable,
    }
}

/// The current login-item state of this app.
#[cfg(target_os = "macos")]
pub(crate) fn login_item_status() -> LoginItem {
    use objc2_service_management::SMAppService;
    if !login_item_api_available() {
        return LoginItem::Unavailable;
    }
    // SAFETY: `mainAppService` and `status` take no pointers and have no preconditions. They are
    // `unsafe fn` in the generated binding only because every ObjC method is.
    #[allow(unsafe_code)]
    let raw = unsafe { SMAppService::mainAppService().status().0 };
    login_item_from_raw(raw)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn login_item_status() -> LoginItem {
    LoginItem::Unavailable
}

/// Register (`true`) or unregister the app as a login item. The error is the OS text, for a toast.
#[cfg(target_os = "macos")]
pub(crate) fn set_login_item(enabled: bool) -> Result<(), String> {
    use objc2_service_management::SMAppService;
    if !login_item_api_available() {
        return Err("Open at login needs macOS 13 or newer".to_owned());
    }
    // SAFETY: same as `login_item_status`. The NSError out-parameter is handled by the binding,
    // which returns it as `Err`.
    #[allow(unsafe_code)]
    let result = unsafe {
        let service = SMAppService::mainAppService();
        if enabled { service.registerAndReturnError() } else { service.unregisterAndReturnError() }
    };
    result.map_err(|e| e.localizedDescription().to_string())
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_login_item(_enabled: bool) -> Result<(), String> {
    Err("Open at login needs macOS".to_owned())
}

/// Open System Settings > General > Login Items, where the user approves a pending item.
#[cfg(target_os = "macos")]
pub(crate) fn open_login_items_settings() {
    if !login_item_api_available() {
        return;
    }
    // SAFETY: a class method with no arguments and no preconditions.
    #[allow(unsafe_code)]
    unsafe {
        objc2_service_management::SMAppService::openSystemSettingsLoginItems();
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn open_login_items_settings() {}

/// Is this a build that can register a login item: macOS, running from an `.app` bundle? A
/// `cargo run` binary has no bundle to register.
pub(crate) fn login_item_supported(exe: &std::path::Path) -> bool {
    cfg!(target_os = "macos")
        && login_item_api_available()
        && crate::update::bundle_path(exe).is_some()
}

/// `SMAppService` was added in macOS 13. objc2's typed class cache assumes a class exists, so
/// check the Objective-C runtime first on every public entry point that uses the API.
#[cfg(target_os = "macos")]
fn login_item_api_available() -> bool {
    objc2::runtime::AnyClass::get(c"SMAppService").is_some()
}

#[cfg(not(target_os = "macos"))]
fn login_item_api_available() -> bool {
    false
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

    /// The objc read must answer the SAME thing the OS stores, on whatever appearance this machine
    /// happens to be in - including the light half of "Auto", where the key is simply absent. A
    /// silent `None`/always-false here is what painted a follow-system window in the wrong theme.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_dark_mode_read_agrees_with_the_defaults_database() {
        let out = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .expect("defaults is part of macOS");
        let stored_dark = out.status.success()
            && String::from_utf8_lossy(&out.stdout).trim().eq_ignore_ascii_case("dark");
        assert_eq!(super::os_dark_mode(), Some(stored_dark));
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

    #[test]
    fn login_item_status_maps_every_service_management_value() {
        use super::{LoginItem, login_item_from_raw};
        let cases = [
            (0, LoginItem::Off),
            (1, LoginItem::On),
            (2, LoginItem::NeedsApproval),
            (3, LoginItem::NotFound),
            (99, LoginItem::Unavailable),
            (-1, LoginItem::Unavailable),
        ];
        for (raw, want) in cases {
            assert_eq!(login_item_from_raw(raw), want, "raw {raw}");
        }
    }

    #[test]
    fn open_at_login_is_offered_only_from_an_app_bundle() {
        use std::path::Path;
        let bundle = Path::new("/Applications/stdusk.app/Contents/MacOS/stdusk");
        assert_eq!(
            super::login_item_supported(bundle),
            cfg!(target_os = "macos") && super::login_item_api_available()
        );
        assert!(!super::login_item_supported(Path::new("/repo/target/debug/stdusk")));
    }

    /// Reading the status is a plain read and must work in a bare test binary (no bundle): the
    /// service reports "not registered" or "not found", never a crash. It never registers anything.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "makes a live ServiceManagement call: run it by hand with --ignored"]
    fn reading_the_login_item_status_is_safe_outside_a_bundle() {
        let status = super::login_item_status();
        assert!(
            matches!(
                status,
                super::LoginItem::Off | super::LoginItem::NotFound | super::LoginItem::Unavailable
            ),
            "{status:?}"
        );
    }
}
