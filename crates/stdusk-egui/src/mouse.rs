//! egui side of mouse reporting: pointer/wheel events -> SGR reports, plus the grid geometry.

pub use stdusk_core::mouse::{
    MouseReporting, drag_autoscroll_lines, sgr_mouse, wheel_button, wheel_report_lines, wheel_sgr,
};

pub trait MouseReportingExt {
    fn app_owns_pointer(self, mods: egui::Modifiers, link_key: &str, input_captured: bool) -> bool;
}

impl MouseReportingExt for MouseReporting {
    /// Pointer events go to the app this frame. Shift (select) and the `app_link_modifier` key
    /// (open a link, Cmd by default) take the pointer back locally, as in iTerm2.
    fn app_owns_pointer(self, mods: egui::Modifiers, link_key: &str, input_captured: bool) -> bool {
        self.reports_buttons()
            && self.sgr
            && !mods.shift
            && !app_link_key_held(mods, link_key)
            && !input_captured
    }
}

/// `terminal.app_link_modifier`: unknown values mean Cmd (the default), never "no key", so a
/// typo can't hand every click back from the app.
fn app_link_key_held(mods: egui::Modifiers, setting: &str) -> bool {
    match setting.to_ascii_lowercase().as_str() {
        "alt" | "option" | "opt" => mods.alt,
        "ctrl" | "control" => mods.ctrl,
        _ => mods.mac_cmd,
    }
}

/// Raw wheel lines for mouse reports; egui's `smooth_scroll_delta` spreads one notch over ~12 frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct WheelAccumulator {
    carry: f32,
}

impl WheelAccumulator {
    pub fn lines(&mut self, events: &[egui::Event], cell_h: f32, page_rows: usize) -> i32 {
        for event in events {
            let egui::Event::MouseWheel { unit, delta, phase, .. } = event else { continue };
            if *phase == egui::TouchPhase::Start {
                self.carry = 0.0;
            }
            self.carry += match unit {
                egui::MouseWheelUnit::Line => delta.y,
                egui::MouseWheelUnit::Point => delta.y / cell_h,
                egui::MouseWheelUnit::Page => delta.y * page_rows as f32,
            };
        }
        let whole = self.carry.trunc();
        self.carry -= whole;
        whole as i32
    }
}

/// A pane's grid in screen space, for mapping pointer positions to 0-based viewport cells.
#[derive(Clone, Copy, Debug)]
pub struct GridGeom {
    pub rect: egui::Rect,
    pub cw: f32,
    pub ch: f32,
    pub cols: usize,
    pub rows: usize,
}

impl GridGeom {
    fn cell(self, p: egui::Pos2) -> (usize, usize) {
        let col = ((p.x - self.rect.left()) / self.cw).max(0.0) as usize;
        let row = ((p.y - self.rect.top()) / self.ch).max(0.0) as usize;
        (col.min(self.cols.saturating_sub(1)), row.min(self.rows.saturating_sub(1)))
    }
}

/// Per-pane pointer state carried between frames: whether the left button went down inside the
/// grid, and the last cell a motion report named (motion is reported once per cell change).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerTracker {
    held: bool,
    last_cell: Option<(usize, usize)>,
}

const LEFT: u8 = 0;

const NO_BUTTON: u8 = 3;

const MOTION: u8 = 32;

fn modifier_bits(m: egui::Modifiers) -> u8 {
    (if m.alt { 8 } else { 0 }) | (if m.ctrl { 16 } else { 0 })
}

/// SGR 1006 reports for this frame's left-button and motion events on one pane. Presses count
/// only inside the grid; a release or a held-button drag is reported wherever the pointer went
/// (clamped to the grid), so the app never sees a press without its release.
pub fn pointer_reports(
    events: &[egui::Event],
    grid: GridGeom,
    mr: MouseReporting,
    tracker: &mut PointerTracker,
) -> Vec<u8> {
    let mut out = Vec::new();
    for ev in events {
        match *ev {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            } => {
                if pressed && !grid.rect.contains(pos) {
                    continue;
                }
                if !pressed && !tracker.held {
                    continue;
                }
                let (col, row) = grid.cell(pos);
                out.extend(sgr_mouse(LEFT | modifier_bits(modifiers), col, row, pressed));
                tracker.held = pressed;
                tracker.last_cell = Some((col, row));
            }
            egui::Event::PointerMoved(pos) => {
                let wanted = mr.motion || (mr.drag && tracker.held);
                if !wanted || (!tracker.held && !grid.rect.contains(pos)) {
                    continue;
                }
                let cell = grid.cell(pos);
                if tracker.last_cell == Some(cell) {
                    continue;
                }
                tracker.last_cell = Some(cell);
                let button = if tracker.held { LEFT } else { NO_BUTTON };
                out.extend(sgr_mouse(button | MOTION, cell.0, cell.1, true));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {

    use super::{
        GridGeom, MouseReporting, MouseReportingExt, PointerTracker, WheelAccumulator,
        pointer_reports,
    };

    fn grid() -> GridGeom {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 480.0));
        GridGeom { rect, cw: 10.0, ch: 20.0, cols: 80, rows: 24 }
    }

    fn at(col: f32, row: f32) -> egui::Pos2 {
        egui::pos2(100.0 + col * 10.0 + 5.0, 50.0 + row * 20.0 + 5.0)
    }

    fn left(pos: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers }
    }

    fn reports(events: &[egui::Event], mr: MouseReporting, t: &mut PointerTracker) -> String {
        String::from_utf8(pointer_reports(events, grid(), mr, t)).expect("reports are ASCII")
    }

    const CLICKS: MouseReporting = MouseReporting {
        report_click: true,
        drag: false,
        motion: false,
        sgr: true,
        alternate_scroll: false,
    };

    #[test]
    fn shift_and_the_link_key_take_the_pointer_back_from_the_app() {
        let none = egui::Modifiers::default();
        let shift = egui::Modifiers { shift: true, ..none };
        let cmd = egui::Modifiers { mac_cmd: true, command: true, ..none };
        let ctrl = egui::Modifiers { ctrl: true, ..none };
        let alt = egui::Modifiers { alt: true, ..none };
        let legacy = MouseReporting { sgr: false, ..CLICKS };
        let cases = [
            (CLICKS, none, "cmd", false, true),
            (CLICKS, ctrl, "cmd", false, true),
            (CLICKS, shift, "cmd", false, false),
            (CLICKS, cmd, "cmd", false, false),
            (CLICKS, cmd, "alt", false, true),
            (CLICKS, alt, "alt", false, false),
            (CLICKS, ctrl, "Ctrl", false, false),
            (CLICKS, shift, "ctrl", false, false),
            (CLICKS, none, "none", false, true),
            (CLICKS, cmd, "typo", false, false),
            (CLICKS, none, "cmd", true, false),
            (legacy, none, "cmd", false, false),
            (MouseReporting::default(), none, "cmd", false, false),
        ];
        for (mr, mods, key, captured, want) in cases {
            assert_eq!(
                mr.app_owns_pointer(mods, key, captured),
                want,
                "{mr:?} {mods:?} {key} {captured}"
            );
        }
    }

    #[test]
    fn a_click_reports_press_then_release_at_its_cell() {
        let mut t = PointerTracker::default();
        let none = egui::Modifiers::default();
        let got = reports(
            &[left(at(78.0, 0.0), true, none), left(at(78.0, 0.0), false, none)],
            CLICKS,
            &mut t,
        );
        assert_eq!(got, "\x1b[<0;79;1M\x1b[<0;79;1m");
    }

    #[test]
    fn presses_outside_the_grid_are_ignored_but_a_held_release_is_not() {
        let none = egui::Modifiers::default();
        let outside = egui::pos2(10.0, 10.0);
        let mut t = PointerTracker::default();
        assert_eq!(
            reports(&[left(outside, true, none), left(outside, false, none)], CLICKS, &mut t),
            ""
        );
        let mut t = PointerTracker::default();
        let got =
            reports(&[left(at(2.0, 3.0), true, none), left(outside, false, none)], CLICKS, &mut t);
        assert_eq!(got, "\x1b[<0;3;4M\x1b[<0;1;1m", "release clamps to the grid edge");
    }

    #[test]
    fn modifiers_set_the_alt_and_ctrl_bits() {
        let mut t = PointerTracker::default();
        let m = egui::Modifiers { alt: true, ctrl: true, ..Default::default() };
        assert_eq!(reports(&[left(at(0.0, 0.0), true, m)], CLICKS, &mut t), "\x1b[<24;1;1M");
    }

    #[test]
    fn motion_follows_the_enabled_tracking_mode_once_per_cell() {
        let none = egui::Modifiers::default();
        let moves = [
            egui::Event::PointerMoved(at(1.0, 1.0)),
            egui::Event::PointerMoved(at(1.2, 1.0)),
            egui::Event::PointerMoved(at(2.0, 1.0)),
        ];
        let mut t = PointerTracker::default();
        assert_eq!(reports(&moves, CLICKS, &mut t), "", "click-only mode reports no motion");

        let any = MouseReporting { motion: true, ..CLICKS };
        let mut t = PointerTracker::default();
        assert_eq!(reports(&moves, any, &mut t), "\x1b[<35;2;2M\x1b[<35;3;2M");

        let drag = MouseReporting { drag: true, ..CLICKS };
        let mut t = PointerTracker::default();
        assert_eq!(reports(&moves, drag, &mut t), "", "button-event mode needs a held button");
        let mut held = vec![left(at(0.0, 1.0), true, none)];
        held.extend(moves);
        assert_eq!(reports(&held, drag, &mut t), "\x1b[<0;1;2M\x1b[<32;2;2M\x1b[<32;3;2M");
    }

    fn wheel(unit: egui::MouseWheelUnit, dy: f32) -> egui::Event {
        egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn one_wheel_notch_is_one_report_however_many_frames_follow() {
        let mut acc = WheelAccumulator::default();
        let frames = [
            acc.lines(&[wheel(egui::MouseWheelUnit::Line, 1.0)], 20.0, 24),
            acc.lines(&[], 20.0, 24),
            acc.lines(&[egui::Event::PointerGone], 20.0, 24),
        ];
        assert_eq!(frames, [1, 0, 0]);
        assert_eq!(acc.lines(&[wheel(egui::MouseWheelUnit::Line, -1.0)], 20.0, 24), -1);
    }

    #[test]
    fn trackpad_points_report_once_per_cell_height_and_carry_the_rest() {
        let mut acc = WheelAccumulator::default();
        let step = [wheel(egui::MouseWheelUnit::Point, 5.0)];
        let frames: Vec<i32> = (0..8).map(|_| acc.lines(&step, 20.0, 24)).collect();
        assert_eq!(frames, [0, 0, 0, 1, 0, 0, 0, 1]);
        let back = [wheel(egui::MouseWheelUnit::Point, -45.0)];
        assert_eq!(acc.lines(&back, 20.0, 24), -2);
    }

    #[test]
    fn a_new_trackpad_gesture_drops_the_last_gestures_leftover() {
        let mut acc = WheelAccumulator::default();
        assert_eq!(acc.lines(&[wheel(egui::MouseWheelUnit::Point, 19.0)], 20.0, 24), 0);
        let start = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::Vec2::ZERO,
            phase: egui::TouchPhase::Start,
            modifiers: egui::Modifiers::NONE,
        };
        let next_swipe = [start, wheel(egui::MouseWheelUnit::Point, 5.0)];
        assert_eq!(acc.lines(&next_swipe, 20.0, 24), 0);
    }

    #[test]
    fn wheel_events_in_one_frame_sum_and_pages_scale_by_rows() {
        let mut acc = WheelAccumulator::default();
        let burst =
            [wheel(egui::MouseWheelUnit::Line, 1.0), wheel(egui::MouseWheelUnit::Line, 2.0)];
        assert_eq!(acc.lines(&burst, 20.0, 24), 3);
        assert_eq!(acc.lines(&[wheel(egui::MouseWheelUnit::Page, -1.0)], 20.0, 24), -24);
    }
}
