//! Repo groups (`appearance.group_by_repo`): every tab belongs to the git repo it was opened in,
//! and the tab bar shows one repo's tabs at a time behind a repo chip. A tab follows its shell: a
//! `cd` into another repo moves it there, and a folder outside any repo moves it to `Other`.

use std::path::{Component, Path, PathBuf};

use eframe::egui;

use crate::progress::Progress;
use crate::terminal::CmdState;
use crate::ui::{self, icons};
use crate::{Stdusk, colors, tabs};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    Repo(PathBuf),
    Other,
}

impl Group {
    pub(crate) fn for_cwd(cwd: Option<&str>) -> Self {
        cwd.and_then(|c| repo_root(Path::new(c))).map_or(Self::Other, Self::Repo)
    }

    /// Session encoding: the repo root path, `None` for `Other`.
    pub(crate) fn to_saved(&self) -> Option<String> {
        match self {
            Self::Repo(p) => Some(p.to_string_lossy().into_owned()),
            Self::Other => None,
        }
    }
}

/// Git writes a linked worktree's .git as a file pointing into the main repo's .git/worktrees
/// folder, so worktrees fold into their main repo. Any other .git file (a submodule) makes that
/// folder its own repo.
pub(crate) fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find_map(|d| {
        let dot = d.join(".git");
        if dot.is_dir() {
            Some(d.to_path_buf())
        } else if dot.is_file() {
            Some(worktree_main(d, &dot).unwrap_or_else(|| d.to_path_buf()))
        } else {
            None
        }
    })
}

fn worktree_main(dir: &Path, dot_git: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = lexical_normalize(&dir.join(gitdir));
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    worktrees.parent()?.parent().map(Path::to_path_buf)
}

/// Resolve `.`/`..` without touching the disk: `canonicalize` would turn `/var` into
/// `/private/var` on macOS and split one repo into two groups.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The group a tab moves to once its cwd is known and differs from the one last checked.
pub(crate) fn regroup(cwd: Option<&str>, probed: Option<&str>) -> Option<Group> {
    let cwd = cwd?;
    (Some(cwd) != probed).then(|| Group::for_cwd(Some(cwd)))
}

/// Distinct groups in first-appearance order, with `Other` always last.
pub(crate) fn order<'a>(groups: impl IntoIterator<Item = &'a Group>) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for g in groups {
        if !out.contains(g) {
            out.push(g.clone());
        }
    }
    out.sort_by_key(|g| *g == Group::Other);
    out
}

/// Display names, parallel to `groups`: the repo folder's name, prefixed by its parent folder
/// when two repos share a name.
pub(crate) fn labels(groups: &[Group]) -> Vec<String> {
    let name = |p: &Path| {
        p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
    };
    groups
        .iter()
        .map(|g| match g {
            Group::Other => "Other".into(),
            Group::Repo(p) => {
                let clash = groups.iter().any(
                    |o| matches!(o, Group::Repo(q) if q != p && q.file_name() == p.file_name()),
                );
                match p.parent().filter(|_| clash) {
                    Some(parent) => format!("{}/{}", name(parent), name(p)),
                    None => name(p),
                }
            }
        })
        .collect()
}

/// Indices of the tabs a group shows, in tab order.
pub(crate) fn members(groups: &[&Group], g: &Group) -> Vec<usize> {
    groups.iter().enumerate().filter(|(_, t)| **t == g).map(|(i, _)| i).collect()
}

/// The tab to land on when switching to `g`: its most recently focused tab, else its first.
pub(crate) fn landing_tab(tabs: &[(u64, &Group)], history: &[u64], g: &Group) -> Option<usize> {
    let in_group = |i: &usize| tabs[*i].1 == g;
    history
        .iter()
        .find_map(|id| tabs.iter().position(|(t, _)| t == id).filter(in_group))
        .or_else(|| (0..tabs.len()).find(in_group))
}

/// The group `d` steps from `cur` in `order`, wrapping.
pub(crate) fn step(order: &[Group], cur: &Group, d: i32) -> Option<Group> {
    let len = order.len() as i32;
    let at = order.iter().position(|g| g == cur)? as i32;
    order.get((at + d).rem_euclid(len) as usize).cloned()
}

/// The focus history with `g`'s tabs moved to the front (order kept), so closing a tab lands on
/// another tab of the same repo before falling back to a different repo.
pub(crate) fn history_preferring(tabs: &[(u64, &Group)], history: &[u64], g: &Group) -> Vec<u64> {
    let in_group = |id: &u64| tabs.iter().any(|(t, tg)| t == id && *tg == g);
    let (mut first, rest): (Vec<u64>, Vec<u64>) = history.iter().partition(|id| in_group(id));
    first.extend(rest);
    first
}

/// The member of `members` that `d` steps from `active`, wrapping; `active` itself when it is
/// not a member.
pub(crate) fn step_within(members: &[usize], active: usize, d: i32) -> usize {
    let Some(at) = members.iter().position(|&i| i == active) else { return active };
    let len = members.len() as i32;
    members[(at as i32 + d).rem_euclid(len) as usize]
}

/// The member next to `i` in direction `dir`, without wrapping.
pub(crate) fn neighbor(members: &[usize], i: usize, dir: i32) -> Option<usize> {
    let at = members.iter().position(|&m| m == i)? as i32 + dir;
    usize::try_from(at).ok().and_then(|at| members.get(at).copied())
}

/// A stable index into the tab color palette, so a repo keeps its dot color across launches.
pub(crate) fn color_slot(g: &Group, slots: usize) -> usize {
    let Group::Repo(p) = g else { return 0 };
    let hash = p
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    (hash % slots as u64) as usize
}

/// A repo's roll-up for the chip: the fullest progress across its tabs, and whether any tab
/// failed a command or fired notify-on-activity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Activity {
    pub(crate) progress: Progress,
    pub(crate) attention: bool,
}

impl Activity {
    fn busy(self) -> bool {
        self.attention || ui::progress_fraction(self.progress).is_some()
    }
}

pub(crate) fn roll_up(items: &[Activity]) -> Activity {
    Activity {
        progress: tabs::aggregate_progress(&items.iter().map(|a| a.progress).collect::<Vec<_>>()),
        attention: items.iter().any(|a| a.attention),
    }
}

struct RepoEntry {
    group: Group,
    label: String,
    tabs: usize,
    activity: Activity,
}

const CHIP_H: f32 = 24.0;
const DOT_R: f32 = 3.5;

fn group_color(g: &Group) -> egui::Color32 {
    let palette = colors::tab_colors();
    match g {
        Group::Other => colors::dim(),
        Group::Repo(_) => palette[color_slot(g, palette.len())],
    }
}

impl Stdusk {
    pub(crate) fn grouping(&self) -> bool {
        self.cfg.appearance.group_by_repo
    }

    fn active_group(&self) -> Option<&Group> {
        self.tabs.get(self.active).map(|t| &t.group)
    }

    fn tab_groups(&self) -> Vec<(u64, &Group)> {
        self.tabs.iter().map(|t| (t.id, &t.group)).collect()
    }

    /// Tab indices the bar shows: the active repo's tabs, or every tab with grouping off.
    pub(crate) fn visible_tabs(&self) -> Vec<usize> {
        match self.active_group().filter(|_| self.grouping()) {
            Some(g) => members(&self.tabs.iter().map(|t| &t.group).collect::<Vec<_>>(), g),
            None => (0..self.tabs.len()).collect(),
        }
    }

    pub(crate) fn group_order(&self) -> Vec<Group> {
        order(self.tabs.iter().map(|t| &t.group))
    }

    pub(crate) fn switch_group(&mut self, g: &Group) {
        if let Some(i) = landing_tab(&self.tab_groups(), &self.focus_history, g) {
            self.active = i;
        }
    }

    pub(crate) fn cycle_group(&mut self, d: i32) {
        let Some(cur) = self.active_group().cloned() else { return };
        if let Some(g) = step(&self.group_order(), &cur, d) {
            self.switch_group(&g);
        }
    }

    pub(crate) fn cycle_visible(&mut self, d: i32) {
        self.active = step_within(&self.visible_tabs(), self.active, d);
    }

    /// The focus history to pick the next tab from when the tab at `closing` goes away.
    pub(crate) fn close_history(&self, closing: usize) -> Vec<u64> {
        match self.tabs.get(closing).filter(|_| self.grouping()) {
            Some(t) => history_preferring(&self.tab_groups(), &self.focus_history, &t.group),
            None => self.focus_history.clone(),
        }
    }

    /// Labels for the palette's "Switch repo" entries, parallel to `group_order`.
    pub(crate) fn group_labels(&self) -> Vec<String> {
        labels(&self.group_order())
    }

    pub(crate) fn active_group_label(&self) -> Option<String> {
        let cur = self.active_group()?;
        let order = self.group_order();
        let at = order.iter().position(|g| g == cur)?;
        labels(&order).into_iter().nth(at)
    }

    fn tab_activity(tab: &tabs::Tab) -> Activity {
        let leaves = tab.root().leaves();
        Activity {
            progress: tabs::aggregate_progress(
                &leaves.iter().map(|t| t.progress()).collect::<Vec<_>>(),
            ),
            attention: tab.activity_notified
                || leaves.iter().any(|t| t.cmd_state() == CmdState::Fail),
        }
    }

    fn repo_entries(&self) -> Vec<RepoEntry> {
        let order = self.group_order();
        let names = labels(&order);
        order
            .into_iter()
            .zip(names)
            .map(|(group, label)| {
                let acts: Vec<Activity> =
                    self.tabs.iter().filter(|t| t.group == group).map(Self::tab_activity).collect();
                RepoEntry { tabs: acts.len(), activity: roll_up(&acts), group, label }
            })
            .collect()
    }

    /// The repo chip at the left of the tab bar and its repo list; returns the repo picked.
    pub(crate) fn repo_chip(&self, ui: &mut egui::Ui) -> Option<Group> {
        let cur = self.active_group()?;
        let hk = &self.cfg.hotkeys;
        let tip = ui::shortcut_tip("Switch repo", &format!("{} / {}", hk.next_repo, hk.prev_repo));
        let (_, picked) = chip_picker(ui, &self.repo_entries(), cur, &tip);
        ui.add_space(4.0);
        picked
    }
}

fn chip_picker(
    ui: &mut egui::Ui,
    entries: &[RepoEntry],
    cur: &Group,
    tip: &str,
) -> (Option<egui::Response>, Option<Group>) {
    let Some(here) = entries.iter().find(|e| &e.group == cur) else { return (None, None) };
    let font = egui::FontId::monospace(12.0);
    let name = ui.painter().layout_no_wrap(here.label.clone(), font.clone(), colors::fg());
    let count = ui.painter().layout_no_wrap(here.tabs.to_string(), font.clone(), colors::dim());
    let caret = ui.painter().layout_no_wrap(
        icons::CARET_DOWN.into(),
        egui::FontId::proportional(11.0),
        colors::dim(),
    );
    let (pad, gap) = (9.0, 7.0);
    let w =
        pad + DOT_R * 2.0 + gap + name.size().x + gap + count.size().x + gap + caret.size().x + pad;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CHIP_H), egui::Sense::click());
    let p = ui.painter();
    let fill = if resp.hovered() { colors::hover_elevated() } else { colors::active_tab() };
    p.rect_filled(rect, 7.0, fill);
    p.rect_stroke(rect, 7.0, egui::Stroke::new(1.0, colors::border()), egui::StrokeKind::Inside);
    let cy = rect.center().y;
    let mut x = rect.left() + pad;
    p.circle_filled(egui::pos2(x + DOT_R, cy), DOT_R, group_color(cur));
    x += DOT_R * 2.0 + gap;
    for g in [name, count, caret] {
        let size = g.size();
        p.galley(egui::pos2(x, cy - size.y / 2.0), g, colors::fg());
        x += size.x + gap;
    }
    if entries.iter().any(|e| &e.group != cur && e.activity.busy()) {
        let c = egui::pos2(rect.right() - 2.0, rect.top() + 2.0);
        p.circle_filled(c, DOT_R + 1.5, colors::titlebar());
        p.circle_filled(c, DOT_R, colors::accent());
    }
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
    let mut picked = None;
    egui::Popup::menu(&resp).show(|ui| {
        crate::widgets::style_menu(ui);
        ui.set_min_width(280.0);
        for (i, e) in entries.iter().enumerate() {
            if repo_row(ui, i, e, &e.group == cur).clicked() {
                picked = Some(e.group.clone());
            }
        }
    });
    (Some(resp), picked.filter(|g| g != cur))
}

fn repo_row_id(i: usize) -> egui::Id {
    egui::Id::new(("repo_row", i))
}

fn repo_row(ui: &mut egui::Ui, i: usize, e: &RepoEntry, current: bool) -> egui::Response {
    const BAR_W: f32 = 36.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
    let resp = ui.interact(rect, repo_row_id(i), egui::Sense::click());
    let p = ui.painter();
    if resp.hovered() || current {
        p.rect_filled(rect, 6.0, colors::hover_elevated());
    }
    let cy = rect.center().y;
    let font = egui::FontId::monospace(12.5);
    let mut x = rect.left() + 10.0;
    p.circle_filled(egui::pos2(x + DOT_R, cy), DOT_R, group_color(&e.group));
    x += DOT_R * 2.0 + 9.0;
    let name = p.layout_no_wrap(e.label.clone(), font.clone(), colors::fg());
    let name_w = name.size().x;
    p.galley(egui::pos2(x, cy - name.size().y / 2.0), name, colors::fg());
    x += name_w + 8.0;
    let count = p.layout_no_wrap(e.tabs.to_string(), font, colors::dim());
    p.galley(egui::pos2(x, cy - count.size().y / 2.0), count, colors::dim());
    let mut right = rect.right() - 10.0;
    if e.activity.attention {
        p.circle_filled(egui::pos2(right - DOT_R, cy), DOT_R, colors::yellow());
        right -= DOT_R * 2.0 + 8.0;
    }
    if let Some(f) = ui::progress_fraction(e.activity.progress) {
        let track =
            egui::Rect::from_min_size(egui::pos2(right - BAR_W, cy - 2.0), egui::vec2(BAR_W, 4.0));
        let fill = if matches!(e.activity.progress, Progress::Error(_)) {
            colors::red()
        } else {
            colors::accent()
        };
        p.rect_filled(track, 2.0, colors::border());
        p.rect_filled(egui::Rect::from_min_size(track.min, egui::vec2(BAR_W * f, 4.0)), 2.0, fill);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(p: &str) -> Group {
        Group::Repo(PathBuf::from(p))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stdusk-repo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        dir
    }

    #[test]
    fn repo_root_finds_repos_worktrees_and_submodules() {
        let root = &scratch("roots");
        let main = root.join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/feat")).unwrap();
        std::fs::create_dir_all(main.join("src/deep")).unwrap();

        let abs_wt = root.join("main-feat");
        std::fs::create_dir_all(abs_wt.join("src")).unwrap();
        let gitdir = main.join(".git/worktrees/feat");
        std::fs::write(abs_wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();

        let rel_wt = root.join("rel-feat");
        std::fs::create_dir_all(&rel_wt).unwrap();
        std::fs::write(rel_wt.join(".git"), "gitdir: ../main/.git/worktrees/feat\n").unwrap();

        let sub = main.join("vendor/lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();

        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        let cases = [
            (main.clone(), Some(main.clone())),
            (main.join("src/deep"), Some(main.clone())),
            (abs_wt.join("src"), Some(main.clone())),
            (rel_wt.clone(), Some(main.clone())),
            (sub.clone(), Some(sub.clone())),
            (plain.clone(), None),
        ];
        for (dir, want) in cases {
            assert_eq!(repo_root(&dir), want, "dir {}", dir.display());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_or_repo_less_cwd_is_other() {
        assert_eq!(Group::for_cwd(None), Group::Other);
        let dir = scratch("plain");
        assert_eq!(Group::for_cwd(dir.to_str()), Group::Other);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn regroup_follows_the_cwd_into_and_out_of_repos() {
        let root = scratch("regroup");
        let repo_dir = root.join("web");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        let plain = root.join("notes");
        std::fs::create_dir_all(&plain).unwrap();
        let (r, p) = (repo_dir.to_str().unwrap(), plain.to_str().unwrap());
        assert_eq!(
            regroup(Some(r), Some(p)),
            Some(Group::Repo(repo_dir.clone())),
            "cd into a repo"
        );
        assert_eq!(regroup(Some(p), Some(r)), Some(Group::Other), "cd out of any repo");
        assert_eq!(regroup(Some(r), None), Some(Group::Repo(repo_dir.clone())), "first known cwd");
        assert_eq!(regroup(Some(r), Some(r)), None, "unchanged cwd: no disk walk");
        assert_eq!(regroup(None, Some(r)), None, "unknown cwd keeps the group");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saved_encoding_round_trips() {
        for g in [repo("/a/b"), Group::Other] {
            let back = g.to_saved().map_or(Group::Other, |p| Group::Repo(p.into()));
            assert_eq!(back, g);
        }
    }

    #[test]
    fn order_keeps_first_appearance_and_puts_other_last() {
        let (a, b) = (repo("/a"), repo("/b"));
        let tabs = [Group::Other, b.clone(), a.clone(), b.clone(), Group::Other];
        assert_eq!(order(&tabs), vec![b, a, Group::Other]);
        assert!(order(&[]).is_empty());
    }

    #[test]
    fn labels_disambiguate_repos_sharing_a_name() {
        let groups = [repo("/g/trakt/web"), repo("/g/hobo/web"), repo("/g/stdusk"), Group::Other];
        assert_eq!(labels(&groups), ["trakt/web", "hobo/web", "stdusk", "Other"]);
    }

    #[test]
    fn landing_tab_prefers_the_last_focused_tab_of_the_group() {
        let (a, b) = (repo("/a"), repo("/b"));
        let tabs = [(10, &a), (11, &b), (12, &a), (13, &b)];
        assert_eq!(landing_tab(&tabs, &[12, 13, 10], &a), Some(2));
        assert_eq!(landing_tab(&tabs, &[12, 13, 10], &b), Some(3));
        assert_eq!(landing_tab(&tabs, &[], &b), Some(1), "no history: the group's first tab");
        assert_eq!(landing_tab(&tabs, &[], &Group::Other), None);
    }

    #[test]
    fn step_wraps_through_the_group_order() {
        let order = [repo("/a"), repo("/b"), Group::Other];
        assert_eq!(step(&order, &repo("/a"), 1), Some(repo("/b")));
        assert_eq!(step(&order, &Group::Other, 1), Some(repo("/a")));
        assert_eq!(step(&order, &repo("/a"), -1), Some(Group::Other));
        assert_eq!(step(&order, &repo("/zzz"), 1), None);
    }

    #[test]
    fn closing_prefers_history_from_the_same_group() {
        let (a, b) = (repo("/a"), repo("/b"));
        let tabs = [(10, &a), (11, &b), (12, &a)];
        assert_eq!(history_preferring(&tabs, &[11, 12, 10], &a), vec![12, 10, 11]);
        assert_eq!(history_preferring(&tabs, &[11, 12, 10], &b), vec![11, 12, 10]);
    }

    #[test]
    fn members_and_step_within_skip_other_groups() {
        let (a, b) = (repo("/a"), repo("/b"));
        let groups = [&a, &b, &a, &b, &a];
        let m = members(&groups, &a);
        assert_eq!(m, vec![0, 2, 4]);
        assert_eq!(step_within(&m, 2, 1), 4);
        assert_eq!(step_within(&m, 4, 1), 0);
        assert_eq!(step_within(&m, 0, -1), 4);
        assert_eq!(step_within(&m, 3, 1), 3, "a non-member stays put");
        assert_eq!(neighbor(&m, 2, 1), Some(4));
        assert_eq!(neighbor(&m, 2, -1), Some(0));
        assert_eq!(neighbor(&m, 4, 1), None, "no wrap at the edges");
        assert_eq!(neighbor(&m, 0, -1), None);
        assert_eq!(neighbor(&m, 3, 1), None);
    }

    #[test]
    fn roll_up_keeps_the_fullest_progress_and_any_attention() {
        let act = |progress, attention| Activity { progress, attention };
        let got = roll_up(&[act(Progress::Normal(20), false), act(Progress::Normal(70), true)]);
        assert_eq!(got, act(Progress::Normal(70), true));
        assert_eq!(roll_up(&[]), act(Progress::None, false));
        assert!(!roll_up(&[act(Progress::None, false)]).busy());
        assert!(roll_up(&[act(Progress::Indeterminate, false)]).busy());
    }

    fn chip_frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        entries: &[RepoEntry],
    ) -> (egui::Rect, Option<Group>) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        let mut out = (egui::Rect::NOTHING, None);
        let _ = ctx.run_ui(raw, |ui| {
            egui::Panel::top("tabbar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (resp, picked) = chip_picker(ui, entries, &entries[0].group, "Switch repo");
                    out = (resp.expect("the current repo has an entry").rect, picked);
                });
            });
        });
        out
    }

    fn click_at(ctx: &egui::Context, entries: &[RepoEntry], pos: egui::Pos2) -> Option<Group> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        chip_frame(ctx, vec![egui::Event::PointerMoved(pos), button(true)], entries);
        chip_frame(ctx, vec![button(false)], entries).1
    }

    #[test]
    fn clicking_the_chip_then_a_repo_row_picks_that_repo() {
        let idle = Activity { progress: Progress::None, attention: false };
        let entries = [
            RepoEntry { group: repo("/a/stdusk"), label: "stdusk".into(), tabs: 2, activity: idle },
            RepoEntry { group: repo("/a/web"), label: "web".into(), tabs: 3, activity: idle },
        ];
        let ctx = egui::Context::default();
        let (chip, _) = chip_frame(&ctx, vec![], &entries);
        assert_eq!(click_at(&ctx, &entries, chip.center()), None, "opening the list picks nothing");
        chip_frame(&ctx, vec![], &entries);
        let row =
            |i| ctx.read_response(repo_row_id(i)).expect("the open list shows every repo").rect;
        assert_eq!(click_at(&ctx, &entries, row(0).center()), None, "the current repo is a no-op");

        let (chip, _) = chip_frame(&ctx, vec![], &entries);
        click_at(&ctx, &entries, chip.center());
        chip_frame(&ctx, vec![], &entries);
        assert_eq!(click_at(&ctx, &entries, row(1).center()), Some(repo("/a/web")));
    }

    #[test]
    fn color_slot_is_stable_and_in_range() {
        let g = repo("/Users/x/Git/stdusk");
        assert_eq!(color_slot(&g, 12), color_slot(&g.clone(), 12));
        assert!(color_slot(&g, 12) < 12);
        assert_eq!(color_slot(&Group::Other, 12), 0);
    }
}
