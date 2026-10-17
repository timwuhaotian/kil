//! Binary split tree of panes. Pure and generic over the leaf type, so it is unit-tested
//! without GPUI (DESIGN.md §7).

use std::time::Instant;

use crate::motion::{self, Ease, Tween};

/// `Horizontal` places children side by side; `Vertical` stacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A rectangle in arbitrary units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const UNIT: Self = Self {
        x: 0.,
        y: 0.,
        w: 1.,
        h: 1.,
    };

    fn split(self, axis: Axis, ratio: f32) -> (Self, Self) {
        match axis {
            Axis::Horizontal => {
                let w = self.w * ratio;
                (
                    Self { w, ..self },
                    Self {
                        x: self.x + w,
                        w: self.w - w,
                        ..self
                    },
                )
            }
            Axis::Vertical => {
                let h = self.h * ratio;
                (
                    Self { h, ..self },
                    Self {
                        y: self.y + h,
                        h: self.h - h,
                        ..self
                    },
                )
            }
        }
    }
}

/// Unique within one tree; identifies a split for divider drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SplitId(u64);

impl SplitId {
    pub fn bits(self) -> u64 {
        self.0
    }
}

#[derive(Clone)]
pub enum Node<T> {
    Leaf(T),
    Split(Box<Split<T>>),
}

#[derive(Clone)]
pub struct Split<T> {
    pub id: SplitId,
    pub axis: Axis,
    pub ratio: Tween,
    pub first: Node<T>,
    pub second: Node<T>,
    /// Set while one child animates away; the node collapses into the other afterwards.
    closing: Option<Side>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    First,
    Second,
}

/// What closing a leaf did.
#[derive(Debug, PartialEq, Eq)]
pub enum Close {
    /// It was the only leaf; the tab should close.
    LastLeaf,
    /// Its share is animating to zero; call [`PaneTree::collapse_finished`] afterwards.
    Animating,
    NotFound,
}

pub struct PaneTree<T> {
    root: Node<T>,
    next_id: u64,
}

const MIN_RATIO: f32 = 0.08;
/// Tolerance for edge adjacency in unit layout space.
const EPS: f32 = 1e-4;

/// Leaves are cheap handles (`Entity` clones bump a refcount), so restructuring clones them
/// instead of needing placeholder values.
impl<T: Clone + PartialEq> PaneTree<T> {
    pub fn new(leaf: T) -> Self {
        Self {
            root: Node::Leaf(leaf),
            next_id: 0,
        }
    }

    pub fn root(&self) -> &Node<T> {
        &self.root
    }

    /// Splits `target`: it keeps the first half and `new` animates in as the second.
    pub fn split(&mut self, target: &T, axis: Axis, new: T, now: Instant) -> bool {
        let id = SplitId(self.next_id);
        let Some(node) = find_leaf_mut(&mut self.root, target) else {
            return false;
        };
        let Node::Leaf(old) = node else { return false };
        let old = old.clone();
        self.next_id += 1;
        let mut ratio = Tween::settled(1.);
        ratio.retarget(0.5, motion::SPLIT, Ease::OutQuint, now);
        let split = Split {
            id,
            axis,
            ratio,
            first: Node::Leaf(old),
            second: Node::Leaf(new),
            closing: None,
        };
        *node = Node::Split(Box::new(split));
        true
    }

    /// Starts closing `target`: its share animates to zero (DESIGN.md §7).
    pub fn close(&mut self, target: &T, now: Instant) -> Close {
        if matches!(&self.root, Node::Leaf(leaf) if leaf == target) {
            return Close::LastLeaf;
        }
        let Some(split) = parent_of_mut(&mut self.root, target) else {
            return Close::NotFound;
        };
        let (side, to) = if matches!(&split.first, Node::Leaf(leaf) if leaf == target) {
            (Side::First, 0.)
        } else {
            (Side::Second, 1.)
        };
        split.closing = Some(side);
        split
            .ratio
            .retarget(to, motion::CLOSE_PANE, Ease::InOut, now);
        Close::Animating
    }

    /// Collapses splits whose closing animation ended; returns the removed leaves.
    pub fn collapse_finished(&mut self, now: Instant) -> Vec<T> {
        let mut removed = Vec::new();
        collapse(&mut self.root, now, &mut removed);
        removed
    }

    /// Sets a split's ratio directly (divider drag), clamped away from the edges.
    pub fn set_ratio(&mut self, id: SplitId, ratio: f32) {
        if let Some(split) = find_split_mut(&mut self.root, id) {
            split.ratio.set(ratio.clamp(MIN_RATIO, 1. - MIN_RATIO));
        }
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        any_split(&self.root, &|split| {
            split.ratio.is_animating(now) || split.closing.is_some()
        })
    }

    pub fn leaves(&self) -> Vec<&T> {
        let mut leaves = Vec::new();
        collect(&self.root, &mut leaves);
        leaves
    }

    pub fn contains(&self, target: &T) -> bool {
        self.leaves().into_iter().any(|leaf| leaf == target)
    }

    /// Leaf rectangles using each split's *target* ratio: where things are going, which is
    /// what focus navigation and grid sizing should follow.
    pub fn target_layout(&self, rect: Rect) -> Vec<(&T, Rect)> {
        let mut out = Vec::new();
        layout(&self.root, rect, &mut out);
        out
    }

    /// The nearest leaf in `direction` from `target`, among leaves that overlap it on the
    /// perpendicular axis.
    pub fn neighbor(&self, target: &T, direction: Direction) -> Option<&T> {
        let rects = self.target_layout(Rect::UNIT);
        let (_, from) = rects.iter().find(|(leaf, _)| *leaf == target)?;
        let from = *from;
        rects
            .iter()
            // Zero-area leaves are closing; never navigate into them.
            .filter(|(leaf, rect)| *leaf != target && rect.w > EPS && rect.h > EPS)
            .filter_map(|(leaf, rect)| {
                let (gap, overlap) = match direction {
                    Direction::Left => (
                        from.x - (rect.x + rect.w),
                        overlap(from.y, from.h, rect.y, rect.h),
                    ),
                    Direction::Right => (
                        rect.x - (from.x + from.w),
                        overlap(from.y, from.h, rect.y, rect.h),
                    ),
                    Direction::Up => (
                        from.y - (rect.y + rect.h),
                        overlap(from.x, from.w, rect.x, rect.w),
                    ),
                    Direction::Down => (
                        rect.y - (from.y + from.h),
                        overlap(from.x, from.w, rect.x, rect.w),
                    ),
                };
                (gap > -EPS && overlap > EPS).then_some((*leaf, gap, overlap))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
            .map(|(leaf, _, _)| leaf)
    }
}

fn overlap(a: f32, a_len: f32, b: f32, b_len: f32) -> f32 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0.)
}

fn find_leaf_mut<'a, T: PartialEq>(node: &'a mut Node<T>, target: &T) -> Option<&'a mut Node<T>> {
    match node {
        Node::Leaf(leaf) if leaf == target => Some(node),
        Node::Leaf(_) => None,
        Node::Split(split) => find_leaf_mut(&mut split.first, target)
            .or_else(|| find_leaf_mut(&mut split.second, target)),
    }
}

fn parent_of_mut<'a, T: PartialEq>(node: &'a mut Node<T>, target: &T) -> Option<&'a mut Split<T>> {
    let Node::Split(split) = node else {
        return None;
    };
    let is_child = |child: &Node<T>| matches!(child, Node::Leaf(leaf) if leaf == target);
    if is_child(&split.first) || is_child(&split.second) {
        return Some(split);
    }
    if contains_leaf(&split.first, target) {
        parent_of_mut(&mut split.first, target)
    } else {
        parent_of_mut(&mut split.second, target)
    }
}

fn contains_leaf<T: PartialEq>(node: &Node<T>, target: &T) -> bool {
    match node {
        Node::Leaf(leaf) => leaf == target,
        Node::Split(split) => {
            contains_leaf(&split.first, target) || contains_leaf(&split.second, target)
        }
    }
}

fn find_split_mut<T>(node: &mut Node<T>, id: SplitId) -> Option<&mut Split<T>> {
    let Node::Split(split) = node else {
        return None;
    };
    if split.id == id {
        return Some(split);
    }
    if let Some(found) = find_split_mut(&mut split.first, id) {
        return Some(found);
    }
    find_split_mut(&mut split.second, id)
}

fn collapse<T: Clone>(node: &mut Node<T>, now: Instant, removed: &mut Vec<T>) {
    let Node::Split(split) = node else { return };
    collapse(&mut split.first, now, removed);
    collapse(&mut split.second, now, removed);
    let Some(side) = split.closing.filter(|_| !split.ratio.is_animating(now)) else {
        return;
    };
    let (keep, gone) = match side {
        Side::First => (&split.second, &split.first),
        Side::Second => (&split.first, &split.second),
    };
    let mut gone_leaves = Vec::new();
    collect(gone, &mut gone_leaves);
    removed.extend(gone_leaves.into_iter().cloned());
    let keep = keep.clone();
    *node = keep;
}

fn collect<'a, T>(node: &'a Node<T>, out: &mut Vec<&'a T>) {
    match node {
        Node::Leaf(leaf) => out.push(leaf),
        Node::Split(split) => {
            collect(&split.first, out);
            collect(&split.second, out);
        }
    }
}

fn any_split<T>(node: &Node<T>, predicate: &dyn Fn(&Split<T>) -> bool) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split(split) => {
            predicate(split)
                || any_split(&split.first, predicate)
                || any_split(&split.second, predicate)
        }
    }
}

fn layout<'a, T>(node: &'a Node<T>, rect: Rect, out: &mut Vec<(&'a T, Rect)>) {
    match node {
        Node::Leaf(leaf) => out.push((leaf, rect)),
        Node::Split(split) => {
            let (first, second) = rect.split(split.axis, split.ratio.target());
            layout(&split.first, first, out);
            layout(&split.second, second, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn settle(tree: &mut PaneTree<u32>, now: Instant) -> Instant {
        let later = now + Duration::from_secs(1);
        tree.collapse_finished(later);
        later
    }

    /// 1 | 2 side by side, then 2 split into 2 over 3.
    fn three_panes() -> PaneTree<u32> {
        let now = Instant::now();
        let mut tree = PaneTree::new(1);
        tree.split(&1, Axis::Horizontal, 2, now);
        tree.split(&2, Axis::Vertical, 3, now);
        tree
    }

    #[test]
    fn split_should_put_new_leaf_after_target() {
        assert_eq!(three_panes().leaves(), [&1, &2, &3]);
    }

    #[test]
    fn close_should_report_last_leaf_for_single_pane() {
        assert_eq!(PaneTree::new(1).close(&1, Instant::now()), Close::LastLeaf);
    }

    #[test]
    fn collapse_finished_should_remove_closed_leaf_after_animation() {
        let mut tree = three_panes();
        let now = Instant::now();
        tree.close(&2, now);
        let removed = tree.collapse_finished(now + Duration::from_secs(1));
        assert_eq!((removed, tree.leaves()), (vec![2], vec![&1, &3]));
    }

    #[test]
    fn collapse_finished_should_keep_leaf_while_animating() {
        let mut tree = three_panes();
        let now = Instant::now();
        tree.close(&2, now);
        assert!(tree.collapse_finished(now).is_empty());
    }

    #[test]
    fn neighbor_should_find_pane_to_the_right() {
        assert_eq!(three_panes().neighbor(&1, Direction::Right), Some(&2));
    }

    #[test]
    fn neighbor_should_find_pane_below() {
        assert_eq!(three_panes().neighbor(&2, Direction::Down), Some(&3));
    }

    #[test]
    fn neighbor_should_be_none_at_edge() {
        assert_eq!(three_panes().neighbor(&1, Direction::Left), None);
    }

    #[test]
    fn target_layout_should_use_target_ratio_mid_animation() {
        let tree = three_panes();
        let first = tree.target_layout(Rect::UNIT)[0].1;
        assert!((first.w - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn is_animating_should_be_false_after_settling() {
        let mut tree = three_panes();
        let later = settle(&mut tree, Instant::now());
        assert!(!tree.is_animating(later));
    }
}
