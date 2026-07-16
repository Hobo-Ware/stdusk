//! stdusk - a quake terminal with a real GUI tab bar.
//! M0 chrome · M1 shell · M1.5 progress · M2 colors · M3 quake · M4 config · M5 tabs · M6 io · M6.5 selection.
//! The `eframe::App` loop here stays thin; drawing widgets + pure helpers live in `ui.rs`.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

mod colors;
mod config;
mod osc;
mod pane;
mod progress;
mod search;
mod terminal;
mod ui;
use config::Config;
use terminal::PtyTerm;
use ui::{
    ICON_BTN_W, apply_theme, basename, collect_input, color_swatch, draw_tab, draw_toast,
    icon_button, icon_toggle, icons, render_grid, style_menu, tint, toast_alpha,
};

const COLS: usize = 80;
const ROWS: usize = 24;
/// Fixed tab-bar row height - keeps every control centered on the same line (no drift).
const TABBAR_ROW_H: f32 = 34.0;

struct Tab {
    title: String,
    color: Option<egui::Color32>, // None = no underline (Tabby default); set via the menu
    renamed: bool,                // once renamed, stop auto-titling from cwd
    root: Option<pane::Pane<PtyTerm>>, // Option so whole-tree transforms can `take()` it
    focused: Vec<pane::Side>,     // path to the focused leaf (its identity)
}

impl Tab {
    fn root(&self) -> &pane::Pane<PtyTerm> {
        self.root.as_ref().expect("pane root")
    }
    fn root_mut(&mut self) -> &mut pane::Pane<PtyTerm> {
        self.root.as_mut().expect("pane root")
    }
    fn focused_term(&self) -> &PtyTerm {
        self.root().leaf_at(&self.focused).expect("focused leaf")
    }
    fn focused_term_mut(&mut self) -> &mut PtyTerm {
        let path = self.focused.clone();
        self.root_mut().leaf_at_mut(&path).expect("focused leaf")
    }
}

fn spawn_tab(ctx: &egui::Context, detect_progress: bool, cwd: Option<String>) -> Tab {
    Tab {
        title: "zsh".into(),
        color: None,
        renamed: false,
        root: Some(pane::Pane::leaf(PtyTerm::spawn(COLS, ROWS, ctx.clone(), detect_progress, cwd))),
        focused: Vec::new(),
    }
}

/// Scrollback-search overlay state (Cmd+F). Matches are found over the buffer; the "current"
/// one is highlighted via the terminal selection and scrolled into view.
struct Search {
    query: String,
    matches: Vec<search::Match>,
    current: usize,
    focus: bool, // request text-field focus on the next frame (set on open / after Enter)
    opts: search::SearchOpts, // case / regex / whole-word toggles
}

/// Deferred tab mutations collected during the UI pass, applied after (avoids borrow clashes).
enum TabAction {
    New,
    Rename(usize),
    SetColor(usize, Option<egui::Color32>),
    MoveLeft(usize),
    MoveRight(usize),
    Close(usize),
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
        if ui.button("Copy").clicked() {
            *action = Some(PaneAction::Copy(path.to_vec()));
        }
    });
    ui.add_enabled_ui(cwd.is_some(), |ui| {
        if ui.button("Copy current path").clicked() {
            *action = Some(PaneAction::CopyPath(path.to_vec()));
        }
    });
    ui.separator();
    ui.menu_button("Split", |ui| {
        use pane::SplitDir::{Column, Row};
        style_menu(ui);
        if ui.button("Right").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, false));
        }
        if ui.button("Down").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, false));
        }
        if ui.button("Left").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Row, true));
        }
        if ui.button("Up").clicked() {
            *action = Some(PaneAction::Split(path.to_vec(), Column, true));
        }
    });
    ui.separator();
    if ui.button("New tab").clicked() {
        *action = Some(PaneAction::NewTab);
    }
    if ui.button("Close pane").clicked() {
        *action = Some(PaneAction::Close(path.to_vec()));
    }
}

struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
    cfg: Config,
    _hotkey: GlobalHotKeyManager, // kept alive so the registration persists
    toggle: Arc<AtomicBool>,      // set by the hotkey thread, consumed in ui()
    visible: bool,
    was_focused: bool, // gained focus since last show (so blur can hide)
    sized: bool,       // applied quake sizing once the monitor size was known
    renaming: Option<(usize, String, bool)>, // (tab index, edit buffer, request-focus-once)
    search: Option<Search>, // scrollback-search overlay (Cmd+F), None when closed
    toast: Option<(String, f64)>, // transient status message + expiry (egui time)
    flash: f64,        // bell visual-flash expiry (egui time); 0 = none
    screenshot: Option<String>, // --screenshot PATH: demo tabs, capture, exit
}

impl Stdusk {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, screenshot: Option<String>) -> Self {
        // Load the Phosphor icon font (used for tab-bar controls + close x) as a fallback
        // in the proportional family, so icon codepoints render in buttons/labels.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "phosphor".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/Phosphor.ttf")).into(),
        );
        if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            keys.insert(1, "phosphor".to_owned());
        }
        // Broad monochrome fallbacks (macOS) for arrows / box-drawing / powerline / misc symbols
        // the bundled fonts miss - appended as lowest priority so the primary fonts win. Loaded
        // best-effort; absent files (other OSes) are simply skipped. NOTE: SMP color emoji
        // (😀 💰) still can't render - egui rasterizes monochrome glyph outlines only.
        for (name, path) in [
            ("sys-unicode", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
            ("sys-symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
                for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    if let Some(keys) = fonts.families.get_mut(&fam) {
                        keys.push(name.to_owned());
                    }
                }
            }
        }
        cc.egui_ctx.set_fonts(fonts);

        apply_theme(&cc.egui_ctx);

        // Global quake hotkey from config (default Ctrl+`). Carbon API on macOS - no
        // Accessibility grant needed.
        let mgr = GlobalHotKeyManager::new().expect("hotkey manager");
        let (mods, code) = config::parse_hotkey(&cfg.quake.hotkey);
        let _ = mgr.register(HotKey::new(mods, code));

        // A thread wakes the UI (even while hidden) when the hotkey fires.
        let toggle = Arc::new(AtomicBool::new(false));
        let toggle_thread = toggle.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if ev.state == HotKeyState::Pressed {
                    toggle_thread.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                }
            }
        });

        let detect = cfg.terminal.detect_progress;
        let mut tabs = vec![spawn_tab(&cc.egui_ctx, detect, None)];
        let mut active = 0;
        let mut sized = false;

        // Visual-test harness: populate representative tabs and skip monitor sizing.
        if screenshot.is_some() {
            for _ in 0..3 {
                tabs.push(spawn_tab(&cc.egui_ctx, detect, None));
            }
            let titles =
                ["auth-session", "smart-lists-really-long-name", "cocaine", "deconversion-monitor"];
            for (t, name) in tabs.iter_mut().zip(titles) {
                t.title = name.into();
                t.renamed = true;
            }
            tabs[0].color = Some(colors::tab_colors()[0]); // red
            tabs[3].color = Some(colors::tab_colors()[4]); // green
            active = 1;
            sized = true;
        }

        Self {
            tabs,
            active,
            cfg,
            _hotkey: mgr,
            toggle,
            visible: true,
            was_focused: false, // arm hide-on-blur only after the first focus gain
            sized,
            renaming: None,
            search: None,
            toast: None,
            flash: 0.0,
            screenshot,
        }
    }

    fn new_tab(&mut self, ctx: &egui::Context) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.focused_term().cwd());
        let detect = self.cfg.terminal.detect_progress;
        self.tabs.push(spawn_tab(ctx, detect, cwd));
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self, i: usize, ctx: &egui::Context) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
        }
        if self.tabs.is_empty() {
            let detect = self.cfg.terminal.detect_progress;
            self.tabs.push(spawn_tab(ctx, detect, None));
        }
        self.active = self.active.min(self.tabs.len() - 1);
    }

    fn move_tab(&mut self, i: usize, dir: i32) {
        let j = i as i32 + dir;
        if j < 0 || j as usize >= self.tabs.len() {
            return;
        }
        let j = j as usize;
        self.tabs.swap(i, j);
        if self.active == i {
            self.active = j;
        } else if self.active == j {
            self.active = i;
        }
    }

    /// Modal rename field, shown while `self.renaming` is set.
    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((idx, mut buf, mut focus)) = self.renaming.take() else {
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        egui::Window::new("Rename tab")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(ui::overlay_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Rename tab").color(colors::dim()));
                ui.add_space(6.0);
                let r = ui::text_field(ui, &mut buf, "Tab name", 220.0, colors::fg());
                // Focus ONCE on open. Re-requesting every frame would stop egui from ever
                // reporting the Enter-triggered lost_focus, so Enter would never commit.
                if focus {
                    r.request_focus();
                    focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui::action_button(ui, "Rename", true).clicked() {
                        commit = true;
                    }
                    if ui::action_button(ui, "Cancel", false).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if commit {
            if let Some(t) = self.tabs.get_mut(idx) {
                if !buf.trim().is_empty() {
                    t.title = buf;
                }
                t.renamed = true;
            }
        } else if !cancel {
            self.renaming = Some((idx, buf, focus)); // keep editing next frame
        }
    }

    /// Docked scrollback-search bar (Cmd+F): a top panel under the tab bar. Enter/Shift+Enter
    /// (or the buttons) cycle matches, Esc/Done closes. Current match highlighted via selection.
    fn find_panel(&mut self, ui: &mut egui::Ui) {
        let Some(mut st) = self.search.take() else {
            return;
        };
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
                    ui::overlay_frame().show(ui, |ui| {
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
                            let r = ui::text_field(ui, &mut st.query, "Find", 300.0, accent);
                            if st.focus {
                                r.request_focus();
                                st.focus = false;
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
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
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

/// Show (drop to the top edge, focused) or "hide" the quake window.
///
/// We do NOT use `Visible(false)` or move fully off-screen: on macOS that lets the OS
/// occlude the window and App-Nap the process, which throttles the run loop so the global
/// hotkey handler never fires again. Instead we park the window mostly below the screen,
/// leaving a ~2px sliver on-screen so it stays un-occluded and the run loop keeps delivering
/// the hotkey. A proper native hide (NSPanel orderOut) is a polish item.
fn apply_visibility(ctx: &egui::Context, visible: bool, height_pct: f32) {
    let mon = ctx.input(|i| i.viewport().monitor_size);
    if visible {
        if let Some(m) = mon {
            let h = (m.y * height_pct).round();
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(m.x, h)));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    } else {
        let y = mon.map_or(2000.0, |m| m.y - 2.0);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, y)));
    }
}

/// Right-click tab context menu. Sets `action`; egui auto-closes the menu on any button click.
fn tab_menu(
    ui: &mut egui::Ui,
    i: usize,
    current: Option<egui::Color32>,
    action: &mut Option<TabAction>,
) {
    style_menu(ui);
    if ui.button("New tab").clicked() {
        *action = Some(TabAction::New);
    }
    if ui.button("Rename…").clicked() {
        *action = Some(TabAction::Rename(i));
    }
    ui.menu_button("Color", |ui| {
        // Snug width for the swatch grid (style_menu's 210 leaves dead space here).
        ui.spacing_mut().button_padding = egui::vec2(12.0, 7.0);
        ui.set_min_width(168.0);
        if ui.button("No color").clicked() {
            *action = Some(TabAction::SetColor(i, None));
        }
        ui.add_space(4.0);
        // Filled-circle swatches, 2 rows of 6; the current color gets a ring.
        for row in colors::tab_colors().chunks(6) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for &col in row {
                    if color_swatch(ui, col, current == Some(col)).clicked() {
                        *action = Some(TabAction::SetColor(i, Some(col)));
                    }
                }
            });
        }
    });
    ui.separator();
    if ui.button("Move left").clicked() {
        *action = Some(TabAction::MoveLeft(i));
    }
    if ui.button("Move right").clicked() {
        *action = Some(TabAction::MoveRight(i));
    }
    ui.separator();
    if ui.button("Close").clicked() {
        *action = Some(TabAction::Close(i));
    }
}

impl eframe::App for Stdusk {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent framebuffer; the panel fills below carry the tint at `opacity`.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let height_pct = self.cfg.quake.height_pct;
        let opacity = self.cfg.appearance.opacity;

        // Quake window management is skipped in the screenshot harness.
        if self.screenshot.is_none() {
            // First run: apply full quake sizing once the monitor size is known.
            if !self.sized {
                if ctx.input(|i| i.viewport().monitor_size).is_some() {
                    apply_visibility(&ctx, true, height_pct);
                    self.sized = true;
                } else {
                    ctx.request_repaint();
                }
            }

            // Quake toggle (from the global-hotkey thread).
            if self.toggle.swap(false, Ordering::SeqCst) {
                self.visible = !self.visible;
                apply_visibility(&ctx, self.visible, height_pct);
                if self.visible {
                    self.was_focused = false;
                }
            }
            // Hide on focus loss (after we've gained focus since showing), if enabled.
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if self.visible {
                if focused {
                    self.was_focused = true;
                } else if self.was_focused && self.cfg.quake.hide_on_focus_loss {
                    self.visible = false;
                    apply_visibility(&ctx, false, height_pct);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // Screenshot harness: keep repainting so eframe's built-in capture (triggered by
        // EFRAME_SCREENSHOT_TO at pass 2) fires, then it saves the PNG and exits.
        if self.screenshot.is_some() {
            ctx.request_repaint();
        }

        // Auto-title unrenamed tabs from their cwd (basename).
        for tab in &mut self.tabs {
            if !tab.renamed
                && let Some(c) = tab.focused_term().cwd()
            {
                tab.title = basename(&c);
            }
        }

        // Browser-style keybinds: Cmd+T new, Cmd+W close focused pane/tab, Cmd+1..9 switch,
        // Cmd+D split side-by-side, Cmd+Shift+D split stacked.
        let mut kb_new = false;
        let mut kb_close = false;
        let mut kb_find = false;
        let mut kb_split: Option<pane::SplitDir> = None;
        let mut kb_switch: Option<usize> = None;
        ctx.input(|i| {
            if i.modifiers.command {
                use egui::Key::{D, F, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, T, W};
                if i.key_pressed(T) {
                    kb_new = true;
                }
                if i.key_pressed(W) {
                    kb_close = true;
                }
                if i.key_pressed(F) {
                    kb_find = true;
                }
                if i.key_pressed(D) {
                    kb_split = Some(if i.modifiers.shift {
                        pane::SplitDir::Column
                    } else {
                        pane::SplitDir::Row
                    });
                }
                for (n, k) in
                    [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9].into_iter().enumerate()
                {
                    if i.key_pressed(k) {
                        kb_switch = Some(n);
                    }
                }
            }
        });

        // Rounded window background - the OS window is transparent, so painting a rounded
        // rect leaves the corner triangles clear and the window reads as rounded. Panels
        // below are transparent so this shows through.
        ui.painter().rect_filled(ui.max_rect(), 10.0, tint(colors::bg(), opacity));

        // Tab bar. Collect clicks + menu actions; apply after the panel to avoid borrow clashes.
        let mut clicked: Option<usize> = None;
        let mut action: Option<TabAction> = None;
        let bar = egui::Panel::top("tabbar")
            .frame(
                egui::Frame::new()
                    // Distinct darker strip with rounded top corners, so the bar reads
                    // separately from the terminal body.
                    .fill(tint(colors::titlebar(), opacity))
                    .corner_radius(egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 })
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                // ONE left-to-right, center-aligned row for every control (tabs + icons). Nesting
                // opposing layouts is what kept misaligning the gear, so don't: the gear is pushed
                // to the right edge with a computed spacer instead. Fixed row height keeps every
                // item centered on the same line regardless of tab content.
                ui.horizontal(|ui| {
                    ui.set_min_height(TABBAR_ROW_H);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let active = i == self.active;
                        let (resp, close) = draw_tab(
                            ui,
                            i + 1,
                            &tab.title,
                            active,
                            tab.color,
                            tab.focused_term().progress(),
                            tab.focused_term().cmd_state(),
                            &tab.root().miniature(),
                        );
                        if close {
                            action = Some(TabAction::Close(i));
                        } else if resp.double_clicked() {
                            action = Some(TabAction::Rename(i)); // double-click to rename
                        } else if resp.clicked() {
                            clicked = Some(i);
                        }
                        let tab_color = tab.color;
                        resp.context_menu(|ui| tab_menu(ui, i, tab_color, &mut action));
                    }
                    ui.add_space(6.0);
                    if icon_button(ui, icons::PLUS, "New tab").clicked() {
                        action = Some(TabAction::New);
                    }
                    let mgr = icon_button(ui, icons::APP_WINDOW, "Tabs");
                    egui::Popup::menu(&mgr).show(|ui| {
                        style_menu(ui);
                        for (i, tab) in self.tabs.iter().enumerate() {
                            if ui.button(format!("{}   {}", i + 1, tab.title)).clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                    // Gear pinned to the right edge (spacer, not a nested layout).
                    ui.add_space((ui.available_width() - ICON_BTN_W).max(0.0));
                    if icon_button(ui, icons::GEAR, "Settings (config.toml)").clicked()
                        && let Some(p) = config::ensure_and_path()
                    {
                        let _ = std::process::Command::new("open").arg(p).spawn();
                    }
                });
            });

        // Hairline under the tab bar to delineate it from the terminal body.
        let br = bar.response.rect;
        ui.painter().hline(
            br.x_range(),
            br.bottom() - 0.5,
            egui::Stroke::new(1.0, colors::border()),
        );

        // Apply tab-bar clicks + keybinds + menu action (all structural mutations here).
        if let Some(i) = clicked {
            self.active = i;
        }
        if let Some(n) = kb_switch
            && n < self.tabs.len()
        {
            self.active = n;
        }
        if kb_new {
            action = Some(TabAction::New);
        }
        if let Some(dir) = kb_split {
            let cwd = self.tabs[self.active].focused_term().cwd();
            let detect = self.cfg.terminal.detect_progress;
            let new = PtyTerm::spawn(COLS, ROWS, ctx.clone(), detect, cwd);
            let tab = &mut self.tabs[self.active];
            let root = tab.root.take().expect("root");
            let (root, focus) = root.split(&tab.focused, dir, new, false);
            tab.root = Some(root);
            if let Some(f) = focus {
                tab.focused = f;
            }
        }
        if kb_close {
            // Cmd+W closes the focused pane; the tab only closes on its last pane.
            let tab = &mut self.tabs[self.active];
            if tab.root().leaf_count() > 1 {
                let root = tab.root.take().expect("root");
                let (root, focus) = root.close(&tab.focused);
                tab.root = root;
                if let Some(f) = focus {
                    tab.focused = f;
                }
            } else {
                action = Some(TabAction::Close(self.active));
            }
        }
        if kb_find {
            match self.search.take() {
                Some(_) => self.tabs[self.active].focused_term().clear_selection(),
                None => {
                    self.search = Some(Search {
                        query: String::new(),
                        matches: Vec::new(),
                        current: 0,
                        focus: true,
                        opts: search::SearchOpts::default(),
                    });
                }
            }
        }
        match action {
            Some(TabAction::New) => self.new_tab(&ctx),
            Some(TabAction::Rename(i)) => {
                if let Some(t) = self.tabs.get(i) {
                    self.renaming = Some((i, t.title.clone(), true));
                }
            }
            Some(TabAction::SetColor(i, c)) => {
                if let Some(t) = self.tabs.get_mut(i) {
                    t.color = c;
                }
            }
            Some(TabAction::MoveLeft(i)) => self.move_tab(i, -1),
            Some(TabAction::MoveRight(i)) => self.move_tab(i, 1),
            Some(TabAction::Close(i)) => self.close_tab(i, &ctx),
            None => {}
        }

        // A text field (find bar or rename dialog) owns the keyboard: don't forward keys to the
        // pty and don't let the terminal steal egui focus back while one is open. Captured MUST be
        // sampled BEFORE the modals run this frame - else the key that closes a modal (Enter to
        // commit a rename) would leak to the shell once the modal clears its own state.
        let input_captured = self.search.is_some() || self.renaming.is_some();

        self.rename_window(&ctx);

        // OSC 52: a shell "copy" request (from the focused pane) -> the system clipboard.
        if let Some(text) =
            self.tabs.get(self.active).and_then(|t| t.focused_term().take_clipboard())
        {
            ctx.copy_text(text);
        }

        self.find_panel(ui);

        let now = ctx.input(|i| i.time);
        let bell_on = self.cfg.terminal.bell != "off";
        let mut copied = false; // set inside the central panel when Cmd+C copies a selection
        let mut bell_rang = false; // any pane rang BEL this frame -> visual flash
        let mut pane_action: Option<PaneAction> = None; // from the pane right-click menu
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let area = ui.max_rect();
                let font = egui::FontId::monospace(self.cfg.appearance.font_size);
                let m = ui.painter().layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
                let (cw, ch) = (m.size().x, m.size().y);
                let cursor = ui::cursor_style(&self.cfg.terminal.cursor);

                let tab = &mut self.tabs[self.active];

                // Cmd+C copies the focused pane's selection (Ctrl+C stays SIGINT).
                let want_copy =
                    ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
                if want_copy && let Some(txt) = tab.focused_term().selection_text() {
                    ctx.copy_text(txt);
                    copied = true;
                }

                // Keystrokes + paste -> focused pane (unless the find bar owns the keyboard).
                if !input_captured {
                    let input = collect_input(ui);
                    if !input.is_empty() {
                        let t = tab.focused_term_mut();
                        t.send(&input);
                        t.clear_selection();
                        t.scroll_to_bottom();
                    }
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
                        let t = tab.focused_term_mut();
                        t.paste(&p);
                        t.clear_selection();
                        t.scroll_to_bottom();
                    }
                }

                // Tile the pane tree and render each leaf in its rect.
                let layout = tab.root().layout(area);
                let multi = layout.len() > 1;
                let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mut focus_click: Option<Vec<pane::Side>> = None;
                for (path, rect) in &layout {
                    {
                        let term = tab.root_mut().leaf_at_mut(path).expect("leaf");
                        let cols = (rect.width() / cw).floor().max(1.0) as usize;
                        let rows = (rect.height() / ch).floor().max(1.0) as usize;
                        term.resize(cols, rows);
                        // Wheel scroll goes to the pane under the pointer.
                        if scroll_y != 0.0 && pointer.is_some_and(|p| rect.contains(p)) {
                            let mut lines = (scroll_y / ch).round() as i32;
                            if lines == 0 {
                                lines = scroll_y.signum() as i32;
                            }
                            term.scroll(lines);
                        }
                    }
                    let term = tab.root().leaf_at(path).expect("leaf");
                    let snap = term.grid_snapshot();
                    let dimmed = multi && path != &tab.focused;
                    let has_sel = term.selection_text().is_some();
                    let cwd = term.cwd();
                    let resp =
                        render_grid(ui, path, *rect, term, &snap, cw, ch, &font, cursor, dimmed);
                    if bell_on && term.take_bell() {
                        bell_rang = true;
                    }
                    if resp.clicked() || resp.drag_started() {
                        focus_click = Some(path.clone());
                    }
                    // Keep egui keyboard focus on the active terminal, or a typed Space/Enter
                    // would activate a focused tab-bar button (e.g. the gear opening config.toml).
                    if !input_captured && path == &tab.focused {
                        resp.request_focus();
                    }
                    resp.context_menu(|ui| {
                        pane_menu(ui, path, has_sel, cwd.as_deref(), &mut pane_action);
                    });
                }
                if let Some(p) = focus_click {
                    tab.focused = p;
                }

                // Draggable splitters between panes.
                for (spath, dir, handle, parent) in tab.root().splitters(area) {
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
                let detect = self.cfg.terminal.detect_progress;
                let new = PtyTerm::spawn(COLS, ROWS, ctx.clone(), detect, cwd);
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
                    self.close_tab(self.active, &ctx);
                }
            }
            Some(PaneAction::NewTab) => self.new_tab(&ctx),
            None => {}
        }

        // Bell: a brief translucent flash over the whole window, fading out.
        if bell_rang {
            self.flash = now + 0.18;
        }
        if self.flash > now {
            let a = toast_alpha(self.flash - now, 0.18);
            let f = colors::fg();
            ui.painter().rect_filled(
                ui.max_rect(),
                10.0,
                egui::Color32::from_rgba_unmultiplied(f.r(), f.g(), f.b(), (55.0 * a) as u8),
            );
            ctx.request_repaint();
        }

        // Transient "Copied" toast at the bottom-center, fading out.
        if copied {
            self.toast = Some(("Copied".into(), now + 1.4));
        }
        if let Some((msg, until)) = self.toast.clone() {
            if now >= until {
                self.toast = None;
            } else {
                draw_toast(ui, &msg, toast_alpha(until - now, 0.35));
                ctx.request_repaint();
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let cfg = Config::load();
    colors::init(colors::by_name(&cfg.appearance.theme));

    // `--screenshot PATH`: populate demo tabs, render, save the PNG, and exit. Uses eframe's
    // built-in glow-backend capture via EFRAME_SCREENSHOT_TO.
    let args: Vec<String> = std::env::args().collect();
    let screenshot =
        args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1).cloned());
    if let Some(path) = &screenshot {
        // SAFE: single-threaded, set before any threads spawn (edition-2024 set_var is unsafe).
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("EFRAME_SCREENSHOT_TO", path);
        }
    }
    let size = if screenshot.is_some() { [1400.0, 420.0] } else { [1200.0, 500.0] };

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow, // __screenshot capture requires the glow backend
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_transparent(true)
            .with_inner_size(size)
            .with_position([0.0, 0.0]),
        ..Default::default()
    };
    eframe::run_native(
        "stdusk",
        options,
        Box::new(move |cc| Ok(Box::new(Stdusk::new(cc, cfg, screenshot)))),
    )
}
