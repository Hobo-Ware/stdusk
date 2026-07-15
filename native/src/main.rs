//! stdusk - a quake terminal with a real GUI tab bar.
//! M0 chrome · M1 shell · M1.5 progress · M2 colors · M3 quake · M4 theming+config.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Phosphor icon codepoints (font vendored in assets/Phosphor.ttf, MIT).
mod ph {
    pub const PLUS: &str = "\u{E3D4}";
    pub const X: &str = "\u{E4F6}";
    pub const GEAR: &str = "\u{E270}";
    pub const APP_WINDOW: &str = "\u{E5DA}";
}

mod colors;
mod config;
mod osc;
mod progress;
mod terminal;
use config::Config;
use progress::Progress;
use terminal::{GridSnap, PtyTerm};

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

/// Deferred tab mutations collected during the UI pass, applied after (avoids borrow clashes).
enum TabAction {
    New,
    Rename(usize),
    SetColor(usize, Option<egui::Color32>),
    MoveLeft(usize),
    MoveRight(usize),
    Close(usize),
}

fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".into();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// A frameless Phosphor-icon button; returns true if clicked.
fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(icon).size(18.0).color(colors::dim()))
            .frame(false),
    )
    .on_hover_text(tip)
    .clicked()
}

/// Truncate to `max` chars with an ellipsis; returns (shown, was_truncated).
fn ellipsize(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    (format!("{head}…"), true)
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
}

impl Stdusk {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
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
        Self {
            tabs: vec![spawn_tab(&cc.egui_ctx, detect, None)],
            active: 0,
            cfg,
            _hotkey: mgr,
            toggle,
            visible: true,
            was_focused: false, // arm hide-on-blur only after the first focus gain
            sized: false,
            renaming: None,
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
        let y = mon.map(|m| m.y - 2.0).unwrap_or(2000.0);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, y)));
    }
}

/// Apply window opacity to a fill color (straight alpha).
fn tint(c: egui::Color32, opacity: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = colors::bg();
    v.window_fill = colors::bg();
    v.override_text_color = Some(colors::fg());
    ctx.set_visuals(v);
}

/// Flat Tabby-style tab: dark bg (elevated when active), optional per-tab colored underline,
/// and progress rendered as a thin bar on the TOP edge.
fn draw_tab(
    ui: &mut egui::Ui,
    idx: usize,
    title: &str,
    active: bool,
    color: Option<egui::Color32>,
    progress: Progress,
) -> (egui::Response, bool) {
    let (shown, truncated) = ellipsize(title, 14);
    let fg = if active { colors::fg() } else { colors::dim() };
    // trailing spaces reserve room for the close x
    let mut rt = egui::RichText::new(format!("{idx}  {shown}   ")).color(fg).monospace();
    if active {
        rt = rt.strong();
    }
    let fill = if active {
        colors::elevated()
    } else {
        egui::Color32::TRANSPARENT
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }) // top-rounded tab shape
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            let lbl = ui.add(egui::Label::new(rt).selectable(false));
            if truncated {
                lbl.on_hover_text(title);
            }
        });
    let rect = inner.response.rect;
    // Per-tab color underline (bottom edge) - only when the user set a color.
    if let Some(color) = color {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() - 2.0),
                egui::vec2(rect.width(), 2.0),
            ),
            0.0,
            color,
        );
    }
    // Progress bar (top edge).
    if let Some((frac, pcolor)) = progress_bar(progress) {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top()),
                egui::vec2(rect.width() * frac, 2.0),
            ),
            0.0,
            pcolor,
        );
    }
    // Close x, revealed on the active or hovered tab.
    let mut close = false;
    if active || inner.response.hovered() {
        let x_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 20.0, rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let x = ui.put(
            x_rect,
            egui::Button::new(egui::RichText::new(ph::X).size(13.0).color(colors::dim()))
                .frame(false),
        );
        if x.clicked() {
            close = true;
        }
    }
    (inner.response.interact(egui::Sense::click()), close)
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
                if ui
                    .button(egui::RichText::new("⬤").color(col))
                    .clicked()
                {
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

/// (fill fraction 0..1, color) for the tab progress bar, or None to hide it.
fn progress_bar(p: Progress) -> Option<(f32, egui::Color32)> {
    match p {
        Progress::None => None,
        Progress::Normal(v) => Some((v as f32 / 100.0, colors::green())),
        Progress::Paused(v) => Some((v as f32 / 100.0, colors::yellow())),
        Progress::Error(_) => Some((1.0, colors::red())),
        Progress::Indeterminate => Some((1.0, colors::accent())),
    }
}

/// Translate this frame's key/text events into bytes for the pty.
fn collect_input(ui: &egui::Ui) -> Vec<u8> {
    let mut out = Vec::new();
    ui.input(|i| {
        for event in &i.events {
            match event {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    use egui::Key;
                    if modifiers.ctrl {
                        if let Some(off) = ctrl_letter(*key) {
                            out.push(off);
                        }
                        return;
                    }
                    match key {
                        Key::Enter => out.push(b'\r'),
                        Key::Backspace => out.push(0x7f),
                        Key::Tab => out.push(b'\t'),
                        Key::Escape => out.push(0x1b),
                        Key::ArrowUp => out.extend_from_slice(b"\x1b[A"),
                        Key::ArrowDown => out.extend_from_slice(b"\x1b[B"),
                        Key::ArrowRight => out.extend_from_slice(b"\x1b[C"),
                        Key::ArrowLeft => out.extend_from_slice(b"\x1b[D"),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    });
    out
}

fn ctrl_letter(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    let n = match key {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8, I => 9,
        J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16, Q => 17,
        R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24, Y => 25, Z => 26,
        _ => return None,
    };
    Some(n)
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

        // Auto-title unrenamed tabs from their cwd (basename).
        for tab in &mut self.tabs {
            if !tab.renamed {
                if let Some(c) = tab.term.cwd() {
                    tab.title = basename(&c);
                }
            }
        }

        // Browser-style keybinds: Cmd+T new, Cmd+W close, Cmd+1..9 switch.
        let mut kb_new = false;
        let mut kb_close = false;
        let mut kb_switch: Option<usize> = None;
        ctx.input(|i| {
            if i.modifiers.command {
                use egui::Key::*;
                if i.key_pressed(T) {
                    kb_new = true;
                }
                if i.key_pressed(W) {
                    kb_close = true;
                }
                for (n, k) in [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9]
                    .into_iter()
                    .enumerate()
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
        ui.painter()
            .rect_filled(ui.max_rect(), 10.0, tint(colors::bg(), opacity));

        // Tab bar. Collect clicks + menu actions; apply after the panel to avoid borrow clashes.
        let mut clicked: Option<usize> = None;
        let mut action: Option<TabAction> = None;
        egui::Panel::top("tabbar")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Gear pinned to the far right; everything else flows from the left.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, ph::GEAR, "Settings (config.toml)") {
                            if let Some(p) = config::ensure_and_path() {
                                let _ = std::process::Command::new("open").arg(p).spawn();
                            }
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
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
                            ui.add_space(4.0);
                            if icon_button(ui, ph::PLUS, "New tab") {
                                action = Some(TabAction::New);
                            }
                            ui.menu_button(
                                egui::RichText::new(ph::APP_WINDOW).size(18.0).color(colors::dim()),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new("Tabs").small().color(colors::dim()),
                                    );
                                    for (i, tab) in self.tabs.iter().enumerate() {
                                        if ui.button(format!("{}  {}", i + 1, tab.title)).clicked() {
                                            clicked = Some(i);
                                        }
                                    }
                                },
                            );
                        });
                    });
                });
            });

        // Apply tab-bar clicks + keybinds + menu action (all structural mutations here).
        if let Some(i) = clicked {
            self.active = i;
        }
        if let Some(n) = kb_switch {
            if n < self.tabs.len() {
                self.active = n;
            }
        }
        if kb_new {
            action = Some(TabAction::New);
        }
        if kb_close {
            action = Some(TabAction::Close(self.active));
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

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let area = ui.max_rect();
                let term = &mut self.tabs[self.active].term;

                // Keystrokes -> pty; typing jumps back to the live bottom.
                let input = collect_input(ui);
                if !input.is_empty() {
                    term.send(&input);
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
                    term.scroll_to_bottom();
                }

                // Cell metrics + wheel scrollback.
                let font = egui::FontId::monospace(self.cfg.appearance.font_size);
                let m = ui
                    .painter()
                    .layout_no_wrap("M".to_owned(), font.clone(), colors::fg());
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
                render_grid(ui, &snap, cw, ch, &font);

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
                    if resp.dragged() || resp.clicked() {
                        if let Some(p) = resp.interact_pointer_pos() {
                            let frac = ((p.y - area.top()) / track).clamp(0.0, 1.0);
                            let tgt = ((1.0 - frac) * history as f32).round() as usize;
                            term.scroll_to_offset(tgt.min(history));
                        }
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
    }
}

/// Paint the terminal grid: per-cell bg rects + fg glyphs + a beam cursor.
fn render_grid(ui: &mut egui::Ui, snap: &GridSnap, cw: f32, ch: f32, font: &egui::FontId) {
    let size = egui::vec2(cw * snap.cols as f32, ch * snap.rows as f32);
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let origin = resp.rect.min;

    for r in 0..snap.rows {
        for c in 0..snap.cols {
            let cell = &snap.cells[r * snap.cols + c];
            let pos = origin + egui::vec2(c as f32 * cw, r as f32 * ch);
            if let Some(bg) = cell.bg {
                painter.rect_filled(egui::Rect::from_min_size(pos, egui::vec2(cw, ch)), 0.0, bg);
            }
            if cell.c != ' ' && cell.c != '\0' {
                painter.text(pos, egui::Align2::LEFT_TOP, cell.c, font.clone(), cell.fg);
            }
        }
    }

    // Beam cursor (block/underline styles land in M9); hidden while scrolled into history.
    if let Some((cr, cc)) = snap.cursor {
        let cpos = origin + egui::vec2(cc as f32 * cw, cr as f32 * ch);
        painter.rect_filled(
            egui::Rect::from_min_size(cpos, egui::vec2(2.0, ch)),
            0.0,
            colors::cursor(),
        );
    }
}

fn main() -> eframe::Result<()> {
    let cfg = Config::load();
    colors::init(colors::by_name(&cfg.appearance.theme));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_transparent(true)
            .with_inner_size([1200.0, 500.0])
            .with_position([0.0, 0.0]),
        ..Default::default()
    };
    eframe::run_native(
        "stdusk",
        options,
        Box::new(move |cc| Ok(Box::new(Stdusk::new(cc, cfg)))),
    )
}
