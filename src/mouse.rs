//! SGR (1006) mouse reporting: the modes an app enables, the wire encoding for pointer/wheel
//! events, and the local drag-autoscroll ramp. Pure logic split out of `terminal.rs`; the
//! `Term`-backed snapshot lives in `PtyTerm::mouse_reporting`.

/// The mouse-reporting an app switched on, snapshotted from the alacritty `Term` modes. `stdusk`
/// sends NO reports unless the app asked (DECSET 1000/1002/1003 + SGR 1006); before this, TUIs
/// that enabled tracking (Claude Code's fullscreen UI, `git` list pickers) never received
/// wheel/click events and so couldn't scroll or repaint efficiently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent DECSET flags, not a state machine
pub(crate) struct MouseReporting {
    pub(crate) report_click: bool,     // ?1000 - button press + release
    pub(crate) drag: bool, // ?1002 - button-event tracking (motion while a button is down)
    pub(crate) motion: bool, // ?1003 - any-motion tracking (every move)
    pub(crate) sgr: bool,  // ?1006 - SGR extended coordinates
    pub(crate) alternate_scroll: bool, // ?1007 - wheel emits arrow keys on the alt screen
}

impl MouseReporting {
    /// A button/motion tracking mode is on, so pointer events belong to the app (not to local
    /// scroll/selection). Encoding still gates on `sgr` - we only speak SGR 1006.
    pub(crate) fn reports_buttons(self) -> bool {
        self.report_click || self.drag || self.motion
    }
}

/// SGR mouse button code for a wheel tick: 64 = up, 65 = down; `None` for a zero delta.
pub(crate) fn wheel_button(delta_lines: i32) -> Option<u8> {
    match delta_lines.signum() {
        1 => Some(64),
        -1 => Some(65),
        _ => None,
    }
}

/// Encode one pointer event in SGR 1006 form: `ESC [ < button ; col ; row (M|m)`. `col`/`row`
/// are 0-based grid cells; the wire format is 1-based, so they're bumped here. `pressed = false`
/// emits the release terminator `m`; presses, wheel ticks and motion use `M`.
pub(crate) fn sgr_mouse(button: u8, col: usize, row: usize, pressed: bool) -> Vec<u8> {
    let terminator = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{button};{};{}{terminator}", col + 1, row + 1).into_bytes()
}

/// SGR 1006 reports for a wheel scroll of `lines` at cell `(col, row)`: one report per line, as a
/// physical mouse sends one event per notch. Positive = wheel up (64), negative = down (65);
/// empty for a zero delta. Wheel events are always the `M` (pressed) form.
pub(crate) fn wheel_sgr(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let Some(button) = wheel_button(lines) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for _ in 0..lines.unsigned_abs() {
        out.extend_from_slice(&sgr_mouse(button, col, row, true));
    }
    out
}

/// Clamp a frame's wheel delta to a physical-wheel-sized burst of MOUSE-REPORT ticks. `lines` is
/// sized for LOCAL scrollback, where a big accelerated / high-res-trackpad flick is harmless -
/// alacritty caps the scroll to the available history. An app that requested mouse reporting has
/// no such backstop: every wheel report we forward scrolls its TUI another notch, so a frame of
/// tens of lines flings its view (the alt-screen over-acceleration users hit in Claude Code's
/// fullscreen UI). Cap the per-frame report count; a normal one-line notch passes through.
pub(crate) fn wheel_report_lines(lines: i32) -> i32 {
    const MAX_REPORTS: i32 = 3; // one deliberate wheel burst - never a whole accelerated flick
    lines.clamp(-MAX_REPORTS, MAX_REPORTS)
}

/// Lines to auto-scroll the viewport per frame while a selection drag is held past a pane edge,
/// so the selection can extend beyond what was visible when the drag began (standard terminal
/// behavior). Sign matches `PtyTerm::scroll`: + past the TOP edge (reveal older history), - past
/// the BOTTOM. Ramps 1..=MAX with how far past the edge the pointer sits; 0 inside the viewport.
pub(crate) fn drag_autoscroll_lines(pointer_y: f32, top: f32, bottom: f32, cell_h: f32) -> i32 {
    const MAX: i32 = 4;
    let step = |past: f32| (1 + (past / cell_h.max(1.0)) as i32).min(MAX);
    if pointer_y < top {
        step(top - pointer_y)
    } else if pointer_y > bottom {
        -step(pointer_y - bottom)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{drag_autoscroll_lines, sgr_mouse, wheel_button, wheel_report_lines, wheel_sgr};

    #[test]
    fn wheel_button_maps_direction() {
        assert_eq!(wheel_button(1), Some(64)); // wheel up
        assert_eq!(wheel_button(5), Some(64)); // magnitude ignored, sign only
        assert_eq!(wheel_button(-1), Some(65)); // wheel down
        assert_eq!(wheel_button(-3), Some(65));
        assert_eq!(wheel_button(0), None);
    }

    #[test]
    fn sgr_mouse_encodes_1based_cells_and_terminator() {
        // 0-based (col, row) -> 1-based on the wire; press = `M`, release = `m`.
        assert_eq!(sgr_mouse(0, 0, 0, true), b"\x1b[<0;1;1M".to_vec());
        assert_eq!(sgr_mouse(2, 4, 9, false), b"\x1b[<2;5;10m".to_vec()); // right-button release
        assert_eq!(sgr_mouse(64, 2, 4, true), b"\x1b[<64;3;5M".to_vec()); // wheel up
    }

    #[test]
    fn wheel_report_lines_clamps_bursts_but_keeps_a_normal_notch() {
        // A normal notch passes through unchanged; an accelerated / high-res flick clamps to a
        // small burst so a mouse-reporting TUI (alt-screen) doesn't over-scroll. Sign preserved.
        assert_eq!(wheel_report_lines(1), 1); // one-line notch untouched
        assert_eq!(wheel_report_lines(-1), -1);
        assert_eq!(wheel_report_lines(3), 3); // right at the cap
        assert_eq!(wheel_report_lines(40), 3); // big accelerated delta -> small burst
        assert_eq!(wheel_report_lines(-40), -3);
        assert_eq!(wheel_report_lines(0), 0); // no delta -> no report
    }

    #[test]
    fn wheel_sgr_emits_one_report_per_line() {
        assert_eq!(wheel_sgr(1, 2, 4), b"\x1b[<64;3;5M".to_vec());
        assert_eq!(wheel_sgr(-1, 0, 0), b"\x1b[<65;1;1M".to_vec());
        assert_eq!(wheel_sgr(2, 0, 0), b"\x1b[<64;1;1M\x1b[<64;1;1M".to_vec());
        assert!(wheel_sgr(0, 3, 3).is_empty());
    }

    #[test]
    fn drag_autoscroll_ramps_and_signs() {
        // Inside the viewport: no scroll.
        assert_eq!(drag_autoscroll_lines(50.0, 10.0, 100.0, 10.0), 0);
        assert_eq!(drag_autoscroll_lines(10.0, 10.0, 100.0, 10.0), 0); // exactly at the top edge
        // Past the TOP edge -> positive (reveal older history), ramps with distance, capped at 4.
        assert_eq!(drag_autoscroll_lines(5.0, 10.0, 100.0, 10.0), 1); // <1 cell over
        assert_eq!(drag_autoscroll_lines(-100.0, 10.0, 100.0, 10.0), 4); // far over -> cap
        // Past the BOTTOM edge -> negative (reveal newer content).
        assert_eq!(drag_autoscroll_lines(105.0, 10.0, 100.0, 10.0), -1);
        assert_eq!(drag_autoscroll_lines(1000.0, 10.0, 100.0, 10.0), -4);
    }
}
