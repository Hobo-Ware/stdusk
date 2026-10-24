//! Workspace: the central panel that tiles the active tab's pane tree (render,
//! input/paste routing, splitters), the pane right-click menu, and the deferred
//! pane-action apply.

use eframe::egui;

use crate::tabs::spawn_opts;
use crate::ui;
use stdusk_core::terminal::PtyTerm;
use stdusk_egui::mouse::GridGeom;
use stdusk_egui::pointer::MousePasted;
use stdusk_egui::terminal_view::{Focus, Gate, TerminalOptions, TerminalView, paste_into};
use stdusk_egui::ui::GridStyle;
use stdusk_egui::widgets::style_menu;

use crate::{COLS, ROWS, Stdusk, colors, pane};

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
        if stdusk_egui::widgets::menu_item(ui, "Copy", "Cmd+C").clicked() {
            *action = Some(PaneAction::Copy(path.to_vec()));
        }
    });
    ui.add_enabled_ui(cwd.is_some(), |ui| {
        if stdusk_egui::widgets::menu_item(ui, "Copy current path", "").clicked() {
            *action = Some(PaneAction::CopyPath(path.to_vec()));
        }
    });
    ui.separator();
    ui.menu_button("Split", |ui| {
        use pane::SplitDir::{Column, Row};
        style_menu(ui);
        if stdusk_egui::widgets::menu_item(ui, "Right", "Cmd+D").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, false));
        }
        if stdusk_egui::widgets::menu_item(ui, "Down", "Cmd+Shift+D").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, false));
        }
        if stdusk_egui::widgets::menu_item(ui, "Left", "").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, true));
        }
        if stdusk_egui::widgets::menu_item(ui, "Up", "").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, true));
        }
    });
    ui.separator();
    if stdusk_egui::widgets::menu_item(ui, "New tab", "Cmd+T").clicked() {
        *action = Some(PaneAction::NewTab);
    }
    if stdusk_egui::widgets::menu_item(ui, "Close pane", "Cmd+W").clicked() {
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
                        egui::FontFamily::Name(stdusk_egui::fonts::BOLD_FONT_FAMILY.into()),
                    )
                });
                let m = ui.painter().layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
                let (cw, ch) = (
                    m.size().x,
                    ui::padded_cell_height(m.size().y, self.cfg.appearance.line_padding),
                );
                let mods = ui.input(|i| i.modifiers);
                let tcfg = self.cfg.terminal.clone();
                // All find-bar matches, drawn as a dim overlay on the searched (focused) pane;
                // the current match keeps its brighter selection highlight on top.
                let search_marks = self.search.as_ref().map_or(&[][..], |s| s.matches.as_slice());
                let tab = &mut self.tabs[self.active];

                // Typing and pastes reach only the focused pane; a text field owns pastes. The find
                // bar counts only while its field has focus - an open, unfocused bar must not eat
                // pastes aimed at the terminal.
                let paste_owned = self.search.as_ref().is_some_and(|s| s.field_focused)
                    || self.renaming.is_some()
                    || self.palette.is_some();
                let mut typed: Vec<u8> = Vec::new();
                let mut pasted: Vec<String> = Vec::new();

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
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mut focus_click: Option<Vec<pane::Side>> = None;
                let mut mouse_paste: Option<(Vec<pane::Side>, MousePasted)> = None; // middle/right click
                let mut restart_pane: Option<Vec<pane::Side>> = None;
                let mut clicked_pane: Option<Vec<pane::Side>> = None; // a click ends its restore notice
                for (path, rect) in &layout {
                    let focused = path == &tab.focused;
                    // Broadcast mode reads every pane as focused (Tabby _allFocusMode drops the
                    // unfocused fade on all panes), so no pane looks like it won't get the keys.
                    let dimmed = multi && !focused && !tab.broadcast;
                    let keys_free = focused && !input_captured;
                    let tab_id = tab.id;
                    let term = tab.root_mut().leaf_at_mut(path).expect("leaf");
                    let cols = (rect.width() / cw).floor().max(1.0) as usize;
                    let rows = (rect.height() / ch).floor().max(1.0) as usize;
                    term.resize(cols, rows);
                    let out = TerminalView {
                        id_src: path,
                        grid: GridGeom { rect: *rect, cw, ch, cols, rows },
                        font: &font,
                        bold_font: bold_font.as_ref(),
                        style: GridStyle {
                            dimmed,
                            ..GridStyle::from_config(&tcfg, mods, keys_free)
                        },
                        // The find bar searches the focused pane only.
                        search_marks: if focused { search_marks } else { &[] },
                        options: TerminalOptions::from_config(
                            &tcfg,
                            if focused { Focus::LockKeys } else { Focus::Off },
                        ),
                        gate: Gate {
                            keyboard: keys_free,
                            paste: focused && !paste_owned,
                            copy: focused,
                            wheel: true,
                            reports: !input_captured,
                            pointer: !input_captured,
                        },
                        injected_wheel: &[],
                    }
                    .show(ui, term);
                    copied |= out.copied.is_some();
                    for text in out.confirm_paste {
                        self.pending_pastes.push_back((tab_id, text));
                    }
                    if focused {
                        typed = out.typed;
                        pasted = out.pasted;
                    }
                    let resp = out.response;
                    let term = tab.root().leaf_at(path).expect("leaf");
                    let has_sel = term.selection_text().is_some();
                    let cwd = term.cwd();
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
                    bell_rang |= out.bell_rang;
                    if let Some(pasted) = out.mouse_pasted {
                        mouse_paste = Some((path.clone(), pasted));
                    }
                    let open_menu = out.menu_requested;
                    let hovered_now = pointer.is_some_and(|p| rect.contains(p));
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
                    if let Some(notice) = term.notice() {
                        ui::draw_pane_notice(ui, *rect, &ui::pane_notice_text(notice));
                    }
                    if resp.clicked() || resp.drag_started() {
                        focus_click = Some(path.clone());
                        clicked_pane = Some(path.clone());
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
                if tab.broadcast && (!typed.is_empty() || !pasted.is_empty()) {
                    let focused = tab.focused.clone();
                    for (path, _) in &layout {
                        if *path == focused {
                            continue;
                        }
                        let Some(term) = tab.root_mut().leaf_at_mut(path) else { continue };
                        if !typed.is_empty() {
                            term.send(&typed);
                            term.clear_selection();
                            term.scroll_to_bottom();
                        }
                        for text in &pasted {
                            paste_into(term, text);
                        }
                    }
                }
                if let Some(p) = focus_click {
                    tab.focused = p;
                }
                if let Some(t) = clicked_pane.and_then(|p| tab.root_mut().leaf_at_mut(&p)) {
                    t.dismiss_notice();
                }
                // Respawn a dead pane (deferred: needs &mut past the render borrow).
                if let Some(p) = restart_pane
                    && let Some(t) = tab.root_mut().leaf_at_mut(&p)
                {
                    crate::tabs::respawn_term(&self.cfg, ctx, t);
                }
                if let Some((p, pasted)) = mouse_paste {
                    tab.focused = p;
                    let label = match pasted {
                        MousePasted::Text => "Pasted",
                        MousePasted::Image => "Pasted image",
                    };
                    self.toast = Some((label.into(), now + 1.4));
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
                let new = PtyTerm::spawn(
                    COLS,
                    ROWS,
                    stdusk_egui::repaint_handle(ctx),
                    &spawn_opts(&self.cfg, cwd),
                );
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
