//! Split-pane tree. A tab's terminals form a binary tree: each node is either a `Leaf`
//! (one payload - a `PtyTerm` in the app, any `T` in tests) or a `Split` of two children.
//! A leaf is addressed by a `path` of `Side`s from the root, which doubles as its identity
//! (focus is a path). All structure ops here are pure and unit-tested with `T = u32`; the
//! egui glue (rendering each leaf's rect, dragging splitters) lives in `main.rs`/`ui.rs`.
use eframe::egui::{Pos2, Rect, pos2, vec2};

/// Gap (px) between sibling panes, where the draggable splitter sits.
pub(crate) const GUTTER: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    A,
    B,
}

/// `Row` = children side by side (a vertical splitter); `Column` = stacked (horizontal splitter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitDir {
    Row,
    Column,
}

pub(crate) enum Pane<T> {
    Leaf(T),
    Split { dir: SplitDir, ratio: f32, a: Box<Pane<T>>, b: Box<Pane<T>> },
}

fn prepend(side: Side, mut path: Vec<Side>) -> Vec<Side> {
    path.insert(0, side);
    path
}

impl<T> Pane<T> {
    pub(crate) fn leaf(value: T) -> Self {
        Pane::Leaf(value)
    }

    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            Pane::Leaf(_) => 1,
            Pane::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    /// Every leaf value, left-to-right (for tab-wide aggregation like CLI-awareness badges).
    pub(crate) fn leaves(&self) -> Vec<&T> {
        match self {
            Pane::Leaf(t) => vec![t],
            Pane::Split { a, b, .. } => {
                let mut v = a.leaves();
                v.extend(b.leaves());
                v
            }
        }
    }

    /// Path to the first (all-`A`) leaf - the focus target after a collapse.
    pub(crate) fn first_leaf_path(&self) -> Vec<Side> {
        let mut path = Vec::new();
        let mut cur = self;
        while let Pane::Split { a, .. } = cur {
            path.push(Side::A);
            cur = a;
        }
        path
    }

    fn at(&self, path: &[Side]) -> Option<&Pane<T>> {
        let mut cur = self;
        for &s in path {
            match cur {
                Pane::Split { a, b, .. } => cur = if s == Side::A { a } else { b },
                Pane::Leaf(_) => return None,
            }
        }
        Some(cur)
    }

    fn at_mut(&mut self, path: &[Side]) -> Option<&mut Pane<T>> {
        let mut cur = self;
        for &s in path {
            cur = match cur {
                Pane::Split { a, b, .. } => {
                    if s == Side::A {
                        a.as_mut()
                    } else {
                        b.as_mut()
                    }
                }
                Pane::Leaf(_) => return None,
            };
        }
        Some(cur)
    }

    pub(crate) fn leaf_at(&self, path: &[Side]) -> Option<&T> {
        match self.at(path)? {
            Pane::Leaf(t) => Some(t),
            Pane::Split { .. } => None,
        }
    }

    pub(crate) fn leaf_at_mut(&mut self, path: &[Side]) -> Option<&mut T> {
        match self.at_mut(path)? {
            Pane::Leaf(t) => Some(t),
            Pane::Split { .. } => None,
        }
    }

    /// Split the leaf at `path` with `dir`. `new_first` places the new pane before the old
    /// (left/up) rather than after (right/down). Returns the rebuilt tree and the path to the
    /// new leaf (to focus), or `None` when `path` doesn't point at a leaf.
    pub(crate) fn split(
        self,
        path: &[Side],
        dir: SplitDir,
        new: T,
        new_first: bool,
    ) -> (Pane<T>, Option<Vec<Side>>) {
        match path.split_first() {
            None => match self {
                Pane::Leaf(t) => {
                    let (a, b, focus) = if new_first {
                        (Pane::Leaf(new), Pane::Leaf(t), Side::A)
                    } else {
                        (Pane::Leaf(t), Pane::Leaf(new), Side::B)
                    };
                    let node = Pane::Split { dir, ratio: 0.5, a: Box::new(a), b: Box::new(b) };
                    (node, Some(vec![focus]))
                }
                split @ Pane::Split { .. } => (split, None),
            },
            Some((&s, rest)) => match self {
                Pane::Split { dir: d, ratio, a, b } => match s {
                    Side::A => {
                        let (na, f) = a.split(rest, dir, new, new_first);
                        (
                            Pane::Split { dir: d, ratio, a: Box::new(na), b },
                            f.map(|p| prepend(Side::A, p)),
                        )
                    }
                    Side::B => {
                        let (nb, f) = b.split(rest, dir, new, new_first);
                        (
                            Pane::Split { dir: d, ratio, a, b: Box::new(nb) },
                            f.map(|p| prepend(Side::B, p)),
                        )
                    }
                },
                leaf @ Pane::Leaf(_) => (leaf, None),
            },
        }
    }

    /// Close the leaf at `path`, collapsing its parent split into the sibling. Returns the new
    /// tree (`None` if the whole tree was that one leaf) and the path to focus next.
    pub(crate) fn close(self, path: &[Side]) -> (Option<Pane<T>>, Option<Vec<Side>>) {
        match path.split_first() {
            None => (None, None), // closed the root leaf -> empty
            Some((&s, rest)) => match self {
                Pane::Leaf(l) => (Some(Pane::Leaf(l)), None), // path too long: no-op
                Pane::Split { dir, ratio, a, b } => {
                    if rest.is_empty() {
                        // Direct child leaf closed -> collapse to the sibling.
                        let sibling = if s == Side::A { *b } else { *a };
                        let focus = sibling.first_leaf_path();
                        (Some(sibling), Some(focus))
                    } else if s == Side::A {
                        let (na, f) = a.close(rest);
                        match na {
                            None => {
                                let focus = b.first_leaf_path();
                                (Some(*b), Some(focus))
                            }
                            Some(na) => (
                                Some(Pane::Split { dir, ratio, a: Box::new(na), b }),
                                f.map(|p| prepend(Side::A, p)),
                            ),
                        }
                    } else {
                        let (nb, f) = b.close(rest);
                        match nb {
                            None => {
                                let focus = a.first_leaf_path();
                                (Some(*a), Some(focus))
                            }
                            Some(nb) => (
                                Some(Pane::Split { dir, ratio, a, b: Box::new(nb) }),
                                f.map(|p| prepend(Side::B, p)),
                            ),
                        }
                    }
                }
            },
        }
    }

    /// Clamp + set the ratio of the split node at `path` (no-op if it's not a split).
    pub(crate) fn set_ratio(&mut self, path: &[Side], ratio: f32) {
        if let Some(Pane::Split { ratio: r, .. }) = self.at_mut(path) {
            *r = ratio.clamp(0.05, 0.95);
        }
    }

    /// Every leaf's `(path, rect)` for the given area, accounting for split ratios + gutters.
    pub(crate) fn layout(&self, area: Rect) -> Vec<(Vec<Side>, Rect)> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        self.layout_into(area, &mut path, &mut out);
        out
    }

    fn layout_into(&self, rect: Rect, path: &mut Vec<Side>, out: &mut Vec<(Vec<Side>, Rect)>) {
        match self {
            Pane::Leaf(_) => out.push((path.clone(), rect)),
            Pane::Split { dir, ratio, a, b } => {
                let (ra, rb) = split_rect(rect, *dir, *ratio, GUTTER);
                path.push(Side::A);
                a.layout_into(ra, path, out);
                path.pop();
                path.push(Side::B);
                b.layout_into(rb, path, out);
                path.pop();
            }
        }
    }

    /// Leaf rects for a tiny tab glyph that previews the split layout - same structure as
    /// `layout` but in a unit square with a proportional gap, so the caller scales it to any box.
    pub(crate) fn miniature(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        self.mini_into(Rect::from_min_size(pos2(0.0, 0.0), vec2(1.0, 1.0)), &mut out);
        out
    }

    fn mini_into(&self, rect: Rect, out: &mut Vec<Rect>) {
        match self {
            Pane::Leaf(_) => out.push(rect),
            Pane::Split { dir, ratio, a, b } => {
                // Gap proportional to the smaller side so nested splits keep visible seams.
                let gap = 0.12 * rect.width().min(rect.height());
                let (ra, rb) = split_rect(rect, *dir, *ratio, gap);
                a.mini_into(ra, out);
                b.mini_into(rb, out);
            }
        }
    }

    /// Each split node's `(path, dir, handle_rect, parent_rect)` - the handle is the gutter the
    /// user drags; the parent rect maps a drag position back to a ratio.
    pub(crate) fn splitters(&self, area: Rect) -> Vec<(Vec<Side>, SplitDir, Rect, Rect)> {
        let mut out = Vec::new();
        let mut path = Vec::new();
        self.splitters_into(area, &mut path, &mut out);
        out
    }

    fn splitters_into(
        &self,
        rect: Rect,
        path: &mut Vec<Side>,
        out: &mut Vec<(Vec<Side>, SplitDir, Rect, Rect)>,
    ) {
        if let Pane::Split { dir, ratio, a, b } = self {
            out.push((path.clone(), *dir, gutter_rect(rect, *dir, *ratio), rect));
            let (ra, rb) = split_rect(rect, *dir, *ratio, GUTTER);
            path.push(Side::A);
            a.splitters_into(ra, path, out);
            path.pop();
            path.push(Side::B);
            b.splitters_into(rb, path, out);
            path.pop();
        }
    }
}

/// Split `rect` into two child rects by `ratio`, leaving a `gutter` gap between them.
fn split_rect(rect: Rect, dir: SplitDir, ratio: f32, gutter: f32) -> (Rect, Rect) {
    match dir {
        SplitDir::Row => {
            let usable = (rect.width() - gutter).max(0.0);
            let aw = usable * ratio;
            let a = Rect::from_min_size(rect.min, vec2(aw, rect.height()));
            let b = Rect::from_min_size(
                pos2(rect.min.x + aw + gutter, rect.min.y),
                vec2((usable - aw).max(0.0), rect.height()),
            );
            (a, b)
        }
        SplitDir::Column => {
            let usable = (rect.height() - gutter).max(0.0);
            let ah = usable * ratio;
            let a = Rect::from_min_size(rect.min, vec2(rect.width(), ah));
            let b = Rect::from_min_size(
                pos2(rect.min.x, rect.min.y + ah + gutter),
                vec2(rect.width(), (usable - ah).max(0.0)),
            );
            (a, b)
        }
    }
}

/// The gutter rect (draggable splitter handle) between the two children of a split.
fn gutter_rect(rect: Rect, dir: SplitDir, ratio: f32) -> Rect {
    match dir {
        SplitDir::Row => {
            let aw = (rect.width() - GUTTER).max(0.0) * ratio;
            Rect::from_min_size(pos2(rect.min.x + aw, rect.min.y), vec2(GUTTER, rect.height()))
        }
        SplitDir::Column => {
            let ah = (rect.height() - GUTTER).max(0.0) * ratio;
            Rect::from_min_size(pos2(rect.min.x, rect.min.y + ah), vec2(rect.width(), GUTTER))
        }
    }
}

/// Map a pointer position on a splitter back to a ratio for its parent rect.
pub(crate) fn ratio_from_pointer(parent: Rect, dir: SplitDir, pointer: Pos2) -> f32 {
    let r = match dir {
        SplitDir::Row => (pointer.x - parent.min.x) / (parent.width() - GUTTER).max(1.0),
        SplitDir::Column => (pointer.y - parent.min.y) / (parent.height() - GUTTER).max(1.0),
    };
    r.clamp(0.05, 0.95)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_root_leaf_focuses_new() {
        let (tree, focus) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false);
        assert_eq!(focus, Some(vec![Side::B]));
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.leaf_at(&[Side::A]), Some(&1));
        assert_eq!(tree.leaf_at(&[Side::B]), Some(&2));
    }

    #[test]
    fn split_nested_leaf() {
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false); // [A]=1 [B]=2
        let (tree, focus) = tree.split(&[Side::B], SplitDir::Column, 3, false); // split the 2
        assert_eq!(focus, Some(vec![Side::B, Side::B]));
        assert_eq!(tree.leaf_at(&[Side::A]), Some(&1));
        assert_eq!(tree.leaf_at(&[Side::B, Side::A]), Some(&2));
        assert_eq!(tree.leaf_at(&[Side::B, Side::B]), Some(&3));
    }

    #[test]
    fn split_new_first_places_new_before_old() {
        let (tree, focus) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, true);
        assert_eq!(focus, Some(vec![Side::A])); // new pane is on the A side
        assert_eq!(tree.leaf_at(&[Side::A]), Some(&2));
        assert_eq!(tree.leaf_at(&[Side::B]), Some(&1));
    }

    #[test]
    fn cannot_split_a_split_node() {
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false);
        let (tree, focus) = tree.split(&[], SplitDir::Row, 9, false); // root is a split now
        assert_eq!(focus, None);
        assert_eq!(tree.leaf_count(), 2); // unchanged
    }

    #[test]
    fn close_collapses_to_sibling() {
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false);
        let (tree, focus) = tree.close(&[Side::B]); // close the 2
        let tree = tree.unwrap();
        assert!(matches!(tree, Pane::Leaf(_)));
        assert_eq!(tree.leaf_at(&[]), Some(&1));
        assert_eq!(focus, Some(vec![])); // focus the surviving leaf (root)
    }

    #[test]
    fn close_nested_keeps_rest_and_refocuses() {
        // [A]=1, [B/A]=2, [B/B]=3 ; close 2 -> B collapses to 3 at path [B]
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false);
        let (tree, _) = tree.split(&[Side::B], SplitDir::Column, 3, false);
        let (tree, focus) = tree.close(&[Side::B, Side::A]);
        let tree = tree.unwrap();
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.leaf_at(&[Side::A]), Some(&1));
        assert_eq!(tree.leaf_at(&[Side::B]), Some(&3));
        assert_eq!(focus, Some(vec![Side::B]));
    }

    #[test]
    fn close_last_leaf_empties() {
        let (tree, focus) = Pane::leaf(1u32).close(&[]);
        assert!(tree.is_none());
        assert_eq!(focus, None);
    }

    #[test]
    fn layout_row_splits_width_with_gutter() {
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false); // ratio 0.5
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 40.0));
        let l = tree.layout(area);
        let a = l.iter().find(|(p, _)| p == &vec![Side::A]).unwrap().1;
        let b = l.iter().find(|(p, _)| p == &vec![Side::B]).unwrap().1;
        assert_eq!(a.width(), (100.0 - GUTTER) * 0.5);
        assert_eq!(b.width(), (100.0 - GUTTER) * 0.5);
        assert_eq!(a.height(), 40.0);
        assert_eq!(b.min.x, a.max.x + GUTTER); // gutter between them
    }

    #[test]
    fn miniature_reflects_structure() {
        // Single leaf -> one rect filling (near) the whole unit square.
        assert_eq!(Pane::leaf(1u32).miniature().len(), 1);
        // Row split -> two rects side by side (a left of b, same height range).
        let (tree, _) = Pane::leaf(1u32).split(&[], SplitDir::Row, 2, false);
        let r = tree.miniature();
        assert_eq!(r.len(), 2);
        assert!(r[0].max.x <= r[1].min.x); // a is left of b (with a gap)
        assert!((r[0].height() - 1.0).abs() < 1e-6 && (r[1].height() - 1.0).abs() < 1e-6);
        // Nested (row then column on B) -> three leaf rects.
        let (tree, _) = tree.split(&[Side::B], SplitDir::Column, 3, false);
        assert_eq!(tree.miniature().len(), 3);
    }

    #[test]
    fn ratio_from_pointer_clamps() {
        let parent = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        assert!((ratio_from_pointer(parent, SplitDir::Row, pos2(47.0, 0.0)) - 0.5).abs() < 0.02);
        assert_eq!(ratio_from_pointer(parent, SplitDir::Row, pos2(-99.0, 0.0)), 0.05); // clamp low
        assert_eq!(ratio_from_pointer(parent, SplitDir::Row, pos2(999.0, 0.0)), 0.95); // clamp high
    }
}
