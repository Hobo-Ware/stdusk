//! What the wheel and the trackpad do over a terminal grid. The scrollback moves on the normal
//! screen; an app on the alternate screen gets arrow keys, or SGR wheel reports once it turned
//! mouse reporting on. Alt always sends arrows.

use egui::{Event, Modifiers, Pos2};
use stdusk_core::terminal::PtyTerm;

use crate::mouse::{GridGeom, MouseReporting, WheelAccumulator, wheel_report_lines, wheel_sgr};

/// Where the number of lines for the scrollback and for arrow keys comes from. SGR reports always
/// sum the raw wheel events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelLines {
    /// Raw wheel events summed across frames: no scroll is lost or invented.
    Raw,
    /// egui's per-frame smoothed delta, rounded, with at least one line per frame while it moves.
    Smoothed,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wheel {
    Scrollback(i32),
    Arrows(i32),
    Reports(i32, usize, usize),
}

pub(crate) fn route(
    lines: i32,
    mouse: MouseReporting,
    alt_screen: bool,
    alt_held: bool,
    cell: (usize, usize),
) -> Wheel {
    if alt_held {
        Wheel::Arrows(lines)
    } else if mouse.reports_buttons() && mouse.sgr {
        Wheel::Reports(wheel_report_lines(lines), cell.0, cell.1)
    } else if mouse.alternate_scroll && alt_screen {
        Wheel::Arrows(lines)
    } else {
        Wheel::Scrollback(lines)
    }
}

pub(crate) fn arrows(lines: i32, app_cursor: bool) -> Vec<u8> {
    let key: &[u8] = match (lines > 0, app_cursor) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    };
    key.repeat(lines.unsigned_abs() as usize)
}

fn scrolls(event: &Event) -> bool {
    matches!(event, Event::MouseWheel { modifiers, .. } if !zooms(*modifiers))
}

fn zooms(modifiers: Modifiers) -> bool {
    modifiers.ctrl || modifiers.command
}

fn smoothed_lines(delta_y: f32, cell_h: f32) -> i32 {
    if delta_y == 0.0 {
        return 0;
    }
    let lines = (delta_y / cell_h).round() as i32;
    if lines == 0 { delta_y.signum() as i32 } else { lines }
}

/// One frame of wheel input for one pane.
pub(crate) struct Frame<'a> {
    pub(crate) events: &'a [Event],
    pub(crate) smooth_delta_y: f32,
    pub(crate) pointer: Pos2,
    pub(crate) grid: GridGeom,
    pub(crate) source: WheelLines,
    pub(crate) alt_held: bool,
}

pub(crate) fn apply(term: &mut PtyTerm, carry: &mut WheelAccumulator, frame: &Frame) {
    let Frame { events, smooth_delta_y, pointer, grid, source, alt_held } = *frame;
    let wheel: Vec<Event> = events.iter().filter(|event| scrolls(event)).cloned().collect();
    let (lines, raw) = match source {
        WheelLines::Raw => {
            let raw = carry.lines(&wheel, grid.ch, grid.rows);
            (raw, raw)
        }
        WheelLines::Smoothed if smooth_delta_y == 0.0 => return,
        WheelLines::Smoothed => {
            (smoothed_lines(smooth_delta_y, grid.ch), carry.lines(&wheel, grid.ch, grid.rows))
        }
    };
    if lines == 0 {
        return;
    }
    let (col, row) = grid.cell(pointer);
    match route(lines, term.mouse_reporting(), term.is_alt_screen(), alt_held, (col, row)) {
        Wheel::Scrollback(lines) => term.scroll(lines),
        Wheel::Arrows(lines) => term.send(&arrows(lines, term.app_cursor())),
        Wheel::Reports(..) => term.send(&wheel_sgr(wheel_report_lines(raw), col, row)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Rect;

    const OFF: MouseReporting = MouseReporting {
        report_click: false,
        drag: false,
        motion: false,
        sgr: false,
        alternate_scroll: true,
    };
    const REPORTING: MouseReporting =
        MouseReporting { report_click: true, drag: true, sgr: true, ..OFF };

    #[test]
    fn the_wheel_goes_where_the_screen_and_the_mouse_mode_say() {
        let cases = [
            (OFF, false, Wheel::Scrollback(2)),
            (OFF, true, Wheel::Arrows(2)),
            (MouseReporting { alternate_scroll: false, ..OFF }, true, Wheel::Scrollback(2)),
            (REPORTING, true, Wheel::Reports(2, 4, 5)),
            (REPORTING, false, Wheel::Reports(2, 4, 5)),
            (MouseReporting { sgr: false, ..REPORTING }, true, Wheel::Arrows(2)),
        ];
        for (mouse, alt, want) in cases {
            assert_eq!(route(2, mouse, alt, false, (4, 5)), want, "{mouse:?} alt={alt}");
        }
    }

    #[test]
    fn alt_sends_arrows_whatever_the_screen_and_the_mouse_mode_are() {
        for mouse in [OFF, REPORTING, MouseReporting { alternate_scroll: false, ..OFF }] {
            for alt_screen in [false, true] {
                assert_eq!(route(3, mouse, alt_screen, true, (0, 0)), Wheel::Arrows(3));
            }
        }
    }

    #[test]
    fn a_flick_is_capped_for_an_app_taking_reports_but_not_for_the_scrollback() {
        assert_eq!(route(40, REPORTING, true, false, (0, 0)), Wheel::Reports(3, 0, 0));
        assert_eq!(route(-40, REPORTING, true, false, (0, 0)), Wheel::Reports(-3, 0, 0));
        assert_eq!(route(40, OFF, false, false, (0, 0)), Wheel::Scrollback(40));
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        let cases = [
            (2, false, &b"\x1b[A\x1b[A"[..]),
            (-1, false, b"\x1b[B"),
            (1, true, b"\x1bOA"),
            (-3, true, b"\x1bOB\x1bOB\x1bOB"),
            (0, false, b""),
        ];
        for (lines, app_cursor, want) in cases {
            assert_eq!(arrows(lines, app_cursor), want, "{lines} {app_cursor}");
        }
    }

    #[test]
    fn ctrl_and_cmd_wheel_is_zoom_and_not_a_scroll() {
        let wheel = |modifiers| Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers,
        };
        assert!(scrolls(&wheel(Modifiers::NONE)));
        assert!(scrolls(&wheel(Modifiers::SHIFT)));
        assert!(!scrolls(&wheel(Modifiers::CTRL)));
        assert!(!scrolls(&wheel(Modifiers::COMMAND)));
    }

    #[test]
    fn smoothed_lines_round_but_never_drop_a_moving_wheel() {
        let cases = [(0.0, 0), (40.0, 2), (-40.0, -2), (3.0, 1), (-3.0, -1), (30.0, 2)];
        for (delta, want) in cases {
            assert_eq!(smoothed_lines(delta, 20.0), want, "{delta}");
        }
    }

    #[test]
    fn the_pointer_names_a_cell_inside_the_grid() {
        let grid = GridGeom {
            rect: Rect::from_min_size(Pos2::new(100.0, 50.0), egui::vec2(800.0, 480.0)),
            cw: 10.0,
            ch: 20.0,
            cols: 80,
            rows: 24,
        };
        assert_eq!(grid.cell(Pos2::new(105.0, 55.0)), (0, 0));
        assert_eq!(grid.cell(Pos2::new(155.0, 135.0)), (5, 4));
        assert_eq!(grid.cell(Pos2::new(5000.0, 5000.0)), (79, 23));
    }
}
