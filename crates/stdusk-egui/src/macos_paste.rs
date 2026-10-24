//! Cmd+V over a clipboard picture. egui-winit folds Cmd+V into a text-only `Event::Paste` and
//! sends nothing for an image-only clipboard, so a local key-down monitor sees the press first.
//! What to do with the picture stays with the app: it drains [`ImagePasteHook::take`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Whether a key press is swallowed as an image paste. `picture` is only asked for Cmd+V.
pub fn swallows_cmd_v(only_command: bool, is_v: bool, picture: impl FnOnce() -> bool) -> bool {
    only_command && is_v && picture()
}

/// Counts the Cmd+V presses the monitor swallowed, for the app to drain once per frame.
#[derive(Clone, Debug, Default)]
pub struct ImagePasteHook {
    requests: Arc<AtomicUsize>,
}

impl ImagePasteHook {
    pub fn request(&self) {
        self.requests.fetch_add(1, Ordering::SeqCst);
    }

    pub fn take(&self) -> usize {
        self.requests.swap(0, Ordering::SeqCst)
    }

    /// Start the monitor: a Cmd+V (and no other modifier) for which `picture_on_clipboard` says
    /// yes is swallowed and counted, every other key passes untouched. It runs on the main thread
    /// inside `[NSApplication sendEvent:]`, so the predicate must be cheap. No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn install_monitor(
        &self,
        ctx: &egui::Context,
        picture_on_clipboard: impl Fn() -> bool + 'static,
    ) {
        use std::ptr::NonNull;

        use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

        let (hook, ctx) = (self.clone(), ctx.clone());
        let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a live NSEvent for the duration of the call.
            #[allow(unsafe_code)]
            let ev = unsafe { event.as_ref() };
            let held = ev.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
            let only_command = held == NSEventModifierFlags::Command;
            let is_v = ev.charactersIgnoringModifiers().is_some_and(|s| s.to_string() == "v");
            if swallows_cmd_v(only_command, is_v, &picture_on_clipboard) {
                if !ev.isARepeat() {
                    hook.request();
                    ctx.request_repaint();
                }
                return std::ptr::null_mut();
            }
            event.as_ptr()
        });
        // SAFETY: the handler returns the event it was given or null, as the monitor contract allows.
        #[allow(unsafe_code)]
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
        };
        std::mem::forget(monitor);
        std::mem::forget(block);
    }

    #[cfg(not(target_os = "macos"))]
    pub fn install_monitor(
        &self,
        _ctx: &egui::Context,
        _picture_on_clipboard: impl Fn() -> bool + 'static,
    ) {
    }
}

/// Queues a Cmd+V key press in the app's own event queue, so it passes the monitor like a real
/// one. For dev switches that exercise the paste without a keyboard.
#[cfg(target_os = "macos")]
pub fn post_cmd_v() -> bool {
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSPoint, NSString};

    let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
    let v = NSString::from_str("v");
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::Command,
        0.0,
        0,
        None,
        &v,
        &v,
        false,
        9,
    );
    let Some(event) = event else { return false };
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    true
}

#[cfg(not(target_os = "macos"))]
pub fn post_cmd_v() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_cmd_v_over_a_picture_is_swallowed() {
        let cases = [
            (true, true, true, true),
            (true, true, false, false),
            (true, false, true, false),
            (false, true, true, false),
            (false, false, false, false),
        ];
        for (only_command, is_v, picture, want) in cases {
            assert_eq!(swallows_cmd_v(only_command, is_v, || picture), want);
        }
    }

    #[test]
    fn the_clipboard_is_not_asked_about_other_keys() {
        assert!(!swallows_cmd_v(false, true, || panic!("asked")));
        assert!(!swallows_cmd_v(true, false, || panic!("asked")));
    }

    #[test]
    fn requests_are_counted_and_drained() {
        let hook = ImagePasteHook::default();
        hook.request();
        hook.request();
        assert_eq!(hook.take(), 2);
        assert_eq!(hook.take(), 0);
    }
}
