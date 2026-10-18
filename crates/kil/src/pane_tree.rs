//! Binary split tree of panes. Pure and generic over the leaf type, so it is unit-tested
//! without GPUI (DESIGN.md §7).

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::motion::{self, Ease, Tween};

/// `Horizontal` places children side by side; `Vertical` stacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// A tree's structure without animation state, leaves in [`PaneTree::leaves`] order: what a
/// saved layout holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Shape<T> {
    // First, so reading tries it first: a leaf's optional fields would accept a split's table.
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Shape<T>>,
        second: Box<Shape<T>>,
    },
    Leaf(T),
}

impl<T> Shape<T> {
    /// The same shape with each leaf replaced by `f(leaf)`, in leaf order.
    pub fn map<U>(self, f: &mut impl FnMut(T) -> U) -> Shape<U> {
        match self {
            Self::Leaf(leaf) => Shape::Leaf(f(leaf)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => Shape::Split {
                axis,
                ratio,
                first: Box::new(first.map(f)),
                second: Box::new(second.map(f)),
            },
        }
    }
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

    /// A settled tree laid out like `shape`, e.g. one saved by an earlier launch.
    pub fn from_shape(shape: Shape<T>) -> Self {
        let mut next_id = 0;
        let root = node_from(shape, &mut next_id);
        Self { root, next_id }
    }

    /// Where the layout is heading: target ratios, closing leaves left out.
    pub fn shape(&self) -> Shape<&T> {
        shape_of(&self.root)
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
        // A tab whose panes changed shares its space evenly again.
        equalize(&mut self.root, now);
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
        // Same as a split: the panes that survive share the tab evenly.
        equalize(&mut self.root, now);
        Close::Animating
    }

    /// Collapses splits whose closing animation ended; returns the removed leaves.
    pub fn collapse_finished(&mut self, now: Instant) -> Vec<T> {
        let mut removed = Vec::new();
        collapse(&mut self.root, now, &mut removed);
        removed
    }

    /// Gives every pane an equal share of the tab: panes side by side share the width, and
    /// panes stacked share the height (divider double-click; also after any pane change).
    pub fn equalize(&mut self, now: Instant) {
        equalize(&mut self.root, now);
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

fn node_from<T>(shape: Shape<T>, next_id: &mut u64) -> Node<T> {
    match shape {
        Shape::Leaf(leaf) => Node::Leaf(leaf),
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let id = SplitId(*next_id);
            *next_id += 1;
            // The saved file may have been edited by hand.
            let ratio = if ratio.is_finite() {
                ratio.clamp(MIN_RATIO, 1. - MIN_RATIO)
            } else {
                0.5
            };
            Node::Split(Box::new(Split {
                id,
                axis,
                ratio: Tween::settled(ratio),
                first: node_from(*first, next_id),
                second: node_from(*second, next_id),
                closing: None,
            }))
        }
    }
}

fn shape_of<T>(node: &Node<T>) -> Shape<&T> {
    match node {
        Node::Leaf(leaf) => Shape::Leaf(leaf),
        Node::Split(split) => match split.closing {
            Some(Side::First) => shape_of(&split.second),
            Some(Side::Second) => shape_of(&split.first),
            None => Shape::Split {
                axis: split.axis,
                ratio: split.ratio.target(),
                first: Box::new(shape_of(&split.first)),
                second: Box::new(shape_of(&split.second)),
            },
        },
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

/// Sends every split to its children's share of the panes along its axis, so `1 | 2 | 3`
/// gets thirds however it was split. A closing split keeps running its animation to zero,
/// and one already heading for its share is left alone so its tween never restarts.
fn equalize<T>(node: &mut Node<T>, now: Instant) {
    let Node::Split(split) = node else { return };
    if split.closing.is_none() {
        let first = panes_along(&split.first, split.axis);
        let share = first / (first + panes_along(&split.second, split.axis));
        if (split.ratio.target() - share).abs() > EPS {
            split
                .ratio
                .retarget(share, motion::SPLIT, Ease::OutQuint, now);
        }
    }
    equalize(&mut split.first, now);
    equalize(&mut split.second, now);
}

/// How many panes `node` lines up along `axis`: panes beside each other add up, and panes
/// stacked across it count as their longest line. A closing pane counts for none.
fn panes_along<T>(node: &Node<T>, axis: Axis) -> f32 {
    let Node::Split(split) = node else { return 1. };
    let (first, second) = (
        panes_along(&split.first, axis),
        panes_along(&split.second, axis),
    );
    match split.closing {
        Some(Side::First) => second,
        Some(Side::Second) => first,
        None if split.axis == axis => first + second,
        None => first.max(second),
    }
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

    /// Every split in the shape, in tree order: `1 | 2` then `2 over 3`.
    fn ratios<T>(node: &Node<T>, out: &mut Vec<(Axis, f32)>) {
        let Node::Split(split) = node else { return };
        out.push((split.axis, split.ratio.target()));
        ratios(&split.first, out);
        ratios(&split.second, out);
    }

    fn ratios_of(tree: &PaneTree<u32>) -> Vec<(Axis, f32)> {
        let mut out = Vec::new();
        ratios(tree.root(), &mut out);
        out
    }

    fn assert_ratios(tree: &PaneTree<u32>, expected: &[f32]) {
        let actual: Vec<f32> = ratios_of(tree)
            .into_iter()
            .map(|(_, ratio)| ratio)
            .collect();
        assert_eq!(actual.len(), expected.len(), "{actual:?}");
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }

    #[test]
    fn split_should_put_new_leaf_after_target() {
        assert_eq!(three_panes().leaves(), [&1, &2, &3]);
    }

    #[test]
    fn split_should_give_every_pane_in_a_row_an_equal_share() {
        let now = Instant::now();
        let mut tree = PaneTree::new(1);
        tree.split(&1, Axis::Horizontal, 2, now);
        // Splitting the new pane, not the first: 1 | 2 | 3 still gets thirds.
        tree.split(&2, Axis::Horizontal, 3, now);
        assert_ratios(&tree, &[1. / 3., 0.5]);
    }

    #[test]
    fn split_should_count_the_panes_across_a_stack() {
        let now = Instant::now();
        let mut tree = three_panes();
        // The user dragged the top divider; a new pane puts it back to equal shares.
        tree.set_ratio(SplitId(0), 0.3);
        tree.split(&3, Axis::Horizontal, 4, now);
        // 1 | (2 over (3 | 4)): 1, 3 and 4 each get a third of the width; 2 spans two.
        assert_ratios(&tree, &[1. / 3., 0.5, 0.5]);
    }

    #[test]
    fn equalize_should_undo_dragged_dividers() {
        let now = Instant::now();
        let mut tree = three_panes();
        tree.set_ratio(SplitId(0), 0.3);
        tree.set_ratio(SplitId(1), 0.8);
        tree.equalize(now);
        assert_ratios(&tree, &[0.5, 0.5]);
    }

    #[test]
    fn close_should_give_the_row_left_equal_shares() {
        let now = Instant::now();
        let mut tree = PaneTree::new(1);
        tree.split(&1, Axis::Horizontal, 2, now);
        tree.split(&2, Axis::Horizontal, 3, now);
        tree.split(&3, Axis::Horizontal, 4, now);
        tree.close(&4, now);
        // 1 | (2 | (3 | closing 4)): the closing pane counts for none, so three thirds.
        assert_ratios(&tree, &[1. / 3., 0.5, 1.]);
    }

    #[test]
    fn close_should_leave_the_panes_left_even() {
        let now = Instant::now();
        let mut tree = three_panes();
        tree.set_ratio(SplitId(0), 0.3);
        tree.set_ratio(SplitId(1), 0.8);
        tree.close(&3, now);
        // The closing split is gone from the way the layout is heading, and the split above
        // it shares evenly.
        assert_eq!(
            tree.shape(),
            Shape::Split {
                axis: Axis::Horizontal,
                ratio: 0.5,
                first: Box::new(Shape::Leaf(&1)),
                second: Box::new(Shape::Leaf(&2)),
            }
        );
    }

    #[test]
    fn close_should_keep_the_closing_split_animating_to_nothing() {
        let now = Instant::now();
        let mut tree = three_panes();
        tree.set_ratio(SplitId(1), 0.8);
        tree.close(&2, now);
        let ratios = ratios_of(&tree);
        assert_eq!(
            ratios[1],
            (Axis::Vertical, 0.),
            "the closing split keeps its own tween"
        );
        assert!(
            (ratios[0].1 - 0.5).abs() < f32::EPSILON,
            "the split above it is even"
        );
    }

    #[test]
    fn dragging_a_divider_should_hold_until_a_pane_changes() {
        let mut tree = three_panes();
        tree.set_ratio(SplitId(0), 0.3);
        assert!((ratios_of(&tree)[0].1 - 0.3).abs() < f32::EPSILON);
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
        assert_eq!(tree.collapse_finished(now), Vec::<u32>::new());
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
    fn from_shape_should_rebuild_the_layout_settled() {
        let tree = PaneTree::from_shape(three_panes().shape().map(&mut |leaf| *leaf));
        assert_eq!(
            (tree.leaves(), tree.is_animating(Instant::now())),
            (vec![&1, &2, &3], false)
        );
        assert_eq!(tree.neighbor(&2, Direction::Down), Some(&3));
    }

    #[test]
    fn shape_should_leave_out_a_closing_leaf() {
        let mut tree = three_panes();
        tree.close(&2, Instant::now());
        let shape = tree.shape().map(&mut |leaf| *leaf);
        let expected = Shape::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(Shape::Leaf(1)),
            second: Box::new(Shape::Leaf(3)),
        };
        assert_eq!(shape, expected);
    }

    #[test]
    fn is_animating_should_be_false_after_settling() {
        let mut tree = three_panes();
        let later = settle(&mut tree, Instant::now());
        assert!(!tree.is_animating(later));
    }
}
