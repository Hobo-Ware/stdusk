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
mod links;
mod osc;
mod pane;
mod procwatch;
mod progress;
mod search;
mod session;
mod shell;
mod terminal;
mod themes;
mod tray;
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

/// Monotonic tab identity - stable across reorders/closes (used to target deferred actions).
static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Tab {
    id: u64,
    title: String,
    color: Option<egui::Color32>, // None = no underline (Tabby default); set via the menu
    renamed: bool,                // once renamed, stop auto-titling from cwd
    root: Option<pane::Pane<PtyTerm>>, // Option so whole-tree transforms can `take()` it
    focused: Vec<pane::Side>,     // path to the focused leaf (its identity)
    cli: Option<procwatch::Cli>,  // a known AI CLI detected running in the tab (badge)
    maximized: bool,              // zoom the focused pane to fill the tab (hide the other panes)
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

/// Bundle the config bits a terminal spawn needs.
fn spawn_opts(cfg: &Config, cwd: Option<String>) -> terminal::SpawnOpts {
    terminal::SpawnOpts {
        detect_progress: cfg.terminal.detect_progress,
        shell_integration: cfg.terminal.shell_integration,
        scrollback_lines: cfg.terminal.scrollback_lines,
        word_separators: cfg.terminal.word_separators.clone(),
        bold_bright: cfg.terminal.bold_bright,
        cwd,
    }
}

fn spawn_tab(cfg: &Config, ctx: &egui::Context, cwd: Option<String>) -> Tab {
    let term = PtyTerm::spawn(COLS, ROWS, ctx.clone(), &spawn_opts(cfg, cwd));
    Tab {
        id: NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed),
        title: "zsh".into(),
        color: None,
        renamed: false,
        root: Some(pane::Pane::leaf(term)),
        focused: Vec::new(),
        cli: None,
        maximized: false,
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
    Duplicate(usize),
    Rename(usize),
    Restart(usize),
    SetColor(usize, Option<egui::Color32>),
    MoveLeft(usize),
    MoveRight(usize),
    Close(usize),
    CloseOthers(usize),
    CloseRight(usize),
    CloseLeft(usize),
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

#[allow(clippy::struct_excessive_bools)] // independent app-state flags, not a mode
struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
    cfg: Config,
    _hotkey: GlobalHotKeyManager, // kept alive so the registration persists
    toggle: Arc<AtomicBool>,      // set by the hotkey thread, consumed in ui()
    visible: bool,
    dock_shown: bool, // last-applied Dock-icon state (dynamic dock_when_visible mode)
    was_focused: bool, // gained focus since last show (so blur can hide)
    sized: bool,      // applied quake sizing once the monitor size was known
    renaming: Option<(usize, String, bool)>, // (tab index, edit buffer, request-focus-once)
    search: Option<Search>, // scrollback-search overlay (Cmd+F), None when closed
    closed: Vec<String>, // cwds of recently closed tabs, for reopen (Cmd+Shift+T)
    pending_pastes: std::collections::VecDeque<(u64, String)>, // multiline pastes awaiting confirm (tab id, text)
    toast: Option<(String, f64)>, // transient status message + expiry (egui time)
    flash: f64,                   // bell visual-flash expiry (egui time); 0 = none
    zoom: f32,                    // font-size multiplier (Cmd +/-/0)
    theme_name: String,           // currently-applied theme (to detect OS light/dark changes)
    sys: sysinfo::System,         // process table for CLI-awareness scans
    next_cli_scan: f64,           // egui time of the next throttled procwatch scan
    next_session_save: f64,       // egui time of the next throttled session persist
    last_session: session::SavedSession, // last persisted session (skip identical writes)
    tray: Option<tray::Tray>,     // menu-bar status item (kept alive; Some when enabled)
    screenshot: Option<String>,   // --screenshot PATH: demo tabs, capture, exit
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
        // Full monochrome Noto Emoji (vendored) - egui's bundled emoji font is a subset that
        // misses most SMP emoji (😀 💰 ...), so append this to both families to fill the gap.
        // Monochrome (glyf outlines) so egui can rasterize it; color emoji still won't render.
        fonts.font_data.insert(
            "noto-emoji".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/NotoEmoji-Regular.ttf")).into(),
        );
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(keys) = fonts.families.get_mut(&fam) {
                keys.push("noto-emoji".to_owned());
            }
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

        // Session restore: reopen last session's tabs (cwd/title/color); else one fresh tab.
        let mut tabs = Vec::new();
        let mut active = 0;
        if cfg.session.restore && screenshot.is_none() {
            let saved = session::load();
            for st in &saved.tabs {
                let mut tab = spawn_tab(&cfg, &cc.egui_ctx, st.cwd.clone());
                if let Some(title) = &st.title {
                    tab.title.clone_from(title);
                    tab.renamed = true;
                }
                tab.color = st.color.as_deref().and_then(session::hex_to_color);
                tabs.push(tab);
            }
            active = saved.active.min(tabs.len().saturating_sub(1));
        }
        if tabs.is_empty() {
            tabs.push(spawn_tab(&cfg, &cc.egui_ctx, None));
            active = 0;
        }
        let mut sized = false;

        // Visual-test harness: populate representative tabs and skip monitor sizing.
        if screenshot.is_some() {
            for _ in 0..3 {
                tabs.push(spawn_tab(&cfg, &cc.egui_ctx, None));
            }
            let titles =
                ["auth-session", "smart-lists-really-long-name", "cocaine", "deconversion-monitor"];
            for (t, name) in tabs.iter_mut().zip(titles) {
                t.title = name.into();
                t.renamed = true;
            }
            tabs[0].color = Some(colors::tab_colors()[0]); // red
            tabs[3].color = Some(colors::tab_colors()[4]); // green
            tabs[0].cli = Some(procwatch::Cli::Claude); // demo the CLI-awareness badge
            tabs[2].cli = Some(procwatch::Cli::Gemini);
            active = 1;
            sized = true;
        }

        // Menu-bar status item is the accessory app's presence + control; skip it in the
        // screenshot harness and when disabled.
        let tray = (cfg.quake.menu_bar_icon && screenshot.is_none()).then(tray::build).flatten();
        let theme_name = cfg.appearance.theme.clone();

        Self {
            tabs,
            active,
            cfg,
            _hotkey: mgr,
            toggle,
            visible: true,
            dock_shown: true, // launches regular (dynamic mode) / irrelevant otherwise
            was_focused: false, // arm hide-on-blur only after the first focus gain
            sized,
            renaming: None,
            search: None,
            closed: Vec::new(),
            pending_pastes: std::collections::VecDeque::new(),
            toast: None,
            flash: 0.0,
            zoom: 1.0,
            theme_name,
            sys: sysinfo::System::new(),
            next_cli_scan: 0.0,
            next_session_save: 0.0,
            last_session: session::SavedSession::default(),
            tray,
            screenshot,
        }
    }

    fn new_tab(&mut self, ctx: &egui::Context) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.focused_term().cwd());
        let tab = spawn_tab(&self.cfg, ctx, cwd);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self, i: usize, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get(i) {
            if let Some(cwd) = tab.focused_term().cwd() {
                self.closed.push(cwd); // remember for reopen (Cmd+Shift+T)
                if self.closed.len() > 20 {
                    self.closed.remove(0);
                }
            }
            self.tabs.remove(i);
        }
        if self.tabs.is_empty() {
            let tab = spawn_tab(&self.cfg, ctx, None);
            self.tabs.push(tab);
        }
        self.active = self.active.min(self.tabs.len() - 1);
    }

    /// In the dynamic `dock_when_visible` mode, keep the Dock icon (+ menu bar) in sync with the
    /// window's visibility. Only touches the activation policy when it actually changes.
    fn sync_dock(&mut self) {
        if self.cfg.quake.hide_from_dock
            && self.cfg.quake.dock_when_visible
            && self.visible != self.dock_shown
        {
            set_dock_icon(self.visible);
            self.dock_shown = self.visible;
        }
    }

    /// Close every tab whose index fails `keep`, remembering cwds for reopen. The tab at
    /// `focus` (which must pass `keep`) becomes active.
    fn close_tabs_where(&mut self, keep: impl Fn(usize) -> bool, focus: usize) {
        let mut kept = Vec::new();
        let mut new_active = 0;
        for (i, tab) in self.tabs.drain(..).enumerate() {
            if keep(i) {
                if i == focus {
                    new_active = kept.len();
                }
                kept.push(tab);
            } else if let Some(cwd) = tab.focused_term().cwd() {
                self.closed.push(cwd);
            }
        }
        // Cap the reopen stack, dropping the OLDEST entries (front) so pop() stays most-recent.
        while self.closed.len() > 20 {
            self.closed.remove(0);
        }
        self.tabs = kept;
        self.active = new_active;
    }

    /// Reopen the most recently closed tab (in its old cwd), if any.
    fn reopen_tab(&mut self, ctx: &egui::Context) {
        if let Some(cwd) = self.closed.pop() {
            let tab = spawn_tab(&self.cfg, ctx, Some(cwd));
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
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

    /// Multiline-paste confirmation (Tabby's warnOnMultilinePaste): preview + Paste/Cancel.
    /// Shown while `pending_pastes` is non-empty (front first); the modal path intentionally
    /// skips the trim rules. Targets the tab the paste happened in, by stable id.
    fn paste_confirm_window(&mut self, ctx: &egui::Context) {
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
            .frame(ui::overlay_frame())
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
                    if ui::action_button(ui, "Paste", true).clicked() {
                        do_paste = true;
                    }
                    if ui::action_button(ui, "Cancel", false).clicked() {
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
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                let t = tab.focused_term_mut();
                t.paste(&text);
                t.clear_selection();
                t.scroll_to_bottom();
            }
        } else if !cancel {
            self.pending_pastes.push_front((tab_id, text)); // keep asking next frame
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
/// Show/hide the Dock icon (+ menu bar) at runtime by flipping the macOS activation policy.
/// Used only in the dynamic `dock_when_visible` mode. No-op off macOS.
#[cfg(target_os = "macos")]
fn set_dock_icon(visible: bool) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
    }
}
#[cfg(not(target_os = "macos"))]
fn set_dock_icon(_visible: bool) {}

/// Post a desktop notification that a long command finished (macOS `osascript`).
fn notify_done(title: &str, code: i32) {
    #[cfg(target_os = "macos")]
    {
        let status =
            if code == 0 { "finished".to_owned() } else { format!("failed (exit {code})") };
        let body = format!("{title}: command {status}");
        let script = format!("display notification {body:?} with title \"stdusk\"");
        let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (title, code);
}

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
    if ui.button("Duplicate").clicked() {
        *action = Some(TabAction::Duplicate(i));
    }
    if ui.button("Rename…").clicked() {
        *action = Some(TabAction::Rename(i));
    }
    if ui.button("Restart").clicked() {
        *action = Some(TabAction::Restart(i));
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
    if ui.button("Close other tabs").clicked() {
        *action = Some(TabAction::CloseOthers(i));
    }
    if ui.button("Close tabs to the right").clicked() {
        *action = Some(TabAction::CloseRight(i));
    }
    if ui.button("Close tabs to the left").clicked() {
        *action = Some(TabAction::CloseLeft(i));
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

            // Menu-bar item: Show/Hide toggles the window, Quit exits.
            if let Some(tray) = &self.tray {
                let (show, quit) = tray::poll(tray);
                if quit {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if show {
                    self.visible = !self.visible;
                    apply_visibility(&ctx, self.visible, height_pct);
                    if self.visible {
                        self.was_focused = false;
                    }
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
            self.sync_dock();
        }

        // Screenshot harness: keep repainting so eframe's built-in capture (triggered by
        // EFRAME_SCREENSHOT_TO at pass 2) fires, then it saves the PNG and exits.
        if self.screenshot.is_some() {
            ctx.request_repaint();
        }

        // Follow the OS light/dark appearance (or the manual theme when follow_system is off).
        // Re-inits colors + egui visuals only when the resolved theme actually changes.
        if self.screenshot.is_none() {
            let want = if self.cfg.appearance.follow_system {
                match ctx.input(|i| i.raw.system_theme) {
                    Some(egui::Theme::Light) => &self.cfg.appearance.theme_light,
                    _ => &self.cfg.appearance.theme_dark,
                }
            } else {
                &self.cfg.appearance.theme
            };
            if *want != self.theme_name {
                colors::set(colors::by_name(want));
                apply_theme(&ctx);
                self.theme_name = want.clone();
                ctx.request_repaint();
            }
        }

        // Auto-title unrenamed tabs from their cwd (basename).
        for tab in &mut self.tabs {
            if !tab.renamed
                && let Some(c) = tab.focused_term().cwd()
            {
                tab.title = basename(&c);
            }
        }

        // Session persist: snapshot open tabs (cwd/title/color) every few seconds; skip identical
        // writes so the file only changes when the session does.
        if self.cfg.session.restore && self.screenshot.is_none() {
            let now = ctx.input(|i| i.time);
            if now >= self.next_session_save {
                self.next_session_save = now + 3.0;
                let snap = session::SavedSession {
                    tabs: self
                        .tabs
                        .iter()
                        .map(|t| session::SavedTab {
                            title: t.renamed.then(|| t.title.clone()),
                            color: t.color.map(session::color_to_hex),
                            cwd: t.focused_term().cwd(),
                        })
                        .collect(),
                    active: self.active,
                };
                if snap != self.last_session {
                    session::save(&snap);
                    self.last_session = snap;
                }
            }
        }

        // Notify-when-done: a long command finished. Consume the flag always (so it doesn't fire
        // late), but only post a notification when stdusk is hidden - no nagging while you watch.
        for tab in &self.tabs {
            if let Some(code) = tab.focused_term().take_done_notify()
                && self.cfg.terminal.notify_on_done
                && !self.visible
            {
                notify_done(&tab.title, code);
            }
        }

        // CLI awareness: ~1 Hz, refresh the process table and badge each tab with any known AI CLI
        // running in it (scanned across all of the tab's panes). Skipped in the screenshot harness
        // (it sets demo badges directly).
        if self.cfg.terminal.detect_clis && self.screenshot.is_none() {
            let now = ctx.input(|i| i.time);
            if now >= self.next_cli_scan {
                self.next_cli_scan = now + 1.0;
                self.sys.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::All,
                    true,
                    sysinfo::ProcessRefreshKind::nothing()
                        .with_cmd(sysinfo::UpdateKind::OnlyIfNotSet),
                );
                for tab in &mut self.tabs {
                    let pids: Vec<u32> =
                        tab.root().leaves().iter().filter_map(|t| t.shell_pid()).collect();
                    tab.cli = pids.iter().find_map(|&pid| procwatch::scan(&self.sys, pid));
                }
                // Keep the cadence ticking even when the window is otherwise idle.
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }

        // Browser-style keybinds: Cmd+T new, Cmd+W close focused pane/tab, Cmd+1..9 switch,
        // Cmd+D split side-by-side, Cmd+Shift+D split stacked.
        let mut kb_new = false;
        let mut kb_close = false;
        let mut kb_find = false;
        let mut kb_split: Option<pane::SplitDir> = None;
        let mut kb_switch: Option<usize> = None;
        let mut kb_pane_dir: Option<pane::Dir> = None; // Cmd+Alt+arrow: focus the neighbor pane
        let mut kb_maximize = false; // Cmd+Alt+Enter: toggle zooming the focused pane
        let mut kb_select_all = false; // Cmd+A
        let mut kb_clear = false; // Cmd+K
        let mut kb_zoom: Option<i8> = None; // Cmd +/= (1), Cmd - (-1), Cmd 0 (0 = reset)
        let mut kb_scroll_pages: Option<i32> = None; // Shift+PageUp/Down: -1 up, +1 down
        let mut kb_tab_cycle: Option<i32> = None; // Ctrl+Tab next (+1) / Ctrl+Shift+Tab prev (-1)
        let mut kb_reopen = false; // Cmd+Shift+T: reopen last closed tab
        let mut kb_resize: Option<(pane::SplitDir, f32)> = None; // Cmd+Ctrl+arrow: resize focused pane
        let mut kb_move_tab: Option<i32> = None; // Cmd+Shift+←/→: move the active tab
        let mut kb_scroll_edge: Option<bool> = None; // Shift+Home/End: scroll to top (true) / bottom
        // A hard modal (rename / paste confirm) owns the keyboard entirely: tab switching or
        // Cmd+W while a paste-confirm shows would retarget/kill the tab under the modal.
        let hard_modal = self.renaming.is_some() || !self.pending_pastes.is_empty();
        ctx.input(|i| {
            if hard_modal {
                return;
            }
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Tab) {
                kb_tab_cycle = Some(if i.modifiers.shift { -1 } else { 1 });
            }
            if i.modifiers.shift {
                if i.key_pressed(egui::Key::PageUp) {
                    kb_scroll_pages = Some(-1);
                }
                if i.key_pressed(egui::Key::PageDown) {
                    kb_scroll_pages = Some(1);
                }
                if i.key_pressed(egui::Key::Home) {
                    kb_scroll_edge = Some(true);
                }
                if i.key_pressed(egui::Key::End) {
                    kb_scroll_edge = Some(false);
                }
                if i.modifiers.command {
                    if i.key_pressed(egui::Key::ArrowLeft) {
                        kb_move_tab = Some(-1);
                    }
                    if i.key_pressed(egui::Key::ArrowRight) {
                        kb_move_tab = Some(1);
                    }
                }
            }
            if i.modifiers.command {
                use egui::Key::{
                    A, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, D, Enter, Equals, F, K, Minus,
                    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9, Plus, T, W,
                };
                if i.key_pressed(A) {
                    kb_select_all = true;
                }
                if i.key_pressed(K) {
                    kb_clear = true;
                }
                if i.key_pressed(Plus) || i.key_pressed(Equals) {
                    kb_zoom = Some(1);
                }
                if i.key_pressed(Minus) {
                    kb_zoom = Some(-1);
                }
                if i.key_pressed(Num0) {
                    kb_zoom = Some(0);
                }
                // Cmd+Alt: pane navigation / maximize (kept separate from the terminal's own
                // Cmd/Alt+arrow line/word motion, which key_to_bytes reserves against Cmd+Alt).
                if i.modifiers.alt {
                    if i.key_pressed(ArrowLeft) {
                        kb_pane_dir = Some(pane::Dir::Left);
                    }
                    if i.key_pressed(ArrowRight) {
                        kb_pane_dir = Some(pane::Dir::Right);
                    }
                    if i.key_pressed(ArrowUp) {
                        kb_pane_dir = Some(pane::Dir::Up);
                    }
                    if i.key_pressed(ArrowDown) {
                        kb_pane_dir = Some(pane::Dir::Down);
                    }
                    if i.key_pressed(Enter) {
                        kb_maximize = true;
                    }
                }
                // Cmd+Ctrl: resize the focused pane (Right/Down grow, Left/Up shrink).
                if i.modifiers.ctrl {
                    const STEP: f32 = 0.05;
                    if i.key_pressed(ArrowRight) {
                        kb_resize = Some((pane::SplitDir::Row, STEP));
                    }
                    if i.key_pressed(ArrowLeft) {
                        kb_resize = Some((pane::SplitDir::Row, -STEP));
                    }
                    if i.key_pressed(ArrowDown) {
                        kb_resize = Some((pane::SplitDir::Column, STEP));
                    }
                    if i.key_pressed(ArrowUp) {
                        kb_resize = Some((pane::SplitDir::Column, -STEP));
                    }
                }
                if i.key_pressed(T) {
                    if i.modifiers.shift {
                        kb_reopen = true; // Cmd+Shift+T
                    } else {
                        kb_new = true; // Cmd+T
                    }
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
                            tab.cli,
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
        if let Some(d) = kb_tab_cycle {
            let len = self.tabs.len() as i32;
            self.active = (self.active as i32 + d).rem_euclid(len) as usize;
        }
        if let Some(d) = kb_move_tab {
            self.move_tab(self.active, d);
        }
        if kb_reopen {
            self.reopen_tab(&ctx);
        }
        if kb_new {
            action = Some(TabAction::New);
        }
        if kb_maximize {
            let tab = &mut self.tabs[self.active];
            tab.maximized = !tab.maximized;
        }
        if let Some((dir, delta)) = kb_resize {
            let tab = &mut self.tabs[self.active];
            let path = tab.focused.clone();
            tab.root_mut().resize_focused(&path, dir, delta);
        }
        if let Some(dir) = kb_split {
            let cwd = self.tabs[self.active].focused_term().cwd();
            let new = PtyTerm::spawn(COLS, ROWS, ctx.clone(), &spawn_opts(&self.cfg, cwd));
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
        // Font zoom (harmless anytime). Reset (0), in (1), out (-1); clamped.
        if let Some(z) = kb_zoom {
            self.zoom = match z {
                0 => 1.0,
                1 => (self.zoom * 1.1).min(3.0),
                _ => (self.zoom / 1.1).max(0.5),
            };
        }
        // Terminal input keybinds - suppressed while a text modal (find/rename) owns the keyboard.
        if self.search.is_none() && self.renaming.is_none() {
            if kb_select_all {
                self.tabs[self.active].focused_term().select_all();
            }
            if kb_clear {
                self.tabs[self.active].focused_term_mut().send(b"\x0c"); // Ctrl-L: clear
            }
            if let Some(dir) = kb_scroll_pages {
                let t = self.tabs[self.active].focused_term();
                let page = t.rows().saturating_sub(1) as i32;
                t.scroll(-dir * page); // PageUp (-1) scrolls up into history
            }
            if let Some(to_top) = kb_scroll_edge {
                let t = self.tabs[self.active].focused_term();
                if to_top {
                    let (_, history) = t.scroll_state();
                    t.scroll_to_offset(history);
                } else {
                    t.scroll_to_bottom();
                }
            }
        }
        match action {
            Some(TabAction::New) => self.new_tab(&ctx),
            Some(TabAction::Duplicate(i)) => {
                let cwd = self.tabs.get(i).and_then(|t| t.focused_term().cwd());
                let tab = spawn_tab(&self.cfg, &ctx, cwd);
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
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
            Some(TabAction::CloseOthers(i)) => self.close_tabs_where(|j| j == i, i),
            Some(TabAction::CloseRight(i)) => self.close_tabs_where(|j| j <= i, i),
            Some(TabAction::CloseLeft(i)) => self.close_tabs_where(|j| j >= i, i),
            Some(TabAction::Restart(i)) => {
                // Fresh shell in the same cwd; keep the tab's identity (title/color/rename).
                if let Some(old) = self.tabs.get(i) {
                    let cwd = old.focused_term().cwd();
                    let mut fresh = spawn_tab(&self.cfg, &ctx, cwd);
                    fresh.title.clone_from(&old.title);
                    fresh.renamed = old.renamed;
                    fresh.color = old.color;
                    self.tabs[i] = fresh;
                }
            }
            None => {}
        }

        // A text field (find bar or rename dialog) owns the keyboard: don't forward keys to the
        // pty and don't let the terminal steal egui focus back while one is open. Captured MUST be
        // sampled BEFORE the modals run this frame - else the key that closes a modal (Enter to
        // commit a rename) would leak to the shell once the modal clears its own state.
        let input_captured =
            self.search.is_some() || self.renaming.is_some() || !self.pending_pastes.is_empty();

        self.rename_window(&ctx);
        self.paste_confirm_window(&ctx);

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
                let font = egui::FontId::monospace(self.cfg.appearance.font_size * self.zoom);
                let m = ui.painter().layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
                let (cw, ch) = (m.size().x, m.size().y);
                let cursor = ui::cursor_style(&self.cfg.terminal.cursor);
                // Links are "active" (underline on hover, open on click) when enabled and the
                // configured modifier is held - default modifier "none" means plain hover, Tabby-style.
                let link_active = self.cfg.terminal.clickable_links
                    && ui::link_modifier_held(
                        ui.input(|i| i.modifiers),
                        &self.cfg.terminal.link_modifier,
                    );

                let tcfg = self.cfg.terminal.clone();
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

                // Keystrokes -> focused pane (unless a text field/modal owns the keyboard).
                if !input_captured {
                    let input = collect_input(ui, tcfg.alt_is_meta, ctrl_c && has_selection);
                    if !input.is_empty() {
                        let t = tab.focused_term_mut();
                        t.send(&input);
                        t.clear_selection();
                        t.scroll_to_bottom();
                    }
                }
                // Paste events: processed even while the paste-confirm modal is open (they queue
                // rather than vanish); only a focused TEXT FIELD (find bar / rename) owns pastes.
                if self.search.is_none() && self.renaming.is_none() {
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
                        let t = tab.focused_term_mut();
                        t.paste(&s);
                        t.clear_selection();
                        t.scroll_to_bottom();
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
                let pointer = ui.input(|i| i.pointer.hover_pos());
                let mut focus_click: Option<Vec<pane::Side>> = None;
                let mut middle_paste: Option<(Vec<pane::Side>, String)> = None;
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
                        cursor,
                        dimmed,
                        link_active,
                        blink,
                    );
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
                    }
                    // Middle-click pastes the clipboard into this pane (and focuses it).
                    if tcfg.paste_on_middle_click
                        && !input_captured
                        && resp.hovered()
                        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle))
                        && let Ok(mut cb) = arboard::Clipboard::new()
                        && let Ok(text) = cb.get_text()
                    {
                        middle_paste = Some((path.clone(), text));
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
                // Apply the middle-click paste (deferred: needs &mut past the render borrow).
                // Runs through the same normalize/trim pipeline; skips the multiline modal like
                // Tabby (middle-click paste is an X11-style immediate action).
                if let Some((p, text)) = middle_paste {
                    tab.focused.clone_from(&p);
                    let s = ui::normalize_paste(&text, tcfg.replace_newlines_on_paste);
                    let s = ui::trim_paste(&s, tcfg.trim_whitespace_on_paste);
                    if let Some(t) = tab.root_mut().leaf_at_mut(&p) {
                        t.paste(&s);
                        t.scroll_to_bottom();
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
    let args: Vec<String> = std::env::args().collect();

    // `--version` / `-V`: print and exit before touching the display (used by the brew test and
    // handy for scripts). No window is created.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("stdusk {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let cfg = Config::load();
    colors::init(colors::by_name(&cfg.appearance.theme));

    // `--screenshot PATH`: populate demo tabs, render, save the PNG, and exit. Uses eframe's
    // built-in glow-backend capture via EFRAME_SCREENSHOT_TO.
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

    let mut viewport = egui::ViewportBuilder::default()
        .with_decorations(false)
        .with_transparent(true)
        .with_inner_size(size)
        .with_position([0.0, 0.0]);
    // App/window icon (the dusk-sun prompt). macOS uses the .app bundle icon for the Dock, so
    // this mainly affects other platforms + the window itself; harmless where ignored.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/stdusk-icon.png"))
    {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow, // __screenshot capture requires the glow backend
        viewport,
        ..Default::default()
    };
    // Dock/menu-bar presence on macOS:
    //   hide_from_dock && !dock_when_visible (default): launch as a pure accessory app - no Dock
    //     icon and (per Apple) no menu bar of its own; it just drops from the top on the hotkey.
    //   hide_from_dock && dock_when_visible: launch regular, then flip to accessory whenever the
    //     window is hidden (see `set_dock_icon`) - Dock icon + real menu bar only while visible.
    //   !hide_from_dock: a normal Dock app.
    if cfg.quake.hide_from_dock && !cfg.quake.dock_when_visible {
        options.event_loop_builder = Some(Box::new(|builder| {
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
                builder.with_activation_policy(ActivationPolicy::Accessory);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = builder;
        }));
    }
    eframe::run_native(
        "stdusk",
        options,
        Box::new(move |cc| Ok(Box::new(Stdusk::new(cc, cfg, screenshot)))),
    )
}
