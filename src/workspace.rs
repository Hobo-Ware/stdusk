//! Workspace: the central panel that tiles the active tab's pane tree (render,
//! input/paste routing, splitters), the pane right-click menu, and the deferred
//! pane-action apply.

use eframe::egui;

use crate::tabs::spawn_opts;
use crate::terminal::PtyTerm;
use crate::ui::{self, collect_input, render_grid, style_menu};
use crate::{COLS, ROWS, Stdusk, colors, pane};

/// The raw byte a Ctrl+V keypress sends (matches `ui::ctrl_letter(Key::V)`). Forwarding it makes
/// an app that reads the system clipboard on ^V - Claude Code's image ingestion - pick up a
/// pasted image without us encoding anything.
const CTRL_V: u8 = 0x16;

/// A resolved mouse-paste payload. Text pastes normally; `Image` means the clipboard held an
/// image and no usable text, so we forward `CTRL_V` for app-native image ingestion.
///
/// NOTE: Cmd+V can NOT reach the image path. egui-winit 0.35 (`lib.rs` ~1007-1015) folds Cmd+V
/// into `Event::Paste` reading clipboard TEXT only, pushes no event when that text is empty, and
/// always swallows the key event - so an image-only clipboard on Cmd+V yields no egui event at
/// all. Ctrl+V already works (it forwards `CTRL_V` via `key_to_bytes`); these mouse paths add a
/// pointer route with the same behavior.
#[derive(Debug, PartialEq)]
enum ClipboardPaste {
    Text(String),
    Image,
}

/// Pure paste-target decision: prefer text, fall back to an image only when there's no usable
/// text (a screenshot copy carries none). Split from clipboard IO so it's unit-testable.
fn resolve_clipboard_paste(text: Option<String>, has_image: bool) -> Option<ClipboardPaste> {
    match text {
        Some(t) => Some(ClipboardPaste::Text(t)),
        None if has_image => Some(ClipboardPaste::Image),
        None => None,
    }
}

/// Read the system clipboard for a mouse paste. Probes the image only when there's no text (the
/// image decode is wasted work otherwise).
fn read_clipboard_paste() -> Option<ClipboardPaste> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let text = cb.get_text().ok().filter(|t| !t.is_empty());
    let has_image = text.is_none() && cb.get_image().is_ok();
    resolve_clipboard_paste(text, has_image)
}

/// Deferred pane action from the right-click menu, applied after the central panel. Each
/// carries the target pane's path.
enum PaneAction {
    Copy(Vec<pane::Side>),
    CopyPath(Vec<pane::Side>),
    Split(Vec<pane::Side>, pane::SplitDir, bool), // (path, dir, new_first)
    Close(Vec<pane::Side>),
    NewTab,
}

/// Right-click menu for a terminal pane. Sets `action`; egui auto-closes on a button click.
fn pane_menu(
    ui: &mut egui::Ui,
    path: &[pane::Side],
    has_selection: bool,
    cwd: Option<&str>,
    action: &mut Option<PaneAction>,
) {
    style_menu(ui);
    ui.add_enabled_ui(has_selection, |ui| {
        if ui::menu_item(ui, "Copy", "Cmd+C").clicked() {
            *action = Some(PaneAction::Copy(path.to_vec()));
        }
    });
    ui.add_enabled_ui(cwd.is_some(), |ui| {
        if ui::menu_item(ui, "Copy current path", "").clicked() {
            *action = Some(PaneAction::CopyPath(path.to_vec()));
        }
    });
    ui.separator();
    ui.menu_button("Split", |ui| {
        use pane::SplitDir::{Column, Row};
        style_menu(ui);
        if ui::menu_item(ui, "Right", "Cmd+D").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, false));
        }
        if ui::menu_item(ui, "Down", "Cmd+Shift+D").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, false));
        }
        if ui::menu_item(ui, "Left", "").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, true));
        }
        if ui::menu_item(ui, "Up", "").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, true));
        }
    });
    ui.separator();
    if ui::menu_item(ui, "New tab", "Cmd+T").clicked() {
        *action = Some(PaneAction::NewTab);
    }
    if ui::menu_item(ui, "Close pane", "Cmd+W").clicked() {
        *action = Some(PaneAction::Close(path.to_vec()));
    }
}

/// Flags the central panel reports back for the caller's bell-flash / toast handling.
pub(crate) struct CentralOut {
    pub(crate) copied: bool,
    pub(crate) bell_rang: bool,
}

impl Stdusk {
    /// The terminal workspace: tiles the active tab's pane tree, routes keystrokes and
    /// pastes to the focused pane, draws splitters, and applies the deferred pane action.
    pub(crate) fn central_panel(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        input_captured: bool,
        kb_pane_dir: Option<pane::Dir>,
        now: f64,
    ) -> CentralOut {
        let bell_on = self.cfg.terminal.bell != "off";
        let mut copied = false; // set inside the central panel when Cmd+C copies a selection
        let mut bell_rang = false; // any pane rang BEL this frame -> visual flash
        let mut pane_action: Option<PaneAction> = None; // from the pane right-click menu
        // Any open text surface means the grid must not steal egui focus (see below); pty
        // input itself is gated on the tighter `input_captured`.
        let focus_guard = self.search.is_some()
            || self.renaming.is_some()
            || self.palette.is_some()
            || !self.pending_pastes.is_empty()
            || self.pending_close.is_some();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let area = ui.max_rect();
                let font = egui::FontId::monospace(self.cfg.appearance.font_size * self.zoom);
                // Real bold face for BOLD cells - only when build_fonts registered the family
                // (naming an absent family in a FontId panics). Metrics stay regular-derived.
                let bold_font = self.bold_font_ready.then(|| {
                    egui::FontId::new(
                        self.cfg.appearance.font_size * self.zoom,
                        egui::FontFamily::Name(crate::BOLD_FONT_FAMILY.into()),
                    )
                });
                let m = ui.painter().layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
                let (cw, ch) = (
                    m.size().x,
                    ui::padded_cell_height(m.size().y, self.cfg.appearance.line_padding),
                );
                let cursor = ui::cursor_style(&self.cfg.terminal.cursor);
                // Links are "active" (underline on hover, open on click) when enabled and the
                // configured modifier is held - default modifier "none" means plain hover, Tabby-style.
                let link_active = self.cfg.terminal.clickable_links
                    && ui::link_modifier_held(
                        ui.input(|i| i.modifiers),
                        &self.cfg.terminal.link_modifier,
                    );
                let tcfg = self.cfg.terminal.clone();
                // All find-bar matches, drawn as a dim overlay on the searched (focused) pane;
                // the current match keeps its brighter selection highlight on top.
                let search_marks: Vec<crate::search::Match> =
                    self.search.as_ref().map(|s| s.matches.clone()).unwrap_or_default();
                let tab = &mut self.tabs[self.active];

                // Cmd+C copies the focused pane's selection; intelligent Ctrl+C (Tabby) copies
                // too when a selection exists, else stays SIGINT (handled via collect_input).
                // Selection read ONCE per frame: the reader thread can invalidate it between a
                // has-selection check and a copy, which would swallow Ctrl-C without copying.
                let sel_text = tab.focused_term().selection_text();
                let has_selection = sel_text.is_some();
                let ctrl_c = ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::C));
                let want_copy = ui
                    .input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
                    || (ctrl_c && has_selection && !input_captured);
                if want_copy && let Some(txt) = sel_text {
                    ctx.copy_text(txt);
                    if ctrl_c {
                        tab.focused_term().clear_selection(); // Tabby clears after Ctrl-C copy
                    }
                    copied = true;
                }

                // Keystrokes -> focused pane (unless a text field/modal owns the keyboard);
                // broadcast mode (Tabby pane-focus-all) fans them out to EVERY pane instead.
                if !input_captured {
                    let input = collect_input(ui, tcfg.alt_is_meta, ctrl_c && has_selection);
                    if !input.is_empty() {
                        let targets = if tab.broadcast {
                            tab.root_mut().leaves_mut()
                        } else {
                            vec![tab.focused_term_mut()]
                        };
                        for t in targets {
                            t.send(&input);
                            t.clear_selection();
                            t.scroll_to_bottom();
                        }
                    }
                }
                // Paste events: processed even while the paste-confirm modal is open (they queue
                // rather than vanish); only a focused TEXT FIELD (find bar / rename / palette)
                // owns pastes. The find bar counts only while its field has focus - an open,
                // unfocused bar must not eat pastes aimed at the terminal.
                let paste_owned = self.search.as_ref().is_some_and(|s| s.field_focused)
                    || self.renaming.is_some()
                    || self.palette.is_some();
                if !paste_owned {
                    let pastes: Vec<String> = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::Paste(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect()
                    });
                    for p in pastes {
                        // Tabby's paste pipeline: normalize newlines (+optional ->spaces); a
                        // multiline paste outside the alt screen asks first (modal, untrimmed);
                        // otherwise apply the trim rules and send.
                        let s = ui::normalize_paste(&p, tcfg.replace_newlines_on_paste);
                        let multiline = s.contains('\r');
                        if multiline
                            && tcfg.warn_on_multiline_paste
                            && !tab.focused_term().is_alt_screen()
                        {
                            self.pending_pastes.push_back((tab.id, s));
                            ctx.request_repaint(); // make sure the confirm modal shows this frame
                            continue;
                        }
                        let s = ui::trim_paste(&s, tcfg.trim_whitespace_on_paste);
                        let targets = if tab.broadcast {
                            tab.root_mut().leaves_mut()
                        } else {
                            vec![tab.focused_term_mut()]
                        };
                        for t in targets {
                            t.paste(&s);
                            t.clear_selection();
                            t.scroll_to_bottom();
                        }
                    }
                }

                // Keyboard pane navigation (Cmd+Alt+arrow): move focus to the neighbor pane.
                let full_layout = tab.root().layout(area);
                if let Some(dir) = kb_pane_dir
                    && let Some(target) = pane::neighbor(&full_layout, &tab.focused, dir)
                {
                    tab.focused = target;
                }

                // Tile the pane tree; when a pane is maximized, show only the focused one full-area.
                let layout = if tab.maximized && full_layout.len() > 1 {
                    vec![(tab.focused.clone(), area)]
                } else {
                    full_layout
                };
                let multi = layout.len() > 1;
                let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
                let alt_wheel = ui.input(|i| i.modifiers.alt);
                let pointer = ui.input(|i| i.pointer.hover_pos());
                // Right-click mode (Tabby): raw press/release tracking, so a long hold (which
                // egui would not report as a click) still resolves to the context menu.
                let rc_mode = ui::right_click_mode(&tcfg.right_click);
                let (rc_pressed, rc_released) = ui.input(|i| {
                    (
                        i.pointer.button_pressed(egui::PointerButton::Secondary),
                        i.pointer.button_released(egui::PointerButton::Secondary),
                    )
                });
                let mut focus_click: Option<Vec<pane::Side>> = None;
                let mut mouse_paste: Option<(Vec<pane::Side>, ClipboardPaste)> = None; // middle/right click
                let mut restart_pane: Option<Vec<pane::Side>> = None;
                for (path, rect) in &layout {
                    {
                        let term = tab.root_mut().leaf_at_mut(path).expect("leaf");
                        let cols = (rect.width() / cw).floor().max(1.0) as usize;
                        let rows = (rect.height() / ch).floor().max(1.0) as usize;
                        term.resize(cols, rows);
                        // Wheel scroll goes to the pane under the pointer.
                        if scroll_y != 0.0
                            && let Some(p) = pointer.filter(|p| rect.contains(*p))
                        {
                            let mut lines = (scroll_y / ch).round() as i32;
                            if lines == 0 {
                                lines = scroll_y.signum() as i32;
                            }
                            let mr = term.mouse_reporting();
                            if alt_wheel {
                                // Alt+wheel sends arrow keys instead of scrolling, one per line
                                // (Tabby's mousewheel handler - gated on Alt alone). An explicit
                                // user override, so it wins even over app mouse reporting.
                                term.send(&ui::alt_scroll_bytes(lines));
                            } else if mr.reports_buttons() && mr.sgr {
                                // The app asked for real mouse reporting: wheel -> SGR 1006 events
                                // at the pointer's cell, so its fullscreen TUI scrolls (Claude
                                // Code, git list pickers) instead of doing nothing.
                                let col = (((p.x - rect.left()) / cw) as usize)
                                    .min(cols.saturating_sub(1));
                                let row = (((p.y - rect.top()) / ch) as usize)
                                    .min(rows.saturating_sub(1));
                                // Clamp the report count: unlike local scroll (which alacritty
                                // caps to available history), the app has no backstop, so an
                                // accelerated frame of many lines would over-scroll its TUI.
                                let reports = crate::terminal::wheel_report_lines(lines);
                                term.send(&crate::terminal::wheel_sgr(reports, col, row));
                            } else if mr.alternate_scroll && term.is_alt_screen() {
                                // Alt-screen app without real mouse reporting (less/pager): wheel
                                // emits arrow keys (xterm alternateScroll), since the alt grid has
                                // no scrollback to move.
                                term.send(&ui::alt_scroll_bytes(lines));
                            } else {
                                term.scroll(lines);
                            }
                        }
                    }
                    let term = tab.root().leaf_at(path).expect("leaf");
                    let snap = term.grid_snapshot();
                    // Broadcast mode reads every pane as focused (Tabby _allFocusMode drops the
                    // unfocused fade on all panes), so no pane looks like it won't get the keys.
                    let dimmed = multi && path != &tab.focused && !tab.broadcast;
                    let has_sel = term.selection_text().is_some();
                    let cwd = term.cwd();
                    let blink = tcfg.cursor_blink && !input_captured && path == &tab.focused;
                    let resp = render_grid(
                        ui,
                        path,
                        *rect,
                        term,
                        &snap,
                        cw,
                        ch,
                        &font,
                        bold_font.as_ref(),
                        ui::GridStyle {
                            cursor,
                            dimmed,
                            link_active,
                            blink,
                            ligatures: tcfg.ligatures,
                            min_contrast: tcfg.minimum_contrast,
                        },
                        // The find bar searches the focused pane only.
                        if path == &tab.focused { &search_marks } else { &[] },
                    );
                    // Broadcast mode: an accent border on EVERY pane makes the keys-go-everywhere
                    // state unmistakable (Tabby paints an app-wide red border + a banner).
                    if tab.broadcast {
                        ui.painter().rect_stroke(
                            *rect,
                            4.0,
                            egui::Stroke::new(1.5, colors::accent()),
                            egui::StrokeKind::Inside,
                        );
                    }
                    // A selection drag held past the pane's top/bottom edge auto-scrolls the
                    // viewport, so the selection extends beyond what was visible when the drag
                    // began (standard terminal behavior). render_grid re-extends the selection to
                    // the clamped edge cell every frame; nudging the offset here feeds it fresh
                    // lines, and the repaint keeps frames coming while held. Skipped when the app
                    // owns the mouse (its own drag reporting handles scrolling).
                    if resp.dragged()
                        && !term.mouse_reporting().reports_buttons()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let lines = crate::terminal::drag_autoscroll_lines(
                            p.y,
                            rect.top(),
                            rect.bottom(),
                            ch,
                        );
                        if lines != 0 {
                            term.scroll(lines);
                            ctx.request_repaint();
                        }
                    }
                    if bell_on && term.take_bell() {
                        bell_rang = true;
                    }
                    // Copy-on-select: whenever a drag/double/triple selection just finished.
                    if tcfg.copy_on_select
                        && (resp.drag_stopped() || resp.double_clicked() || resp.triple_clicked())
                        && let Some(txt) = term.selection_text()
                        && !txt.trim().is_empty()
                    {
                        ctx.copy_text(txt);
                        copied = true; // "Copied" toast
                    }
                    // Middle-click pastes the clipboard into this pane (and focuses it).
                    if tcfg.paste_on_middle_click
                        && !input_captured
                        && resp.hovered()
                        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle))
                        && let Some(clip) = read_clipboard_paste()
                    {
                        mouse_paste = Some((path.clone(), clip));
                    }
                    // Right-click (Tabby baseTerminalTab handleRightMouseDown/Up): "menu" pops
                    // the context menu; "paste"/"clipboard" act on a quick tap (<250ms) and
                    // fall back to the menu on a hold. Decision table: ui::right_click_action.
                    let hovered_now = pointer.is_some_and(|p| rect.contains(p));
                    if rc_pressed && hovered_now {
                        self.right_press = Some((path.clone(), now));
                    }
                    let mut open_menu = false;
                    if rc_released
                        && hovered_now
                        && let Some((_, t0)) = self.right_press.take_if(|(p, _)| *p == *path)
                    {
                        match ui::right_click_action(rc_mode, now - t0, has_sel) {
                            ui::RightClickAction::Menu => open_menu = true,
                            ui::RightClickAction::Copy => {
                                if let Some(txt) = term.selection_text() {
                                    ctx.copy_text(txt);
                                    term.clear_selection(); // Tabby clears after the copy
                                    copied = true;
                                }
                            }
                            ui::RightClickAction::Paste => {
                                if !input_captured && let Some(clip) = read_clipboard_paste() {
                                    mouse_paste = Some((path.clone(), clip));
                                }
                            }
                        }
                    }
                    // Focus follows mouse (Tabby splitTab attachTabView: mousemove focuses the
                    // pane; suppressed during spanner drags): pointer movement over an unfocused
                    // pane focuses it. No buttons down - a selection or splitter drag crossing
                    // panes must not steal focus mid-gesture.
                    if tcfg.focus_follows_mouse
                        && path != &tab.focused
                        && hovered_now
                        && ui.input(|i| {
                            i.pointer.delta() != egui::Vec2::ZERO && !i.pointer.any_down()
                        })
                    {
                        focus_click = Some(path.clone());
                    }
                    // Dead pane (on_exit = keep / restart's crash-loop fallback): dim overlay;
                    // Enter (while focused) or a click respawns the shell in the same cwd.
                    if let Some(exit) = term.exited() {
                        ui::draw_exit_overlay(ui, *rect, exit.code);
                        let enter = path == &tab.focused
                            && !input_captured
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if resp.clicked() || enter {
                            restart_pane = Some(path.clone());
                        }
                    }
                    if resp.clicked() || resp.drag_started() {
                        focus_click = Some(path.clone());
                    }
                    // Keep egui keyboard focus on the active terminal, or a typed Space/Enter
                    // would activate a focused tab-bar button (e.g. the gear opening config.toml).
                    // Gated on any text surface being OPEN (not just focused): the grid requests
                    // focus every frame and would instantly steal it back from a find field the
                    // user just clicked into.
                    if !focus_guard && path == &tab.focused {
                        resp.request_focus();
                    }
                    // The pane menu opens on OUR decision (`open_menu`), not egui's built-in
                    // secondary-click detection - paste/clipboard modes must suppress it on a
                    // quick tap and force it after a hold. A plain click closes it.
                    let menu_cmd = if open_menu {
                        Some(egui::SetOpenCommand::Bool(true))
                    } else if resp.clicked() {
                        Some(egui::SetOpenCommand::Bool(false))
                    } else {
                        None
                    };
                    egui::Popup::menu(&resp).open_memory(menu_cmd).at_pointer_fixed().show(|ui| {
                        pane_menu(ui, path, has_sel, cwd.as_deref(), &mut pane_action);
                    });
                }
                if rc_released {
                    self.right_press = None; // release landed off-pane: drop the stale press
                }
                if let Some(p) = focus_click {
                    tab.focused = p;
                }
                // Apply the middle/right-click paste (deferred: needs &mut past the render
                // borrow). Runs through the same normalize/trim pipeline; skips the multiline
                // modal like Tabby (both are immediate mouse actions - see `paste()` there).
                // Respawn a dead pane (deferred: needs &mut past the render borrow).
                if let Some(p) = restart_pane
                    && let Some(t) = tab.root_mut().leaf_at_mut(&p)
                {
                    crate::tabs::respawn_term(&self.cfg, ctx, t);
                }
                if let Some((p, clip)) = mouse_paste {
                    tab.focused.clone_from(&p);
                    if let Some(t) = tab.root_mut().leaf_at_mut(&p) {
                        let label = match clip {
                            ClipboardPaste::Text(text) => {
                                let s = ui::normalize_paste(&text, tcfg.replace_newlines_on_paste);
                                let s = ui::trim_paste(&s, tcfg.trim_whitespace_on_paste);
                                t.paste(&s);
                                "Pasted"
                            }
                            // Image-only clipboard: forward ^V so an app that reads the clipboard
                            // on Ctrl+V (Claude Code) ingests it. Cmd+V can't reach here (see the
                            // ClipboardPaste doc comment).
                            ClipboardPaste::Image => {
                                t.send(&[CTRL_V]);
                                "Pasted image"
                            }
                        };
                        t.scroll_to_bottom();
                        self.toast = Some((label.into(), now + 1.4));
                    }
                }

                // Draggable splitters between panes (hidden while a pane is maximized).
                for (spath, dir, handle, parent) in
                    if tab.maximized { Vec::new() } else { tab.root().splitters(area) }
                {
                    let resp = ui.interact(
                        handle,
                        egui::Id::new(("split", &spath)),
                        egui::Sense::click_and_drag(),
                    );
                    let hot = resp.hovered() || resp.dragged();
                    ui.painter().rect_filled(
                        handle,
                        1.0,
                        if hot { colors::accent() } else { colors::border() },
                    );
                    if hot {
                        ui.ctx().set_cursor_icon(match dir {
                            pane::SplitDir::Row => egui::CursorIcon::ResizeHorizontal,
                            pane::SplitDir::Column => egui::CursorIcon::ResizeVertical,
                        });
                    }
                    if resp.dragged()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        tab.root_mut().set_ratio(&spath, pane::ratio_from_pointer(parent, dir, p));
                    }
                }
            });

        // Apply the deferred pane action (menu), now that the panel borrow is released.
        match pane_action {
            Some(PaneAction::Copy(p)) => {
                if let Some(txt) =
                    self.tabs[self.active].root().leaf_at(&p).and_then(PtyTerm::selection_text)
                {
                    ctx.copy_text(txt);
                    copied = true;
                }
            }
            Some(PaneAction::CopyPath(p)) => {
                if let Some(path) = self.tabs[self.active].root().leaf_at(&p).and_then(PtyTerm::cwd)
                {
                    ctx.copy_text(path);
                    self.toast = Some(("Copied path".into(), now + 1.4));
                }
            }
            Some(PaneAction::Split(p, dir, new_first)) => {
                let cwd = self.tabs[self.active].root().leaf_at(&p).and_then(PtyTerm::cwd);
                let new = PtyTerm::spawn(COLS, ROWS, ctx.clone(), &spawn_opts(&self.cfg, cwd));
                let tab = &mut self.tabs[self.active];
                let root = tab.root.take().expect("root");
                let (root, focus) = root.split(&p, dir, new, new_first);
                tab.root = Some(root);
                if let Some(f) = focus {
                    tab.focused = f;
                }
            }
            Some(PaneAction::Close(p)) => {
                let tab = &mut self.tabs[self.active];
                if tab.root().leaf_count() > 1 {
                    let root = tab.root.take().expect("root");
                    let (root, focus) = root.close(&p);
                    tab.root = root;
                    if let Some(f) = focus {
                        tab.focused = f;
                    }
                } else {
                    self.close_tab(self.active, ctx);
                }
            }
            Some(PaneAction::NewTab) => self.new_tab(ctx),
            None => {}
        }
        CentralOut { copied, bell_rang }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardPaste, resolve_clipboard_paste};

    #[test]
    fn text_wins_even_when_an_image_is_also_present() {
        assert_eq!(
            resolve_clipboard_paste(Some("hi".into()), true),
            Some(ClipboardPaste::Text("hi".into())),
        );
    }

    #[test]
    fn image_used_only_when_there_is_no_text() {
        assert_eq!(resolve_clipboard_paste(None, true), Some(ClipboardPaste::Image));
    }

    #[test]
    fn empty_clipboard_pastes_nothing() {
        assert_eq!(resolve_clipboard_paste(None, false), None);
    }
}
