//! Tabs: the `Tab` model + spawn helpers, the tab-bar panel, the right-click tab
//! menu, and Stdusk's tab-management methods (new/close/move/reopen/rename/apply).

use std::sync::atomic::Ordering;

use eframe::egui;

use crate::config::{Config, Profile};
use crate::terminal::{self, PtyTerm};
use crate::ui::{self, ICON_BTN_W, color_swatch, draw_tab, icon_button, icons, style_menu, tint};
use crate::{COLS, ROWS, Stdusk, colors, pane, procwatch, session};

/// Fixed tab-bar row height - keeps every control centered on the same line (no drift).
const TABBAR_ROW_H: f32 = 34.0;

/// Monotonic tab identity - stable across reorders/closes (used to target deferred actions).
static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) struct Tab {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) color: Option<egui::Color32>, // None = no underline (Tabby default); set via the menu
    pub(crate) renamed: bool,                // once renamed, stop auto-titling from cwd
    pub(crate) root: Option<pane::Pane<PtyTerm>>, // Option so whole-tree transforms can `take()` it
    pub(crate) focused: Vec<pane::Side>,     // path to the focused leaf (its identity)
    pub(crate) cli: Option<procwatch::Cli>,  // a known AI CLI detected running in the tab (badge)
    pub(crate) maximized: bool, // zoom the focused pane to fill the tab (hide the other panes)
}

impl Tab {
    pub(crate) fn root(&self) -> &pane::Pane<PtyTerm> {
        self.root.as_ref().expect("pane root")
    }
    pub(crate) fn root_mut(&mut self) -> &mut pane::Pane<PtyTerm> {
        self.root.as_mut().expect("pane root")
    }
    pub(crate) fn focused_term(&self) -> &PtyTerm {
        self.root().leaf_at(&self.focused).expect("focused leaf")
    }
    pub(crate) fn focused_term_mut(&mut self) -> &mut PtyTerm {
        let path = self.focused.clone();
        self.root_mut().leaf_at_mut(&path).expect("focused leaf")
    }
}

/// Bundle the config bits a terminal spawn needs.
pub(crate) fn spawn_opts(cfg: &Config, cwd: Option<String>) -> terminal::SpawnOpts {
    terminal::SpawnOpts {
        detect_progress: cfg.terminal.detect_progress,
        profile: None,
        shell_integration: cfg.terminal.shell_integration,
        scrollback_lines: cfg.terminal.scrollback_lines,
        word_separators: cfg.terminal.word_separators.clone(),
        bold_bright: cfg.terminal.bold_bright,
        cwd,
    }
}

pub(crate) fn spawn_tab(cfg: &Config, ctx: &egui::Context, cwd: Option<String>) -> Tab {
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

/// Spawn a tab from a launch profile: shell/args/cwd/env overrides, titled after the profile
/// (renamed, so cwd auto-titling stays off) and colored per its `color`.
pub(crate) fn spawn_profile_tab(cfg: &Config, ctx: &egui::Context, profile: &Profile) -> Tab {
    let mut opts = spawn_opts(cfg, None);
    opts.profile = Some(profile.clone());
    let term = PtyTerm::spawn(COLS, ROWS, ctx.clone(), &opts);
    Tab {
        id: NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed),
        title: profile.name.clone(),
        color: profile.color.as_deref().and_then(session::hex_to_color),
        renamed: true,
        root: Some(pane::Pane::leaf(term)),
        focused: Vec::new(),
        cli: None,
        maximized: false,
    }
}

/// Deferred tab mutations collected during the UI pass, applied after (avoids borrow clashes).
pub(crate) enum TabAction {
    New,
    NewWithProfile(usize), // index into cfg.profiles
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

/// While dragging the tab at `from`, the neighbor to swap with once the pointer's x
/// crosses that neighbor's horizontal midpoint (`None` = keep position). Uses actual
/// rects so it stays correct with mixed tab widths.
fn drag_swap_target(rects: &[egui::Rect], from: usize, pointer_x: f32) -> Option<usize> {
    if from + 1 < rects.len() && pointer_x > rects[from + 1].center().x {
        return Some(from + 1);
    }
    if from > 0 && pointer_x < rects[from - 1].center().x {
        return Some(from - 1);
    }
    None
}

/// One menu row per profile, each spawning a tab with it. Shared by the tab context menu's
/// submenu and the "+" button's right-click menu.
fn profile_menu_rows(ui: &mut egui::Ui, profiles: &[Profile], action: &mut Option<TabAction>) {
    for (pi, p) in profiles.iter().enumerate() {
        if ui.button(&p.name).clicked() {
            *action = Some(TabAction::NewWithProfile(pi));
        }
    }
}

/// Right-click tab context menu. Sets `action`; egui auto-closes the menu on any button click.
fn tab_menu(
    ui: &mut egui::Ui,
    i: usize,
    current: Option<egui::Color32>,
    profiles: &[Profile],
    action: &mut Option<TabAction>,
) {
    style_menu(ui);
    if ui.button("New tab").clicked() {
        *action = Some(TabAction::New);
    }
    if !profiles.is_empty() {
        ui.menu_button("New tab with profile", |ui| {
            style_menu(ui);
            profile_menu_rows(ui, profiles, action);
        });
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

impl Stdusk {
    pub(crate) fn new_tab(&mut self, ctx: &egui::Context) {
        let cwd = self.tabs.get(self.active).and_then(|t| t.focused_term().cwd());
        let tab = spawn_tab(&self.cfg, ctx, cwd);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    pub(crate) fn close_tab(&mut self, i: usize, ctx: &egui::Context) {
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
    pub(crate) fn reopen_tab(&mut self, ctx: &egui::Context) {
        if let Some(cwd) = self.closed.pop() {
            let tab = spawn_tab(&self.cfg, ctx, Some(cwd));
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
    }

    pub(crate) fn move_tab(&mut self, i: usize, dir: i32) {
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
    pub(crate) fn rename_window(&mut self, ctx: &egui::Context) {
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

    /// The tab-bar panel. Collects tab clicks + menu actions for the caller to apply
    /// after the panel (avoids borrow clashes). Returns (clicked tab, deferred action).
    pub(crate) fn tab_bar(&mut self, ui: &mut egui::Ui) -> (Option<usize>, Option<TabAction>) {
        let opacity = self.cfg.appearance.opacity;
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
                    // Drag-to-reorder state, derived per frame (nothing persists): the tab
                    // whose drag response is active + the pointer x, plus every tab's rect.
                    let mut rects: Vec<egui::Rect> = Vec::with_capacity(self.tabs.len());
                    let mut drag: Option<(usize, f32)> = None;
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
                        // Drag sense layered over the tab (draw_tab's response is click-only,
                        // and ui.rs is off-limits). Drag-only widgets don't take clicks, so
                        // click / double-click / close-x / context menu keep working. Id is
                        // seeded from the stable tab id, not the index, so egui keeps tracking
                        // the same drag across swaps (ui.md).
                        let dr = ui.interact(
                            resp.rect,
                            ui.id().with(("tab_drag", tab.id)),
                            egui::Sense::drag(),
                        );
                        // A drag-only sense reports dragged() on bare press; gate on egui's
                        // decided-drag threshold so a plain click never reorders.
                        if dr.dragged()
                            && ui.input(|inp| inp.pointer.is_decidedly_dragging())
                            && let Some(p) = dr.interact_pointer_pos()
                        {
                            drag = Some((i, p.x));
                        }
                        rects.push(resp.rect);
                        resp.context_menu(|ui| {
                            tab_menu(ui, i, tab_color, &self.cfg.profiles, &mut action);
                        });
                    }
                    // Apply the drag AFTER the loop (deferred, like every other TabAction):
                    // crossing a neighbor's midpoint emits one move; re-derived every frame,
                    // so dragging across several tabs reorders repeatedly.
                    if let Some((from, px)) = drag {
                        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
                        // Overlay on the deco layer - the row clip would cut ui.painter().
                        let dp = ui.ctx().layer_painter(egui::LayerId::new(
                            egui::Order::Middle,
                            egui::Id::new("tab_drag_overlay"),
                        ));
                        dp.rect_filled(
                            rects[from],
                            egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                            colors::selection(),
                        );
                        if action.is_none()
                            && let Some(to) = drag_swap_target(&rects, from, px)
                        {
                            action = Some(if to < from {
                                TabAction::MoveLeft(from)
                            } else {
                                TabAction::MoveRight(from)
                            });
                        }
                    }
                    ui.add_space(6.0);
                    let plus = icon_button(ui, icons::PLUS, "New tab");
                    if plus.clicked() {
                        action = Some(TabAction::New);
                    }
                    // Right-click the "+" for a per-profile spawn menu (only when configured).
                    if !self.cfg.profiles.is_empty() {
                        plus.context_menu(|ui| {
                            style_menu(ui);
                            profile_menu_rows(ui, &self.cfg.profiles, &mut action);
                        });
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
                    if icon_button(ui, icons::GEAR, "Settings").clicked() {
                        self.settings_open = !self.settings_open;
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
        (clicked, action)
    }

    /// Apply a deferred tab action (from the tab bar, keybinds, or a context menu).
    #[allow(clippy::needless_pass_by_value)] // consumes the frame's deferred action by design
    pub(crate) fn apply_tab_action(&mut self, action: Option<TabAction>, ctx: &egui::Context) {
        match action {
            Some(TabAction::New) => self.new_tab(ctx),
            Some(TabAction::NewWithProfile(pi)) => {
                if let Some(p) = self.cfg.profiles.get(pi).cloned() {
                    let tab = spawn_profile_tab(&self.cfg, ctx, &p);
                    self.tabs.push(tab);
                    self.active = self.tabs.len() - 1;
                }
            }
            Some(TabAction::Duplicate(i)) => {
                let cwd = self.tabs.get(i).and_then(|t| t.focused_term().cwd());
                let tab = spawn_tab(&self.cfg, ctx, cwd);
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
            Some(TabAction::Close(i)) => self.close_tab(i, ctx),
            Some(TabAction::CloseOthers(i)) => self.close_tabs_where(|j| j == i, i),
            Some(TabAction::CloseRight(i)) => self.close_tabs_where(|j| j <= i, i),
            Some(TabAction::CloseLeft(i)) => self.close_tabs_where(|j| j >= i, i),
            Some(TabAction::Restart(i)) => {
                // Fresh shell in the same cwd; keep the tab's identity (title/color/rename).
                if let Some(old) = self.tabs.get(i) {
                    let cwd = old.focused_term().cwd();
                    let mut fresh = spawn_tab(&self.cfg, ctx, cwd);
                    fresh.title.clone_from(&old.title);
                    fresh.renamed = old.renamed;
                    fresh.color = old.color;
                    self.tabs[i] = fresh;
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay tabs left-to-right with the given widths (mixed widths on purpose).
    fn rects(widths: &[f32]) -> Vec<egui::Rect> {
        let mut x = 0.0;
        widths
            .iter()
            .map(|&w| {
                let r = egui::Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(w, 30.0));
                x += w;
                r
            })
            .collect()
    }

    #[test]
    fn drag_swaps_only_past_neighbor_midpoint() {
        // Tabs: [0,100) [100,200) [200,300) - midpoints 50 / 150 / 250.
        let r = rects(&[100.0, 100.0, 100.0]);
        let cases = [
            (1, 251.0, Some(2)), // crossed right neighbor's midpoint
            (1, 49.0, Some(0)),  // crossed left neighbor's midpoint
            (1, 120.0, None),    // still inside own rect
            (1, 249.0, None),    // over the neighbor but short of its midpoint
            (1, 51.0, None),
            (0, 151.0, Some(1)), // leftmost can only go right
            (0, -50.0, None),    // no left neighbor
            (2, 149.0, Some(1)), // rightmost can only go left
            (2, 1000.0, None),   // no right neighbor
        ];
        for (from, px, want) in cases {
            assert_eq!(drag_swap_target(&r, from, px), want, "from {from} at x={px}");
        }
    }

    #[test]
    fn drag_uses_actual_rects_for_mixed_widths() {
        // Tabs: [0,60) [60,260) [260,340) - midpoints 30 / 160 / 300.
        let r = rects(&[60.0, 200.0, 80.0]);
        assert_eq!(drag_swap_target(&r, 0, 159.0), None); // wide neighbor: midpoint is far
        assert_eq!(drag_swap_target(&r, 0, 161.0), Some(1));
        assert_eq!(drag_swap_target(&r, 2, 161.0), None);
        assert_eq!(drag_swap_target(&r, 2, 159.0), Some(1));
    }
}
