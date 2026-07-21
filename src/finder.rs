//! Finder overlays: the Cmd+F scrollback-search bar and its modal sibling, the
//! multiline paste-confirmation window.

use eframe::egui;

use crate::ui::icons;
use crate::widgets::{icon_button, icon_toggle};
use crate::{Stdusk, colors, search};

/// Scrollback-search overlay state (Cmd+F). Matches are found over the buffer; the "current"
/// one is highlighted via the terminal selection and scrolled into view.
pub(crate) struct Search {
    pub(crate) query: String,
    pub(crate) matches: Vec<search::Match>,
    pub(crate) current: usize,
    pub(crate) focus: bool, // request text-field focus on the next frame (set on open / after Enter)
    pub(crate) opts: search::SearchOpts, // case / regex / whole-word toggles
    /// Whether the query field had egui focus last frame. While true the find bar owns the
    /// keyboard; once the user clicks back into the terminal the bar stays open but typing
    /// flows to the shell again (an open bar must never silently swallow terminal input).
    pub(crate) field_focused: bool,
}

impl Search {
    pub(crate) fn new() -> Self {
        Self {
            query: String::new(),
            matches: Vec::new(),
            current: 0,
            focus: true,
            opts: search::SearchOpts::default(),
            field_focused: true, // owns the keyboard from the frame it opens
        }
    }
}

impl Stdusk {
    /// Multiline-paste confirmation (Tabby's warnOnMultilinePaste): preview + Paste/Cancel.
    /// Shown while `pending_pastes` is non-empty (front first); the modal path intentionally
    /// skips the trim rules. Targets the tab the paste happened in, by stable id.
    pub(crate) fn paste_confirm_window(&mut self, ctx: &egui::Context) {
        let Some((tab_id, text)) = self.pending_pastes.pop_front() else {
            return;
        };
        let mut do_paste = false;
        let mut cancel = false;
        let lines = text.split('\r').count();
        egui::Window::new("Paste multiple lines?")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(crate::widgets::overlay_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("Paste {lines} lines?"))
                        .strong()
                        .color(colors::fg()),
                );
                ui.add_space(4.0);
                // Preview the first few lines (Tabby shows a 1000-char preview).
                let preview: String = text
                    .split('\r')
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .chars()
                    .take(200)
                    .collect();
                ui.label(egui::RichText::new(preview).monospace().small().color(colors::dim()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::widgets::action_button(ui, "Paste", true).clicked() {
                        do_paste = true;
                    }
                    if crate::widgets::action_button(ui, "Cancel", false).clicked() {
                        cancel = true;
                    }
                });
            });
        // Keyboard confirm - unless the rename modal is also open (rename owns Enter/Esc then).
        if self.renaming.is_none() {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_paste = true;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        }
        if do_paste {
            // Paste into the ORIGINATING tab (by id); if it was closed, drop the paste.
            // Broadcast mode fans the confirmed paste out to every pane, like live input.
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                let targets = if tab.broadcast {
                    tab.root_mut().leaves_mut()
                } else {
                    vec![tab.focused_term_mut()]
                };
                for t in targets {
                    t.paste(&text);
                    t.clear_selection();
                    t.scroll_to_bottom();
                }
            }
        } else if !cancel {
            self.pending_pastes.push_front((tab_id, text)); // keep asking next frame
        }
    }

    /// Docked scrollback-search bar (Cmd+F): a top panel under the tab bar. Enter/Shift+Enter
    /// (or the buttons) cycle matches, Esc/Done closes. Current match highlighted via selection.
    pub(crate) fn find_panel(&mut self, ui: &mut egui::Ui) {
        let Some(mut st) = self.search.take() else {
            return;
        };
        let was_focused = st.field_focused;
        let mut close = false;
        let mut recompute = false;
        let mut step: i32 = 0;
        // Red input + count once a non-empty query matches nothing (reflects last recompute).
        let no_results = !st.query.is_empty() && st.matches.is_empty();
        egui::Panel::top("findbar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                // A rounded pill pushed to the right edge - floats like Tabby's find bar.
                const PILL_W: f32 = 620.0;
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - PILL_W).max(0.0));
                    crate::widgets::overlay_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 7.0;
                            ui.visuals_mut().extreme_bg_color = colors::bg(); // field bg = theme
                            // One uniform font for the whole pill so the magnifier, hint text,
                            // typed text and count all share a baseline + size.
                            ui.style_mut().override_font_id =
                                Some(egui::FontId::proportional(15.0));
                            // Magnifier painted centered in its box (Phosphor ink sits high in
                            // the line box, so a plain label would float above the field).
                            let (mrect, _) = ui
                                .allocate_exact_size(egui::vec2(20.0, 28.0), egui::Sense::hover());
                            ui.painter().text(
                                mrect.center(),
                                egui::Align2::CENTER_CENTER,
                                icons::MAGNIFYING_GLASS,
                                egui::FontId::proportional(16.0),
                                colors::dim(),
                            );
                            let accent = if no_results { colors::red() } else { colors::fg() };
                            let r = crate::widgets::text_field(
                                ui,
                                &mut st.query,
                                "Find",
                                300.0,
                                accent,
                            );
                            if st.focus {
                                r.request_focus();
                                st.focus = false;
                                st.field_focused = true; // focus lands next frame; capture now
                            } else {
                                st.field_focused = r.has_focus();
                            }
                            if r.changed() {
                                recompute = true;
                            }
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                                st.focus = true; // keep focus for repeated Enter
                            }
                            let count = if st.matches.is_empty() { 0 } else { st.current + 1 };
                            let count_color =
                                if no_results { colors::red() } else { colors::dim() };
                            ui.label(
                                egui::RichText::new(format!("{count}/{}", st.matches.len()))
                                    .color(count_color)
                                    .monospace(),
                            );
                            // Case / regex / whole-word toggles (Tabby parity) - each flips
                            // its option and re-runs the search.
                            if icon_toggle(
                                ui,
                                icons::TEXT_AA,
                                st.opts.case_sensitive,
                                "Case sensitive",
                            )
                            .clicked()
                            {
                                st.opts.case_sensitive = !st.opts.case_sensitive;
                                recompute = true;
                            }
                            if icon_toggle(ui, icons::ASTERISK, st.opts.regex, "Regular expression")
                                .clicked()
                            {
                                st.opts.regex = !st.opts.regex;
                                recompute = true;
                            }
                            if icon_toggle(
                                ui,
                                icons::BRACKETS_SQUARE,
                                st.opts.whole_word,
                                "Whole word",
                            )
                            .clicked()
                            {
                                st.opts.whole_word = !st.opts.whole_word;
                                recompute = true;
                            }
                            if icon_button(ui, icons::CARET_UP, "Previous (Shift+Enter)").clicked()
                            {
                                step = -1;
                            }
                            if icon_button(ui, icons::CARET_DOWN, "Next (Enter)").clicked() {
                                step = 1;
                            }
                            if icon_button(ui, icons::X, "Close (Esc)").clicked() {
                                close = true;
                            }
                        });
                    });
                });
            });
        // Esc closes the bar only while its field owned the keyboard (sampled BEFORE this
        // frame's draw - egui clears widget focus on the same Esc press). With the terminal
        // focused, Esc belongs to the shell; Cmd+F still toggles the bar away.
        if was_focused && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        let term = self.tabs[self.active].focused_term();
        if recompute {
            st.matches = search::find_matches(&term.buffer_lines(), &st.query, st.opts);
            st.current = 0;
        } else if step != 0 && !st.matches.is_empty() {
            let len = st.matches.len() as i32;
            st.current = (st.current as i32 + step).rem_euclid(len) as usize;
        }
        if let Some(m) = st.matches.get(st.current).copied() {
            term.highlight_match(m);
            term.scroll_to_line(m.line);
        } else {
            term.clear_selection();
        }
        // "No results" toast, only when a query change just produced zero matches.
        let empty_now = recompute && !st.query.is_empty() && st.matches.is_empty();
        if empty_now {
            let now = ui.input(|i| i.time);
            self.toast = Some(("No results".into(), now + 1.4));
        }
        if close {
            self.tabs[self.active].focused_term().clear_selection();
        } else {
            self.search = Some(st);
        }
    }
}
