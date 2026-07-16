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
mod progress;
mod search;
mod terminal;
mod ui;
use config::Config;
use terminal::PtyTerm;
use ui::{
    apply_theme, basename, collect_input, draw_tab, draw_toast, icon_button, icon_toggle, icons,
    render_grid, tint, toast_alpha,
};

const COLS: usize = 80;
const ROWS: usize = 24;

struct Tab {
    title: String,
    color: Option<egui::Color32>, // None = no underline (Tabby default); set via the menu
    renamed: bool,                // once renamed, stop auto-titling from cwd
    term: PtyTerm,
}

fn spawn_tab(ctx: &egui::Context, detect_progress: bool, cwd: Option<String>) -> Tab {
    Tab {
        title: "zsh".into(),
        color: None,
        renamed: false,
        term: PtyTerm::spawn(COLS, ROWS, ctx.clone(), detect_progress, cwd),
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

struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
    cfg: Config,
    _hotkey: GlobalHotKeyManager, // kept alive so the registration persists
    toggle: Arc<AtomicBool>,      // set by the hotkey thread, consumed in ui()
    visible: bool,
    was_focused: bool, // gained focus since last show (so blur can hide)
    sized: bool,       // applied quake sizing once the monitor size was known
    renaming: Option<(usize, String)>, // tab index + edit buffer while renaming
    search: Option<Search>, // scrollback-search overlay (Cmd+F), None when closed
    toast: Option<(String, f64)>, // transient status message + expiry (egui time)
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
            screenshot,
        }
    }

    fn new_tab(&mut self, ctx: &egui::Context) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.term.cwd());
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
        let Some((idx, mut buf)) = self.renaming.take() else {
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        egui::Window::new("Rename tab")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut buf);
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
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
            self.renaming = Some((idx, buf)); // keep editing next frame
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
                    egui::Frame::new()
                        .fill(colors::elevated())
                        .stroke(egui::Stroke::new(1.0, colors::border()))
                        .corner_radius(12)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(100),
                        })
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 7.0;
                                ui.visuals_mut().extreme_bg_color = colors::bg(); // field bg = theme
                                ui.label(
                                    egui::RichText::new(icons::MAGNIFYING_GLASS)
                                        .size(17.0)
                                        .color(colors::dim()),
                                );
                                let accent = if no_results { colors::red() } else { colors::fg() };
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut st.query)
                                        .desired_width(300.0)
                                        .font(egui::FontId::proportional(16.0))
                                        .margin(egui::Margin::symmetric(8, 6))
                                        .text_color(accent)
                                        .hint_text("Find"),
                                );
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
                                if icon_toggle(
                                    ui,
                                    icons::ASTERISK,
                                    st.opts.regex,
                                    "Regular expression",
                                )
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
                                if icon_button(ui, icons::CARET_UP, "Previous (Shift+Enter)")
                                    .clicked()
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

        let term = &self.tabs[self.active].term;
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
            self.tabs[self.active].term.clear_selection();
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
fn tab_menu(ui: &mut egui::Ui, i: usize, action: &mut Option<TabAction>) {
    if ui.button("New tab").clicked() {
        *action = Some(TabAction::New);
    }
    if ui.button("Rename…").clicked() {
        *action = Some(TabAction::Rename(i));
    }
    ui.menu_button("Color", |ui| {
        if ui.button("No color").clicked() {
            *action = Some(TabAction::SetColor(i, None));
        }
        ui.horizontal(|ui| {
            for col in colors::tab_colors() {
                if ui.button(egui::RichText::new("⬤").color(col)).clicked() {
                    *action = Some(TabAction::SetColor(i, Some(col)));
                }
            }
        });
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
                && let Some(c) = tab.term.cwd()
            {
                tab.title = basename(&c);
            }
        }

        // Browser-style keybinds: Cmd+T new, Cmd+W close, Cmd+1..9 switch.
        let mut kb_new = false;
        let mut kb_close = false;
        let mut kb_find = false;
        let mut kb_switch: Option<usize> = None;
        ctx.input(|i| {
            if i.modifiers.command {
                use egui::Key::{F, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, T, W};
                if i.key_pressed(T) {
                    kb_new = true;
                }
                if i.key_pressed(W) {
                    kb_close = true;
                }
                if i.key_pressed(F) {
                    kb_find = true;
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
                ui.horizontal(|ui| {
                    // Gear pinned to the far right; everything else flows from the left.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        if icon_button(ui, icons::GEAR, "Settings (config.toml)").clicked()
                            && let Some(p) = config::ensure_and_path()
                        {
                            let _ = std::process::Command::new("open").arg(p).spawn();
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            // Tabs.
                            for (i, tab) in self.tabs.iter().enumerate() {
                                let active = i == self.active;
                                let (resp, close) = draw_tab(
                                    ui,
                                    i + 1,
                                    &tab.title,
                                    active,
                                    tab.color,
                                    tab.term.progress(),
                                );
                                if close {
                                    action = Some(TabAction::Close(i));
                                } else if resp.clicked() {
                                    clicked = Some(i);
                                }
                                resp.context_menu(|ui| tab_menu(ui, i, &mut action));
                            }
                            // New tab + tab manager, right after the tabs.
                            ui.add_space(6.0);
                            if icon_button(ui, icons::PLUS, "New tab").clicked() {
                                action = Some(TabAction::New);
                            }
                            let mgr = icon_button(ui, icons::APP_WINDOW, "Tabs");
                            egui::Popup::menu(&mgr).show(|ui| {
                                ui.set_min_width(200.0);
                                for (i, tab) in self.tabs.iter().enumerate() {
                                    if ui.button(format!("{}   {}", i + 1, tab.title)).clicked() {
                                        clicked = Some(i);
                                    }
                                }
                            });
                        });
                    });
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
        if kb_close {
            action = Some(TabAction::Close(self.active));
        }
        if kb_find {
            match self.search.take() {
                Some(_) => self.tabs[self.active].term.clear_selection(),
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
                    self.renaming = Some((i, t.title.clone()));
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

        self.rename_window(&ctx);

        // OSC 52: a shell "copy" request -> the system clipboard.
        if let Some(text) = self.tabs.get(self.active).and_then(|t| t.term.take_clipboard()) {
            ctx.copy_text(text);
        }

        let search_open = self.search.is_some(); // gate pty input while the find bar is open
        self.find_panel(ui);

        let now = ctx.input(|i| i.time);
        let mut copied = false; // set inside the central panel when Cmd+C copies a selection
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let area = ui.max_rect();
                let term = &mut self.tabs[self.active].term;

                // Cmd+C arrives as egui's Copy event (not a raw key). Copy the selection;
                // Ctrl+C stays SIGINT (collect_input handles that). `copied` -> toast below.
                let want_copy =
                    ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
                if want_copy && let Some(txt) = term.selection_text() {
                    ctx.copy_text(txt);
                    copied = true;
                }

                // Keystrokes -> pty; typing clears any selection and jumps back to the bottom.
                // Suppressed while the find bar is open (it owns the keyboard).
                if !search_open {
                    let input = collect_input(ui);
                    if !input.is_empty() {
                        term.send(&input);
                        term.clear_selection();
                        term.scroll_to_bottom();
                    }

                    // Paste (Cmd+V -> egui Paste event); bracketed if the app requested it.
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
                        term.paste(&p);
                        term.clear_selection();
                        term.scroll_to_bottom();
                    }
                }

                // Cell metrics + wheel scrollback.
                let font = egui::FontId::monospace(self.cfg.appearance.font_size);
                let m = ui.painter().layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
                let (cw, ch) = (m.size().x, m.size().y);

                let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll_y != 0.0 {
                    let mut lines = (scroll_y / ch).round() as i32;
                    if lines == 0 {
                        lines = scroll_y.signum() as i32; // ensure small deltas still move
                    }
                    term.scroll(lines);
                }

                // Resize pty + grid to fit the available area.
                let avail = ui.available_size();
                let cols = (avail.x / cw).floor().max(1.0) as usize;
                let rows = (avail.y / ch).floor().max(1.0) as usize;
                term.resize(cols, rows);

                let snap = term.grid_snapshot();
                render_grid(ui, term, &snap, cw, ch, &font);

                // Scrollbar on the right edge when there's scrollback history.
                let (offset, history) = term.scroll_state();
                if history > 0 {
                    let track = area.height();
                    let total = (history + snap.rows) as f32;
                    let thumb_h = (snap.rows as f32 / total * track).max(24.0);
                    let top_frac = (history - offset) as f32 / total;
                    let thumb_y = area.top() + top_frac * track;
                    let bar_x = area.right() - 6.0;

                    let track_rect = egui::Rect::from_min_max(
                        egui::pos2(bar_x - 2.0, area.top()),
                        egui::pos2(area.right(), area.bottom()),
                    );
                    let resp = ui.interact(
                        track_rect,
                        ui.id().with("scrollbar"),
                        egui::Sense::click_and_drag(),
                    );
                    if (resp.dragged() || resp.clicked())
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let frac = ((p.y - area.top()) / track).clamp(0.0, 1.0);
                        let tgt = ((1.0 - frac) * history as f32).round() as usize;
                        term.scroll_to_offset(tgt.min(history));
                    }
                    let alpha = if resp.hovered() || resp.dragged() { 180 } else { 90 };
                    let d = colors::dim();
                    let col = egui::Color32::from_rgba_unmultiplied(d.r(), d.g(), d.b(), alpha);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(bar_x, thumb_y),
                            egui::vec2(4.0, thumb_h),
                        ),
                        2.0,
                        col,
                    );
                }
            });

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
