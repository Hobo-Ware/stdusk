//! stdusk - a quake terminal with a real GUI tab bar.
//! M0 chrome · M1 shell · M1.5 progress · M2 colors · M3 quake · M4 config · M5 tabs · M6 io · M6.5 selection.
//! The `eframe::App` loop here stays thin; tabs live in `tabs.rs`, the pane workspace in
//! `workspace.rs`, find/paste overlays in `finder.rs`, drawing widgets + pure helpers in `ui.rs`.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

mod colors;
mod config;
mod finder;
mod links;
mod osc;
mod palette;
mod pane;
mod procwatch;
mod progress;
mod search;
mod session;
mod settings;
mod shell;
mod tabs;
mod terminal;
mod themes;
mod tray;
mod ui;
mod workspace;
use config::Config;
use finder::Search;
use tabs::{Tab, TabAction, spawn_opts, spawn_tab};
use terminal::PtyTerm;
use ui::{apply_theme, basename, draw_toast, tint, toast_alpha};

const COLS: usize = 80;
const ROWS: usize = 24;

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
    palette: Option<palette::PaletteState>, // command palette (Cmd+Shift+P), None when closed
    settings_open: bool, // settings window (tab-bar gear), edits cfg live
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
            palette: None,
            settings_open: false,
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
        let mut kb_palette = false; // Cmd+Shift+P: toggle the command palette
        // A hard modal (rename / paste confirm / palette) owns the keyboard entirely: tab
        // switching or Cmd+W while a paste-confirm shows would retarget/kill the tab under it.
        let text_modal = self.renaming.is_some() || !self.pending_pastes.is_empty();
        let hard_modal = text_modal || self.palette.is_some();
        ctx.input(|i| {
            // Cmd+Shift+P toggles the palette even while it's open (it's its own dismissal),
            // but stays suppressed under the rename/paste-confirm modals.
            if !text_modal
                && i.modifiers.command
                && i.modifiers.shift
                && i.key_pressed(egui::Key::P)
            {
                kb_palette = true;
            }
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
        let (clicked, mut action) = self.tab_bar(ui);

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
        if kb_palette && self.palette.take().is_none() {
            self.palette = Some(palette::PaletteState::new());
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
        self.apply_tab_action(action, &ctx);

        // A text field (find bar, rename dialog, or command palette) owns the keyboard: don't
        // forward keys to the pty and don't let the terminal steal egui focus back while one is
        // open. Captured MUST be
        // sampled BEFORE the modals run this frame - else the key that closes a modal (Enter to
        // commit a rename) would leak to the shell once the modal clears its own state.
        let input_captured = self.search.is_some()
            || self.renaming.is_some()
            || self.palette.is_some()
            || self.settings_open
            || !self.pending_pastes.is_empty();

        self.rename_window(&ctx);
        self.paste_confirm_window(&ctx);
        self.palette_window(&ctx);
        self.settings_window(&ctx);

        // OSC 52: a shell "copy" request (from the focused pane) -> the system clipboard.
        if let Some(text) =
            self.tabs.get(self.active).and_then(|t| t.focused_term().take_clipboard())
        {
            ctx.copy_text(text);
        }

        self.find_panel(ui);

        let now = ctx.input(|i| i.time);
        let out = self.central_panel(ui, &ctx, input_captured, kb_pane_dir, now);

        // Bell: a brief translucent flash over the whole window, fading out.
        if out.bell_rang {
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
        if out.copied {
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
