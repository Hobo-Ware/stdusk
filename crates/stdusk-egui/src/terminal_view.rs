//! One terminal pane as a unit: the keys, pastes, wheel, pointer reports, focus lock and copy
//! that belong to a grid, around `ui::render_grid`. The app decides who may type and hands the
//! view `Gate`; the view hands back facts (`TerminalOutput`) and never touches tabs, toasts or
//! windows.

use egui::{Event, FontId, Id, Key, Modifiers};
use stdusk_core::paste::{self, PasteRules};
use stdusk_core::search::Match;
use stdusk_core::terminal::{ECHO_WAIT, PtyTerm};

use crate::focus;
use crate::mouse::{
    GridGeom, MouseReportingExt as _, PointerTracker, WheelAccumulator, drag_autoscroll_lines,
    pointer_reports,
};
use crate::ui::{GridStyle, collect_input, render_grid};
use crate::wheel::{self, WheelLines};

/// Who may use this frame's input. The app knows its modals and which pane is focused.
// Independent permissions, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// Key presses and typed text go to the pty (and Ctrl+C may copy).
    pub keyboard: bool,
    /// `Event::Paste` goes to the pty.
    pub paste: bool,
    /// `Event::Copy` copies the selection.
    pub copy: bool,
    /// The wheel scrolls, sends arrows or reports.
    pub wheel: bool,
    /// An app with mouse reporting on gets clicks, drags and motion.
    pub reports: bool,
}

impl Gate {
    pub const fn all(allowed: bool) -> Self {
        Self { keyboard: allowed, paste: allowed, copy: allowed, wheel: allowed, reports: allowed }
    }
}

/// How the view treats egui's keyboard focus for the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// Take focus when nothing has it, give it up when the keys are not free, lock the shell's keys.
    Manage,
    /// Lock the shell's keys while they are free; the app requests focus itself.
    LockKeys,
    Off,
}

// Independent switches, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub struct TerminalOptions<'a> {
    pub alt_is_meta: bool,
    pub paste: PasteRules,
    /// Hand a multiline paste outside the alternate screen back in `confirm_paste` instead of
    /// sending it.
    pub confirm_multiline_paste: bool,
    /// Ctrl+C copies and clears a selection and is ^C only without one.
    pub ctrl_c_copies: bool,
    pub copy_on_select: bool,
    pub wheel_lines: WheelLines,
    /// `terminal.app_link_modifier`: the key that takes the pointer back from an app.
    pub app_link_modifier: &'a str,
    pub focus: Focus,
}

pub struct TerminalView<'a> {
    pub id_src: &'a [stdusk_core::pane::Side],
    pub grid: GridGeom,
    pub font: &'a FontId,
    pub bold_font: Option<&'a FontId>,
    /// `app_mouse` is decided by the view and overwritten.
    pub style: GridStyle,
    pub search_marks: &'a [Match],
    pub options: TerminalOptions<'a>,
    pub gate: Gate,
    /// Wheel events from outside egui (dev switches); they count as the pointer being over the grid.
    pub injected_wheel: &'a [Event],
}

pub struct TerminalOutput {
    pub response: egui::Response,
    /// Bytes sent to the pty from the keyboard.
    pub typed: Vec<u8>,
    /// Pastes sent to the pty, after normalizing.
    pub pasted: Vec<String>,
    /// Multiline pastes held back (normalized, untrimmed) for the app to ask about.
    pub confirm_paste: Vec<String>,
    /// Text put on the clipboard this frame.
    pub copied: Option<String>,
    /// An app with mouse reporting on owns the pointer this frame.
    pub app_owns_pointer: bool,
}

impl TerminalOutput {
    pub fn sent_input(&self) -> bool {
        !self.typed.is_empty() || !self.pasted.is_empty()
    }
}

/// User input for a pty: send it, drop the selection, follow the output, and wait a moment for the
/// echo so the frame that sent the key paints it.
pub fn send_typed(term: &mut PtyTerm, bytes: &[u8]) {
    let since = term.output_gen();
    term.send(bytes);
    term.clear_selection();
    term.scroll_to_bottom();
    let _ = term.wait_for_output_after(since, ECHO_WAIT);
}

/// A prepared paste: bracketed when the app asked, selection dropped, view at the bottom.
pub fn paste_into(term: &mut PtyTerm, text: &str) {
    term.paste(text);
    term.clear_selection();
    term.scroll_to_bottom();
}

struct Copy {
    wanted: bool,
    ctrl_c: bool,
    intercept_ctrl_c: bool,
}

fn copy_request(
    events: &[Event],
    mods: Modifiers,
    ctrl_c_key: bool,
    options: &TerminalOptions,
    gate: Gate,
    has_selection: bool,
    macos: bool,
) -> Copy {
    let copy_event = events.iter().any(|event| matches!(event, Event::Copy));
    let folded_ctrl_c = !macos && mods.ctrl && !mods.shift && copy_event;
    let ctrl_c = options.ctrl_c_copies && ((mods.ctrl && ctrl_c_key) || folded_ctrl_c);
    let plain_copy = copy_event && (options.ctrl_c_copies || !folded_ctrl_c);
    Copy {
        wanted: gate.copy && (plain_copy || (ctrl_c && has_selection && gate.keyboard)),
        ctrl_c,
        intercept_ctrl_c: ctrl_c && has_selection,
    }
}

impl TerminalView<'_> {
    pub fn show(self, ui: &mut egui::Ui, term: &mut PtyTerm) -> TerminalOutput {
        let Self {
            id_src,
            grid,
            font,
            bold_font,
            style,
            search_marks,
            options,
            gate,
            injected_wheel,
        } = self;
        let ctx = ui.ctx().clone();
        let (events, mods, ctrl_c_key) =
            ui.input(|i| (i.events.clone(), i.modifiers, i.key_pressed(Key::C)));
        let mut out = Vec::new();
        let mut pasted = Vec::new();
        let mut confirm_paste = Vec::new();
        let mut copied = None;

        let selection = term.selection_text();
        let copy = copy_request(
            &events,
            mods,
            ctrl_c_key,
            &options,
            gate,
            selection.is_some(),
            cfg!(target_os = "macos"),
        );
        if copy.wanted
            && let Some(text) = selection
        {
            ctx.copy_text(text.clone());
            if copy.ctrl_c {
                term.clear_selection();
            }
            copied = Some(text);
        }

        if gate.keyboard {
            let typed =
                collect_input(ui, options.alt_is_meta, copy.intercept_ctrl_c, term.app_cursor());
            if !typed.is_empty() {
                send_typed(term, &typed);
                out = typed;
            }
        }

        if gate.paste {
            for event in &events {
                let Event::Paste(text) = event else { continue };
                let flat = paste::normalize(text, options.paste.newlines_to_spaces);
                if options.confirm_multiline_paste && flat.contains('\r') && !term.is_alt_screen() {
                    confirm_paste.push(flat);
                    ctx.request_repaint();
                    continue;
                }
                let text = paste::trim(&flat, options.paste.trim);
                if !text.is_empty() {
                    paste_into(term, &text);
                    pasted.push(text);
                }
            }
        }

        let reporting = term.mouse_reporting();
        let app_owns_pointer =
            reporting.app_owns_pointer(mods, options.app_link_modifier, !gate.reports);
        if app_owns_pointer {
            let id = Id::new(("pointer_tracker", id_src));
            let mut tracker: PointerTracker = ui.data(|d| d.get_temp(id)).unwrap_or_default();
            let bytes = pointer_reports(&events, grid, reporting, &mut tracker);
            ui.data_mut(|d| d.insert_temp(id, tracker));
            if !bytes.is_empty() {
                term.send(&bytes);
            }
        }

        if gate.wheel {
            let hovered = ui.input(|i| i.pointer.hover_pos());
            let pointer =
                hovered.or_else(|| (!injected_wheel.is_empty()).then(|| grid.rect.center()));
            if let Some(pointer) = pointer.filter(|p| grid.rect.contains(*p)) {
                let id = Id::new(("wheel_carry", id_src));
                let mut carry: WheelAccumulator = ui.data(|d| d.get_temp(id)).unwrap_or_default();
                let mut wheel_events = events.clone();
                wheel_events.extend_from_slice(injected_wheel);
                let frame = wheel::Frame {
                    events: &wheel_events,
                    smooth_delta_y: ui.input(|i| i.smooth_scroll_delta.y),
                    pointer,
                    grid,
                    source: options.wheel_lines,
                    alt_held: mods.alt,
                };
                wheel::apply(term, &mut carry, &frame);
                ui.data_mut(|d| d.insert_temp(id, carry));
            }
        }

        let snap = term.grid_snapshot();
        let response = render_grid(
            ui,
            id_src,
            grid.rect,
            term,
            &snap,
            grid.cw,
            grid.ch,
            font,
            bold_font,
            GridStyle { app_mouse: app_owns_pointer, ..style },
            search_marks,
        );
        match options.focus {
            Focus::Manage => focus::hold(&ctx, response.id, gate.keyboard),
            Focus::LockKeys => focus::lock_keys(&ctx, response.id, gate.keyboard),
            Focus::Off => {}
        }

        if response.dragged()
            && !reporting.reports_buttons()
            && let Some(p) = response.interact_pointer_pos()
        {
            let lines = drag_autoscroll_lines(p.y, grid.rect.top(), grid.rect.bottom(), grid.ch);
            if lines != 0 {
                term.scroll(lines);
                ctx.request_repaint();
            }
        }
        if options.copy_on_select
            && (response.drag_stopped() || response.double_clicked() || response.triple_clicked())
            && let Some(text) = term.selection_text().filter(|t| !t.trim().is_empty())
        {
            ctx.copy_text(text.clone());
            copied = Some(text);
        }

        TerminalOutput { response, typed: out, pasted, confirm_paste, copied, app_owns_pointer }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use egui::{Context, MouseWheelUnit, OutputCommand, Pos2, Rect, TouchPhase};
    use stdusk_core::test_support::spawn_opts;

    use super::*;
    use crate::ui::CursorStyle;

    const CELL: (f32, f32) = (8.0, 16.0);
    const COLS: usize = 75;
    const ROWS: usize = 25;
    const RULES: PasteRules = PasteRules { newlines_to_spaces: false, trim: true };

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static CALLS: AtomicUsize = AtomicUsize::new(0);
            let call = CALLS.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("stdusk-tv-{}-{call}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Rig {
        ctx: Context,
        term: PtyTerm,
        scratch: Scratch,
        text: String,
        button: Option<egui::Response>,
        field: Option<egui::Response>,
        out: Option<TerminalOutput>,
        copied_to_clipboard: Vec<String>,
        ctrl_c_copies: bool,
        copy_on_select: bool,
        confirm: bool,
        wheel_lines: WheelLines,
    }

    impl Rig {
        fn new(script: &str) -> Self {
            let scratch = Scratch::new();
            let script = script
                .replace("OUT", &scratch.0.join("got").display().to_string())
                .replace("READY", &scratch.0.join("ready").display().to_string());
            let mut term = PtyTerm::spawn(
                COLS,
                ROWS,
                stdusk_core::repaint::Repaint::none(),
                &spawn_opts("/bin/sh", &["-c", &script]),
            );
            term.resize(COLS, ROWS);
            Self {
                ctx: Context::default(),
                term,
                scratch,
                text: String::new(),
                button: None,
                field: None,
                out: None,
                copied_to_clipboard: Vec::new(),
                ctrl_c_copies: true,
                copy_on_select: false,
                confirm: false,
                wheel_lines: WheelLines::Raw,
            }
        }

        fn area() -> Rect {
            Rect::from_min_size(Pos2::new(0.0, 100.0), egui::vec2(600.0, 400.0))
        }

        fn at(col: usize, row: usize) -> Pos2 {
            Pos2::new(col as f32 * CELL.0 + CELL.0 / 2.0, 100.0 + row as f32 * CELL.1 + 8.0)
        }

        fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                modifiers,
                focused: true,
                ..Default::default()
            };
            let ctx = self.ctx.clone();
            let font = FontId::monospace(14.0);
            let options = TerminalOptions {
                alt_is_meta: false,
                paste: RULES,
                confirm_multiline_paste: self.confirm,
                ctrl_c_copies: self.ctrl_c_copies,
                copy_on_select: self.copy_on_select,
                wheel_lines: self.wheel_lines,
                app_link_modifier: "cmd",
                focus: Focus::Manage,
            };
            let full = ctx.run_ui(raw, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    self.field = Some(ui.text_edit_singleline(&mut self.text));
                    self.button = Some(ui.button("Close"));
                    let typing = ui.ctx().text_edit_focused();
                    let out = TerminalView {
                        id_src: &[],
                        grid: GridGeom {
                            rect: Self::area(),
                            cw: CELL.0,
                            ch: CELL.1,
                            cols: COLS,
                            rows: ROWS,
                        },
                        font: &font,
                        bold_font: None,
                        style: GridStyle {
                            cursor: CursorStyle::Block,
                            dimmed: false,
                            link_active: false,
                            blink: false,
                            ligatures: false,
                            min_contrast: 1.0,
                            app_mouse: false,
                        },
                        search_marks: &[],
                        options,
                        gate: Gate::all(!typing),
                        injected_wheel: &[],
                    }
                    .show(ui, &mut self.term);
                    self.out = Some(out);
                });
            });
            for command in full.platform_output.commands {
                if let OutputCommand::CopyText(text) = command {
                    self.copied_to_clipboard.push(text);
                }
            }
        }

        fn out(&self) -> &TerminalOutput {
            self.out.as_ref().unwrap()
        }

        fn grid(&self) -> Id {
            self.out().response.id
        }

        fn settle(&mut self) {
            for _ in 0..3 {
                self.frame(vec![], Modifiers::NONE);
            }
        }

        fn press(&mut self, key: Key, modifiers: Modifiers) {
            let event =
                Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
            self.frame(vec![event], modifiers);
        }

        fn hover(&mut self) {
            self.frame(vec![Event::PointerMoved(Self::area().center())], Modifiers::NONE);
            self.frame(vec![], Modifiers::NONE);
        }

        fn wheel(&mut self, unit: MouseWheelUnit, lines: f32, modifiers: Modifiers) {
            let event = Event::MouseWheel {
                unit,
                delta: egui::vec2(0.0, lines),
                phase: TouchPhase::Move,
                modifiers,
            };
            self.frame(vec![event], modifiers);
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(from)], Modifiers::NONE);
            self.frame(vec![button(from, true)], Modifiers::NONE);
            let steps = ((to.x - from.x).abs() / 3.0).ceil().max(1.0) as usize;
            for step in 1..=steps {
                let pos = from + (to - from) * (step as f32 / steps as f32);
                self.frame(vec![Event::PointerMoved(pos)], Modifiers::NONE);
            }
            self.frame(vec![button(to, false)], Modifiers::NONE);
            self.frame(vec![], Modifiers::NONE);
        }

        fn wait_for(&self, what: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !what(self) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(what(self), "the terminal never got there");
        }

        fn ready(&self) -> bool {
            self.scratch.0.join("ready").exists()
        }

        fn received(&self, len: usize) -> Vec<u8> {
            let out = self.scratch.0.join("got");
            self.wait_for(|_| std::fs::read(&out).is_ok_and(|got| got.len() >= len));
            std::fs::read(&out).unwrap()
        }

        fn row_text(&self, row: usize) -> String {
            let snap = self.term.grid_snapshot();
            snap.cells[row * snap.cols..(row + 1) * snap.cols].iter().map(|c| c.c).collect()
        }

        fn focused(&self) -> Option<Id> {
            self.ctx.memory(egui::Memory::focused)
        }
    }

    fn with_history() -> Rig {
        let mut rig = Rig::new("seq 1 500; sleep 20");
        rig.wait_for(|rig| rig.term.scroll_state().1 > 400);
        rig.hover();
        rig
    }

    fn raw_app(setup: &str, len: usize) -> Rig {
        let script = format!("{setup}; stty raw -echo; : > READY; head -c {len} > OUT; sleep 20");
        let mut rig = Rig::new(&script);
        rig.wait_for(Rig::ready);
        rig.hover();
        rig
    }

    fn with_text() -> Rig {
        let mut rig = Rig::new("printf 'hello world\\n'; sleep 20");
        rig.wait_for(|rig| rig.row_text(0).starts_with("hello world"));
        rig.hover();
        rig
    }

    fn select_hello_world(rig: &Rig) {
        rig.term.start_selection(rig.term.grid_snapshot().top_line, 0, false);
        rig.term.update_selection(rig.term.grid_snapshot().top_line, 10, true);
        assert_eq!(rig.term.selection_text().as_deref(), Some("hello world"));
    }

    fn copy_chord() -> (Event, Modifiers) {
        let mods = if cfg!(target_os = "macos") {
            Modifiers::COMMAND
        } else {
            Modifiers::CTRL | Modifiers::SHIFT
        };
        (Event::Copy, mods)
    }

    fn ctrl_c() -> (Event, Modifiers) {
        if cfg!(target_os = "macos") {
            let event = Event::Key {
                key: Key::C,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::CTRL,
            };
            (event, Modifiers::CTRL)
        } else {
            (Event::Copy, Modifiers::CTRL)
        }
    }

    #[test]
    fn tab_in_the_terminal_reaches_the_shell_and_leaves_focus_on_the_grid() {
        let mut rig = with_history();
        rig.settle();
        assert_eq!(rig.focused(), Some(rig.grid()));
        rig.press(Key::Tab, Modifiers::NONE);
        assert_eq!(rig.out().typed, b"\t");
        assert_eq!(rig.focused(), Some(rig.grid()));
        rig.settle();
        assert_eq!(rig.focused(), Some(rig.grid()));
        assert!(!rig.button.as_ref().unwrap().has_focus());
    }

    #[test]
    fn shift_tab_in_the_terminal_is_back_tab_and_leaves_focus_on_the_grid() {
        let mut rig = with_history();
        rig.settle();
        rig.press(Key::Tab, Modifiers::SHIFT);
        assert_eq!(rig.out().typed, b"\x1b[Z");
        rig.settle();
        assert_eq!(rig.focused(), Some(rig.grid()));
    }

    #[test]
    fn arrows_and_esc_in_the_terminal_do_not_move_or_drop_focus() {
        let mut rig = with_history();
        rig.settle();
        let cases = [
            (Key::ArrowUp, &b"\x1b[A"[..]),
            (Key::ArrowDown, b"\x1b[B"),
            (Key::ArrowRight, b"\x1b[C"),
            (Key::ArrowLeft, b"\x1b[D"),
            (Key::Escape, b"\x1b"),
        ];
        for (key, bytes) in cases {
            rig.press(key, Modifiers::NONE);
            assert_eq!(rig.out().typed, bytes, "{key:?}");
            assert_eq!(rig.focused(), Some(rig.grid()), "{key:?}");
        }
    }

    #[test]
    fn tab_in_a_text_field_still_moves_focus_and_the_grid_does_not_take_it_back() {
        let mut rig = with_history();
        rig.settle();
        rig.field.as_ref().unwrap().request_focus();
        rig.settle();
        assert_eq!(rig.focused(), rig.field.as_ref().map(|field| field.id));
        rig.press(Key::Tab, Modifiers::NONE);
        rig.settle();
        assert_eq!(rig.focused(), rig.button.as_ref().map(|button| button.id));
        assert_eq!(rig.out().typed, b"");
    }

    #[test]
    fn the_bytes_of_tab_and_shift_tab_arrive_at_a_program_on_the_pty() {
        let mut rig = raw_app("true", 4);
        rig.settle();
        rig.press(Key::Tab, Modifiers::NONE);
        rig.press(Key::Tab, Modifiers::SHIFT);
        assert_eq!(rig.received(4), b"\t\x1b[Z");
    }

    #[test]
    fn the_wheel_scrolls_the_scrollback_of_a_shell_and_the_grid_keeps_the_keyboard() {
        let mut rig = with_history();
        assert_eq!(rig.term.scroll_state().0, 0);
        rig.wheel(MouseWheelUnit::Line, 3.0, Modifiers::NONE);
        assert_eq!(rig.term.scroll_state().0, 3);
        rig.wheel(MouseWheelUnit::Line, -1.0, Modifiers::NONE);
        assert_eq!(rig.term.scroll_state().0, 2);
        assert_eq!(rig.focused(), Some(rig.grid()));
        assert_eq!(rig.out().typed, b"");
    }

    #[test]
    fn a_trackpad_swipe_in_points_adds_up_across_frames() {
        let mut rig = with_history();
        for _ in 0..4 {
            rig.wheel(MouseWheelUnit::Point, CELL.1 / 2.0, Modifiers::NONE);
        }
        assert_eq!(rig.term.scroll_state().0, 2);
        rig.wheel(MouseWheelUnit::Page, 1.0, Modifiers::NONE);
        assert_eq!(rig.term.scroll_state().0, 27);
    }

    #[test]
    fn egui_smoothed_lines_scroll_at_least_a_line_while_the_wheel_moves() {
        let mut rig = with_history();
        rig.wheel_lines = WheelLines::Smoothed;
        rig.wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE);
        assert!(rig.term.scroll_state().0 >= 1);
    }

    #[test]
    fn the_wheel_does_nothing_while_the_pointer_is_elsewhere() {
        let mut rig = with_history();
        rig.frame(vec![Event::PointerMoved(Pos2::new(700.0, 20.0))], Modifiers::NONE);
        rig.frame(vec![], Modifiers::NONE);
        rig.wheel(MouseWheelUnit::Line, 3.0, Modifiers::NONE);
        assert_eq!(rig.term.scroll_state().0, 0);
    }

    #[test]
    fn an_alternate_screen_app_gets_arrow_keys() {
        let mut rig = raw_app("printf '\\033[?1049h'", 9);
        rig.wait_for(|rig| rig.term.is_alt_screen());
        rig.wheel(MouseWheelUnit::Line, 2.0, Modifiers::NONE);
        rig.wheel(MouseWheelUnit::Line, -1.0, Modifiers::NONE);
        assert_eq!(rig.received(9), b"\x1b[A\x1b[A\x1b[B");
        assert_eq!(rig.term.scroll_state().0, 0);
    }

    #[test]
    fn the_arrows_of_an_alternate_screen_app_follow_its_cursor_key_mode() {
        let mut rig = raw_app("printf '\\033[?1049h\\033[?1h'", 6);
        rig.wait_for(|rig| rig.term.app_cursor());
        rig.wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE);
        rig.wheel(MouseWheelUnit::Line, -1.0, Modifiers::NONE);
        assert_eq!(rig.received(6), b"\x1bOA\x1bOB");
    }

    #[test]
    fn alt_and_the_wheel_send_arrows_even_on_the_normal_screen() {
        let mut rig = raw_app("true", 9);
        rig.wheel(MouseWheelUnit::Line, 2.0, Modifiers::ALT);
        rig.wheel(MouseWheelUnit::Line, -1.0, Modifiers::ALT);
        assert_eq!(rig.received(9), b"\x1b[A\x1b[A\x1b[B");
        assert_eq!(rig.term.scroll_state().0, 0);
    }

    #[test]
    fn an_app_that_turned_on_mouse_reporting_gets_sgr_wheel_events_at_the_pointer() {
        let mut rig = raw_app("printf '\\033[?1049h\\033[?1000h\\033[?1006h'", 24);
        rig.wait_for(|rig| rig.term.mouse_reporting().sgr);
        rig.wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE);
        rig.wheel(MouseWheelUnit::Line, -1.0, Modifiers::NONE);
        assert_eq!(rig.received(24), b"\x1b[<64;38;13M\x1b[<65;38;13M");
    }

    #[test]
    fn mouse_reporting_in_the_normal_screen_still_gets_the_wheel() {
        let mut rig = raw_app("printf '\\033[?1002h\\033[?1006h'", 12);
        rig.wait_for(|rig| rig.term.mouse_reporting().sgr);
        rig.wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE);
        assert_eq!(rig.received(12), b"\x1b[<64;38;13M");
    }

    #[test]
    fn a_click_reaches_an_app_with_mouse_reporting_and_selects_nothing() {
        let mut rig = raw_app("printf '\\033[?1000h\\033[?1006h'", 18);
        rig.wait_for(|rig| rig.term.mouse_reporting().sgr);
        rig.drag(Rig::at(3, 2), Rig::at(3, 2));
        assert_eq!(rig.received(18), b"\x1b[<0;4;3M\x1b[<0;4;3m");
        assert!(rig.out().app_owns_pointer);
        assert_eq!(rig.term.selection_text(), None);
    }

    #[test]
    fn a_drag_reaches_an_app_that_asked_for_motion_while_a_button_is_down() {
        let mut rig = raw_app("printf '\\033[?1002h\\033[?1006h'", 38);
        rig.wait_for(|rig| rig.term.mouse_reporting().sgr);
        rig.drag(Rig::at(3, 2), Rig::at(5, 2));
        assert_eq!(rig.received(38), b"\x1b[<0;4;3M\x1b[<32;5;3M\x1b[<32;6;3M\x1b[<0;6;3m");
    }

    #[test]
    fn shift_takes_the_pointer_back_from_the_app() {
        let mut rig = raw_app("printf '\\033[?1000h\\033[?1006h'", 1);
        rig.wait_for(|rig| rig.term.mouse_reporting().sgr);
        rig.frame(vec![Event::PointerMoved(Rig::at(3, 2))], Modifiers::SHIFT);
        assert!(!rig.out().app_owns_pointer);
    }

    #[test]
    fn a_selection_is_copied_by_the_copy_chord() {
        let mut rig = with_text();
        select_hello_world(&rig);
        let (event, mods) = copy_chord();
        rig.frame(vec![event], mods);
        assert_eq!(rig.out().copied.as_deref(), Some("hello world"));
        assert_eq!(rig.copied_to_clipboard, ["hello world"]);
        assert_eq!(rig.out().typed, b"");
        assert_eq!(rig.term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn the_copy_chord_without_a_selection_copies_nothing() {
        let mut rig = with_text();
        let (event, mods) = copy_chord();
        rig.frame(vec![event], mods);
        assert_eq!(rig.out().copied, None);
        assert_eq!(rig.copied_to_clipboard.len(), 0);
    }

    #[test]
    fn ctrl_c_copies_and_clears_a_selection_and_is_ctrl_c_without_one() {
        let mut rig = with_text();
        let (event, mods) = ctrl_c();
        rig.frame(vec![event], mods);
        assert_eq!(rig.out().typed, b"\x03");
        assert_eq!(rig.out().copied, None);

        let mut rig = with_text();
        select_hello_world(&rig);
        let (event, mods) = ctrl_c();
        rig.frame(vec![event], mods);
        assert_eq!(rig.out().typed, b"");
        assert_eq!(rig.out().copied.as_deref(), Some("hello world"));
        assert_eq!(rig.term.selection_text(), None);
    }

    #[test]
    fn ctrl_c_stays_ctrl_c_with_a_selection_when_it_may_not_copy() {
        let mut rig = with_text();
        rig.ctrl_c_copies = false;
        select_hello_world(&rig);
        let (event, mods) = ctrl_c();
        rig.frame(vec![event], mods);
        assert_eq!(rig.out().typed, b"\x03");
        assert_eq!(rig.out().copied, None);
        assert_eq!(rig.copied_to_clipboard.len(), 0);
        assert_eq!(rig.term.selection_text(), None);
    }

    #[test]
    fn a_finished_drag_is_copied_when_copy_on_select_is_on() {
        let mut rig = with_text();
        rig.copy_on_select = true;
        rig.drag(Rig::at(0, 0), Rig::at(10, 0));
        assert_eq!(rig.copied_to_clipboard.len(), 1);
        assert!(rig.copied_to_clipboard[0].contains("llo wor"), "{:?}", rig.copied_to_clipboard);
    }

    #[test]
    fn typing_drops_the_selection_and_jumps_to_the_bottom() {
        let mut rig = with_history();
        rig.term.start_selection(rig.term.grid_snapshot().top_line, 0, false);
        rig.term.update_selection(rig.term.grid_snapshot().top_line, 3, true);
        assert!(rig.term.selection_text().is_some());
        rig.term.scroll(40);
        rig.frame(vec![Event::Text("a".into())], Modifiers::NONE);
        assert_eq!(rig.out().typed, b"a");
        assert_eq!(rig.term.scroll_state().0, 0);
        assert_eq!(rig.term.selection_text(), None);
    }

    #[test]
    fn a_paste_is_cleaned_and_bracketed_for_a_program_that_armed_it() {
        let text = "a\r\nb\x1b[201~";
        let want = "\x1b[200~a\rb[201~\x1b[201~";
        let mut rig = raw_app("printf '\\033[?2004h'", want.len());
        std::thread::sleep(Duration::from_millis(200));
        rig.frame(vec![Event::Paste(text.into())], Modifiers::NONE);
        assert_eq!(rig.out().pasted, ["a\rb[201~"]);
        assert_eq!(String::from_utf8_lossy(&rig.received(want.len())), want);
    }

    #[test]
    fn a_paste_is_bare_for_a_program_that_did_not_arm_bracketing() {
        let mut rig = raw_app("true", 9);
        std::thread::sleep(Duration::from_millis(200));
        rig.frame(vec![Event::Paste("  one\ntwo \n".into())], Modifiers::NONE);
        assert_eq!(rig.out().pasted, ["  one\rtwo"]);
        assert_eq!(rig.received(9), b"  one\rtwo");
    }

    #[test]
    fn a_multiline_paste_is_held_back_for_the_app_when_it_asks_to_confirm() {
        let mut rig = with_history();
        rig.confirm = true;
        rig.frame(vec![Event::Paste("a\nb".into())], Modifiers::NONE);
        assert_eq!(rig.out().confirm_paste, ["a\rb"]);
        assert_eq!(rig.out().pasted.len(), 0);
        rig.frame(vec![Event::Paste("one line".into())], Modifiers::NONE);
        assert_eq!(rig.out().confirm_paste.len(), 0);
        assert_eq!(rig.out().pasted, ["one line"]);
    }

    #[test]
    fn a_paste_is_ignored_and_the_keys_stay_free_while_a_text_field_has_the_keyboard() {
        let mut rig = with_history();
        rig.settle();
        rig.field.as_ref().unwrap().request_focus();
        rig.settle();
        rig.frame(vec![Event::Paste("x".into())], Modifiers::NONE);
        assert_eq!(rig.out().pasted.len(), 0);
        rig.frame(vec![Event::Text("y".into())], Modifiers::NONE);
        assert_eq!(rig.out().typed, b"");
    }

    #[test]
    fn the_grid_lets_go_of_the_keyboard_when_the_keys_are_not_free() {
        let ctx = Context::default();
        let grid = Id::new("grid");
        let run = |free: bool| {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.interact(
                        Rect::from_min_size(Pos2::ZERO, egui::vec2(50.0, 50.0)),
                        grid,
                        egui::Sense::click_and_drag(),
                    );
                    focus::hold(ui.ctx(), grid, free);
                });
            });
        };
        run(true);
        run(true);
        assert_eq!(ctx.memory(egui::Memory::focused), Some(grid));
        run(false);
        assert_eq!(ctx.memory(egui::Memory::focused), None);
    }

    #[test]
    fn a_gate_and_a_copy_request_table_agree_with_the_platform() {
        let options = TerminalOptions {
            alt_is_meta: false,
            paste: RULES,
            confirm_multiline_paste: false,
            ctrl_c_copies: true,
            copy_on_select: false,
            wheel_lines: WheelLines::Raw,
            app_link_modifier: "cmd",
            focus: Focus::Off,
        };
        let plain = TerminalOptions { ctrl_c_copies: false, ..options };
        let copy = [Event::Copy];
        let ctrl = Modifiers::CTRL;
        let on = Gate::all(true);
        let check = |events: &[Event], mods, key, opts: &TerminalOptions, gate, selection, want| {
            let got = copy_request(events, mods, key, opts, gate, selection, false);
            assert_eq!((got.wanted, got.intercept_ctrl_c), want, "{events:?} {mods:?} {key}");
        };
        check(&copy, Modifiers::COMMAND, false, &options, on, false, (true, false));
        check(&copy, ctrl, false, &options, on, false, (true, false));
        check(&copy, ctrl, false, &plain, on, false, (false, false));
        check(&[], ctrl, true, &options, on, true, (true, true));
        check(&[], ctrl, true, &options, on, false, (false, false));
        check(&copy, Modifiers::COMMAND, false, &options, Gate::all(false), false, (false, false));
    }
}
