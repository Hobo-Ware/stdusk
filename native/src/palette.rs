//! Command palette (Cmd+Shift+P): a fuzzy-searchable list of app commands, shown as a
//! centered overlay near the top. Filtering/scoring are pure functions, unit-tested here.

use eframe::egui;

use crate::finder::Search;
use crate::tabs::{TabAction, spawn_opts};
use crate::terminal::PtyTerm;
use crate::ui;
use crate::{COLS, ROWS, Stdusk, colors, config, pane, search};

const MAX_ROWS: usize = 8;
const ROW_W: f32 = 320.0;
const ROW_H: f32 = 26.0;

/// Command-palette overlay state, `Some` while open (mirrors `Search` / `renaming`).
pub(crate) struct PaletteState {
    pub(crate) query: String,
    pub(crate) selected: usize,
    pub(crate) focus: bool, // request text-field focus once on open (re-requesting breaks Enter)
}

impl PaletteState {
    pub(crate) fn new() -> Self {
        Self { query: String::new(), selected: 0, focus: true }
    }
}

/// Every command the palette can run. Enum order is the tie-break order in the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaletteCmd {
    NewTab,
    CloseTab,
    DuplicateTab,
    ReopenTab,
    RestartTab,
    RenameTab,
    SplitRight,
    SplitDown,
    MaximizePane,
    NextTab,
    PrevTab,
    Find,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ClearTerminal,
    CopyPath,
    OpenConfig,
    Quit,
}

impl PaletteCmd {
    const ALL: [Self; 19] = [
        Self::NewTab,
        Self::CloseTab,
        Self::DuplicateTab,
        Self::ReopenTab,
        Self::RestartTab,
        Self::RenameTab,
        Self::SplitRight,
        Self::SplitDown,
        Self::MaximizePane,
        Self::NextTab,
        Self::PrevTab,
        Self::Find,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ZoomReset,
        Self::ClearTerminal,
        Self::CopyPath,
        Self::OpenConfig,
        Self::Quit,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NewTab => "New Tab",
            Self::CloseTab => "Close Tab",
            Self::DuplicateTab => "Duplicate Tab",
            Self::ReopenTab => "Reopen Closed Tab",
            Self::RestartTab => "Restart Tab",
            Self::RenameTab => "Rename Tab",
            Self::SplitRight => "Split Right",
            Self::SplitDown => "Split Down",
            Self::MaximizePane => "Maximize Pane",
            Self::NextTab => "Next Tab",
            Self::PrevTab => "Previous Tab",
            Self::Find => "Find",
            Self::ZoomIn => "Zoom In",
            Self::ZoomOut => "Zoom Out",
            Self::ZoomReset => "Zoom Reset",
            Self::ClearTerminal => "Clear Terminal",
            Self::CopyPath => "Copy Path",
            Self::OpenConfig => "Open Config",
            Self::Quit => "Quit",
        }
    }
}

/// Case-insensitive subsequence match; `None` when `query` isn't a subsequence of `label`.
/// Score favors word-initial matches (start / after a space), contiguous runs, and earlier
/// positions. Empty query matches everything with score 0.
fn fuzzy_match(query: &str, label: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let l: Vec<char> = label.chars().collect();
    let mut score = 0i64;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (li, &c) in l.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if c.to_ascii_lowercase() == q[qi] {
            let word_start = li == 0 || l[li - 1] == ' ';
            let contiguous = li > 0 && prev == Some(li - 1);
            score += 1 - li as i64; // earlier matches rank higher
            if word_start {
                score += 8;
            }
            if contiguous {
                score += 4;
            }
            prev = Some(li);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// All commands matching `query`, best score first; ties keep enum order (stable sort).
fn filter_commands(query: &str) -> Vec<PaletteCmd> {
    let mut scored: Vec<(i64, PaletteCmd)> = PaletteCmd::ALL
        .iter()
        .filter_map(|&c| fuzzy_match(query, c.label()).map(|s| (s, c)))
        .collect();
    scored.sort_by_key(|&(s, _)| std::cmp::Reverse(s));
    scored.into_iter().map(|(_, c)| c).collect()
}

impl Stdusk {
    /// The palette overlay, shown while `self.palette` is set. Up/Down move the selection,
    /// Enter (or a row click) runs the command and closes, Esc closes.
    pub(crate) fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.palette.take() else {
            return;
        };
        // Keys read directly (the palette is a hard modal, nothing else sees them). TextEdit
        // doesn't consume them from InputState, so arrows work while the field is focused.
        let (up, down, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let mut chosen: Option<PaletteCmd> = None;
        egui::Window::new("Command palette")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(ui::overlay_frame())
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 64.0))
            .show(ctx, |ui| {
                let r = ui::text_field(ui, &mut st.query, "Type a command", ROW_W, colors::fg());
                // Focus ONCE on open. Re-requesting every frame would stop egui from ever
                // reporting lost_focus (same bug as rename/find - see ui.md).
                if st.focus {
                    r.request_focus();
                    st.focus = false;
                }
                if r.changed() {
                    st.selected = 0;
                }
                let filtered = filter_commands(&st.query);
                let shown = filtered.len().min(MAX_ROWS);
                ui.add_space(6.0);
                if shown == 0 {
                    ui.label(egui::RichText::new("No matching commands").color(colors::dim()));
                    return;
                }
                st.selected = st.selected.min(shown - 1);
                if down {
                    st.selected = (st.selected + 1) % shown;
                }
                if up {
                    st.selected = (st.selected + shown - 1) % shown;
                }
                if enter {
                    chosen = Some(filtered[st.selected]);
                }
                for (i, &cmd) in filtered.iter().take(shown).enumerate() {
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(ROW_W, ROW_H), egui::Sense::click());
                    if i == st.selected {
                        ui.painter().rect_filled(rect, 6.0, colors::selection());
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 6.0, colors::hover());
                    }
                    ui.painter().text(
                        rect.left_center() + egui::vec2(8.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        cmd.label(),
                        egui::FontId::proportional(15.0),
                        colors::fg(),
                    );
                    if resp.clicked() {
                        chosen = Some(cmd);
                    }
                }
            });
        if let Some(cmd) = chosen {
            self.run(cmd, ctx);
        } else if !esc {
            self.palette = Some(st); // keep it open next frame
        }
    }

    /// Execute a palette command, reusing the existing tab/pane/find/zoom paths.
    fn run(&mut self, cmd: PaletteCmd, ctx: &egui::Context) {
        match cmd {
            PaletteCmd::NewTab => self.apply_tab_action(Some(TabAction::New), ctx),
            PaletteCmd::CloseTab => self.apply_tab_action(Some(TabAction::Close(self.active)), ctx),
            PaletteCmd::DuplicateTab => {
                self.apply_tab_action(Some(TabAction::Duplicate(self.active)), ctx);
            }
            PaletteCmd::ReopenTab => self.reopen_tab(ctx),
            PaletteCmd::RestartTab => {
                self.apply_tab_action(Some(TabAction::Restart(self.active)), ctx);
            }
            PaletteCmd::RenameTab => {
                self.apply_tab_action(Some(TabAction::Rename(self.active)), ctx);
            }
            PaletteCmd::SplitRight => self.split_focused(pane::SplitDir::Row, ctx),
            PaletteCmd::SplitDown => self.split_focused(pane::SplitDir::Column, ctx),
            PaletteCmd::MaximizePane => {
                let tab = &mut self.tabs[self.active];
                tab.maximized = !tab.maximized;
            }
            PaletteCmd::NextTab => self.cycle_tab(1),
            PaletteCmd::PrevTab => self.cycle_tab(-1),
            PaletteCmd::Find => {
                if self.search.is_none() {
                    self.search = Some(Search {
                        query: String::new(),
                        matches: Vec::new(),
                        current: 0,
                        focus: true,
                        opts: search::SearchOpts::default(),
                    });
                }
            }
            PaletteCmd::ZoomIn => self.zoom = (self.zoom * 1.1).min(3.0),
            PaletteCmd::ZoomOut => self.zoom = (self.zoom / 1.1).max(0.5),
            PaletteCmd::ZoomReset => self.zoom = 1.0,
            PaletteCmd::ClearTerminal => {
                self.tabs[self.active].focused_term_mut().send(b"\x0c"); // Ctrl-L: clear
            }
            PaletteCmd::CopyPath => {
                if let Some(cwd) = self.tabs[self.active].focused_term().cwd() {
                    ctx.copy_text(cwd);
                    let now = ctx.input(|i| i.time);
                    self.toast = Some(("Copied".into(), now + 1.4));
                }
            }
            PaletteCmd::OpenConfig => {
                if let Some(p) = config::ensure_and_path() {
                    let _ = std::process::Command::new("open").arg(p).spawn();
                }
            }
            PaletteCmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// Split the focused pane (same transform as the Cmd+D / Cmd+Shift+D keybind).
    fn split_focused(&mut self, dir: pane::SplitDir, ctx: &egui::Context) {
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

    /// Cycle the active tab by `d` with wraparound (same as Ctrl+Tab / Ctrl+Shift+Tab).
    fn cycle_tab(&mut self, d: i32) {
        let len = self.tabs.len() as i32;
        self.active = (self.active as i32 + d).rem_euclid(len) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_subsequence_matches() {
        assert!(fuzzy_match("spr", "Split Right").is_some());
        assert!(fuzzy_match("SPLIT", "Split Right").is_some()); // case-insensitive
        assert!(fuzzy_match("xyz", "Split Right").is_none());
        assert!(fuzzy_match("tabn", "New Tab").is_none()); // order matters
    }

    #[test]
    fn fuzzy_empty_query_matches_with_zero() {
        assert_eq!(fuzzy_match("", "New Tab"), Some(0));
    }

    #[test]
    fn fuzzy_word_initials_rank_higher() {
        // "nt" hits both word starts of "New Tab" but lands mid-word in "Rename Tab".
        assert!(fuzzy_match("nt", "New Tab").unwrap() > fuzzy_match("nt", "Rename Tab").unwrap());
    }

    #[test]
    fn fuzzy_contiguous_early_ranks_higher() {
        // "re" is a contiguous word-start run in "Rename Tab" but scattered mid-word
        // (r in Clear, e in Terminal) in "Clear Terminal".
        assert!(
            fuzzy_match("re", "Rename Tab").unwrap() > fuzzy_match("re", "Clear Terminal").unwrap()
        );
    }

    #[test]
    fn filter_empty_query_lists_all_in_enum_order() {
        assert_eq!(filter_commands(""), PaletteCmd::ALL.to_vec());
    }

    #[test]
    fn filter_ranks_new_tab_first_for_nt() {
        let f = filter_commands("nt");
        assert_eq!(f[0], PaletteCmd::NewTab);
        assert!(f.contains(&PaletteCmd::RenameTab)); // still matches, just lower
    }

    #[test]
    fn filter_drops_non_matches() {
        assert!(filter_commands("zzzz").is_empty());
    }

    #[test]
    fn filter_ties_keep_enum_order() {
        assert_eq!(filter_commands("split"), vec![PaletteCmd::SplitRight, PaletteCmd::SplitDown]);
    }
}
