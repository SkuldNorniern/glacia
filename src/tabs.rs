//! Tabs and the panes split inside them. The pane with the keyboard lives
//! in `App`'s own fields; every other pane waits in a store with its shell
//! and how it was being looked at, and swaps in when it gets the keyboard.

use std::io;

use crate::search::Search;
use crate::selection::Selection;
use crate::terminal::TerminalSession;
use crate::theme::Colors;

/// Starts a shell at `cols` x `rows`, in a directory when one is given.
pub type Spawner = Box<dyn Fn(u16, u16, Option<&str>) -> io::Result<TerminalSession>>;

/// A pane without the keyboard.
pub struct Pane {
    pub term: TerminalSession,
    pub scroll_offset: usize,
    pub selection: Option<Selection>,
    pub search: Option<Search>,
    pub colors: Colors,
    pub palette_version: u64,
    /// It rang while it didn't have the keyboard.
    pub bell: bool,
}

impl Pane {
    pub fn new(term: TerminalSession) -> Self {
        Self {
            term,
            scroll_offset: 0,
            selection: None,
            search: None,
            colors: Colors::theme(),
            palette_version: 0,
            bell: false,
        }
    }
}

/// A window rectangle: x, y, width, height.
pub type Area = (f32, f32, f32, f32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// side by side, the new pane on the right
    Right,
    /// stacked, the new pane below
    Down,
}

/// Smallest share a split gives either side.
const MIN_RATIO: f32 = 0.1;

/// How a tab's panes share its area. Leaves are pane ids; a split gives
/// `ratio` of its area to `a`, the rest to `b`.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    Leaf(usize),
    Split {
        dir: Dir,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// Where a split's dividing line is: its area, the split it belongs to
/// (as a path of a/b turns from the root, false is `a`), and the area the
/// split shares out.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    pub line: Area,
    pub path: Vec<bool>,
    pub dir: Dir,
    pub parent: Area,
}

impl Node {
    fn split_of(dir: Dir, a: Node, b: Node) -> Self {
        Node::Split {
            dir,
            ratio: 0.5,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// The pane ids, in reading order.
    pub fn panes(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn contains(&self, id: usize) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// Each pane's area inside `area`, split by `gap` wide lines.
    pub fn areas(&self, area: Area, gap: f32) -> Vec<(usize, Area)> {
        let mut out = Vec::new();
        self.lay_out(area, gap, &mut Vec::new(), &mut out, &mut Vec::new());
        out
    }

    /// The lines between panes, for dragging.
    pub fn dividers(&self, area: Area, gap: f32) -> Vec<Divider> {
        let mut out = Vec::new();
        self.lay_out(area, gap, &mut Vec::new(), &mut Vec::new(), &mut out);
        out
    }

    fn lay_out(
        &self,
        (x, y, w, h): Area,
        gap: f32,
        path: &mut Vec<bool>,
        out: &mut Vec<(usize, Area)>,
        lines: &mut Vec<Divider>,
    ) {
        let Node::Split { dir, ratio, a, b } = self else {
            if let Node::Leaf(id) = self {
                out.push((*id, (x, y, w, h)));
            }
            return;
        };
        let (first, line, second) = match dir {
            Dir::Right => {
                let left = ((w - gap) * ratio).floor().max(0.0);
                (
                    (x, y, left, h),
                    (x + left, y, gap, h),
                    (x + left + gap, y, (w - left - gap).max(0.0), h),
                )
            }
            Dir::Down => {
                let top = ((h - gap) * ratio).floor().max(0.0);
                (
                    (x, y, w, top),
                    (x, y + top, w, gap),
                    (x, y + top + gap, w, (h - top - gap).max(0.0)),
                )
            }
        };
        lines.push(Divider {
            line,
            path: path.clone(),
            dir: *dir,
            parent: (x, y, w, h),
        });
        path.push(false);
        a.lay_out(first, gap, path, out, lines);
        path.pop();
        path.push(true);
        b.lay_out(second, gap, path, out, lines);
        path.pop();
    }

    /// Splits pane `id` in two, `new` going right of or under it.
    pub fn split(&mut self, id: usize, new: usize, dir: Dir) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == id => {
                *self = Node::split_of(dir, Node::Leaf(id), Node::Leaf(new));
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(id, new, dir) || b.split(id, new, dir),
        }
    }

    /// Takes pane `id` out; its sibling gets its space. None when `id` was
    /// the only pane, so the whole tree is gone.
    pub fn remove(self, id: usize) -> Option<Node> {
        match self {
            Node::Leaf(leaf) if leaf == id => None,
            Node::Leaf(_) => Some(self),
            Node::Split { dir, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Moves by `delta` of its area the line of the innermost `dir` split
    /// holding pane `id`: the line the pane shares with its closest
    /// neighbour that way. False when no such split holds the pane.
    pub fn nudge(&mut self, id: usize, dir: Dir, delta: f32) -> bool {
        let Node::Split {
            dir: own,
            ratio,
            a,
            b,
        } = self
        else {
            return false;
        };
        if !a.contains(id) && !b.contains(id) {
            return false;
        }
        // the innermost line wins
        if a.nudge(id, dir, delta) || b.nudge(id, dir, delta) {
            return true;
        }
        if *own != dir {
            return false;
        }
        *ratio = (*ratio + delta).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        true
    }

    /// Sets the split at `path` to `ratio`.
    pub fn set_ratio(&mut self, path: &[bool], value: f32) {
        let Node::Split { ratio, a, b, .. } = self else {
            return;
        };
        match path.split_first() {
            None => *ratio = value.clamp(MIN_RATIO, 1.0 - MIN_RATIO),
            Some((false, rest)) => a.set_ratio(rest, value),
            Some((true, rest)) => b.set_ratio(rest, value),
        }
    }
}

/// The pane next to `from` in the arrow's direction (`dx`, `dy` each -1,
/// 0 or 1): the nearest one that overlaps it on the other axis.
pub fn neighbour(areas: &[(usize, Area)], from: usize, (dx, dy): (i8, i8)) -> Option<usize> {
    let &(_, (fx, fy, fw, fh)) = areas.iter().find(|(id, _)| *id == from)?;
    areas
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|&(id, (x, y, w, h))| {
            let overlaps_y = y < fy + fh && fy < y + h;
            let overlaps_x = x < fx + fw && fx < x + w;
            let distance = match (dx, dy) {
                (1, 0) if overlaps_y && x >= fx + fw => x - (fx + fw),
                (-1, 0) if overlaps_y && x + w <= fx => fx - (x + w),
                (0, 1) if overlaps_x && y >= fy + fh => y - (fy + fh),
                (0, -1) if overlaps_x && y + h <= fy => fy - (y + h),
                _ => return None,
            };
            Some((id, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// Widest a tab is drawn.
pub const TAB_MAX: f32 = 220.0;
/// Narrowest a tab still shows its close button at.
const CLOSE_MIN: f32 = 64.0;

/// What is under the pointer on the tab bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarHit {
    Tab(usize),
    Close(usize),
    New,
}

/// Where things sit on the tab bar: tabs share the width left of the new
/// tab button, each at most `TAB_MAX`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bar {
    pub tab_w: f32,
    pub count: usize,
    pub height: f32,
}

impl Bar {
    pub fn new(window_width: f32, count: usize, height: f32) -> Self {
        let room = (window_width - height).max(1.0);
        Self {
            tab_w: (room / count.max(1) as f32).min(TAB_MAX).floor().max(1.0),
            count,
            height,
        }
    }

    pub fn slot_x(&self, index: usize) -> f32 {
        index as f32 * self.tab_w
    }

    /// The new tab button, right after the last tab.
    pub fn new_button(&self) -> Area {
        (self.slot_x(self.count), 0.0, self.height, self.height)
    }

    /// A tab's close button, when the tab is wide enough to hold one and
    /// isn't the only tab (the last one stays).
    pub fn close_button(&self, x: f32) -> Option<Area> {
        let size = (self.height * 0.6).round();
        (self.tab_w >= CLOSE_MIN && self.count > 1).then(|| {
            (
                x + self.tab_w - size - 6.0,
                ((self.height - size) / 2.0).round(),
                size,
                size,
            )
        })
    }

    pub fn hit(&self, x: f64, y: f64) -> Option<BarHit> {
        let (x, y) = (x as f32, y as f32);
        let inside = |(ax, ay, w, h): Area| x >= ax && y >= ay && x < ax + w && y < ay + h;
        if y < 0.0 || y >= self.height || x < 0.0 {
            return None;
        }
        if inside(self.new_button()) {
            return Some(BarHit::New);
        }
        let i = (x / self.tab_w).floor() as usize;
        if i >= self.count {
            return None;
        }
        match self.close_button(self.slot_x(i)) {
            Some(close) if inside(close) => Some(BarHit::Close(i)),
            _ => Some(BarHit::Tab(i)),
        }
    }

    /// Where a tab dragged with its left edge at `x` drops: the slot its
    /// middle is over.
    pub fn drop_slot(&self, x: f32) -> usize {
        let i = ((x + self.tab_w / 2.0) / self.tab_w).floor().max(0.0) as usize;
        i.min(self.count.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_shares_its_width_and_finds_what_is_hit() {
        let bar = Bar::new(1028.0, 2, 28.0);
        assert_eq!(bar.tab_w, 220.0);
        assert_eq!(bar.new_button(), (440.0, 0.0, 28.0, 28.0));
        assert_eq!(bar.hit(10.0, 10.0), Some(BarHit::Tab(0)));
        assert_eq!(bar.hit(230.0, 10.0), Some(BarHit::Tab(1)));
        assert_eq!(bar.hit(420.0, 14.0), Some(BarHit::Close(1)));
        assert_eq!(bar.hit(450.0, 10.0), Some(BarHit::New));
        assert_eq!(bar.hit(600.0, 10.0), None, "empty bar");
        assert_eq!(bar.hit(10.0, 40.0), None, "under the bar");
        let tight = Bar::new(328.0, 6, 28.0);
        assert_eq!(tight.tab_w, 50.0);
        assert_eq!(
            tight.close_button(0.0),
            None,
            "too narrow for a close button"
        );
        assert_eq!(Bar::new(0.0, 3, 28.0).tab_w, 1.0, "never zero");
        assert_eq!(
            Bar::new(1028.0, 1, 28.0).close_button(0.0),
            None,
            "the last tab stays"
        );
    }

    #[test]
    fn a_dragged_tab_drops_where_its_middle_is() {
        let bar = Bar::new(1028.0, 3, 28.0);
        assert_eq!(bar.drop_slot(0.0), 0);
        assert_eq!(bar.drop_slot(100.0), 0);
        assert_eq!(bar.drop_slot(120.0), 1);
        assert_eq!(bar.drop_slot(-50.0), 0);
        assert_eq!(bar.drop_slot(5000.0), 2);
    }

    #[test]
    fn splits_share_the_area_with_a_gap() {
        let mut tree = Node::Leaf(0);
        assert!(tree.split(0, 1, Dir::Right));
        assert!(tree.split(1, 2, Dir::Down));
        assert!(!tree.split(9, 3, Dir::Down));
        assert_eq!(tree.panes(), [0, 1, 2]);
        let areas = tree.areas((0.0, 0.0, 101.0, 51.0), 1.0);
        assert_eq!(tree.dividers((0.0, 0.0, 101.0, 51.0), 1.0).len(), 2);
        assert_eq!(
            areas,
            [
                (0, (0.0, 0.0, 50.0, 51.0)),
                (1, (51.0, 0.0, 50.0, 25.0)),
                (2, (51.0, 26.0, 50.0, 25.0)),
            ]
        );
    }

    #[test]
    fn removing_a_pane_gives_its_space_to_the_sibling() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Dir::Right);
        tree.split(1, 2, Dir::Down);
        let tree = tree.remove(1).unwrap();
        assert_eq!(
            tree,
            Node::split_of(Dir::Right, Node::Leaf(0), Node::Leaf(2))
        );
        let tree = tree.remove(0).unwrap();
        assert_eq!(tree, Node::Leaf(2));
        assert_eq!(tree.remove(2), None);
    }

    #[test]
    fn arrows_find_the_pane_beside() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Dir::Right);
        tree.split(1, 2, Dir::Down);
        let areas = tree.areas((0.0, 0.0, 101.0, 51.0), 1.0);
        assert_eq!(neighbour(&areas, 0, (1, 0)), Some(1));
        assert_eq!(neighbour(&areas, 2, (-1, 0)), Some(0));
        assert_eq!(neighbour(&areas, 1, (0, 1)), Some(2));
        assert_eq!(neighbour(&areas, 2, (0, -1)), Some(1));
        assert_eq!(neighbour(&areas, 0, (-1, 0)), None);
    }

    #[test]
    fn lines_move_and_stay_inside() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Dir::Right);
        tree.split(1, 2, Dir::Down);
        // the stacked line holds pane 2; across, the side by side one
        assert!(tree.nudge(2, Dir::Right, 0.2));
        assert!(tree.nudge(2, Dir::Down, -0.1));
        let areas = tree.areas((0.0, 0.0, 101.0, 101.0), 1.0);
        assert_eq!(areas[0].1.2, 70.0, "left pane grew");
        assert_eq!(areas[1].1.3, 40.0, "top right pane shrank");
        for _ in 0..20 {
            tree.nudge(0, Dir::Right, 0.1);
        }
        let areas = tree.areas((0.0, 0.0, 101.0, 101.0), 1.0);
        assert_eq!(areas[0].1.2, 90.0, "held at the edge");
        assert!(!Node::Leaf(0).nudge(0, Dir::Right, 0.1));
        tree.set_ratio(&[], 0.25);
        tree.set_ratio(&[true], 0.75);
        let areas = tree.areas((0.0, 0.0, 101.0, 101.0), 1.0);
        assert_eq!(areas[0].1.2, 25.0);
        assert_eq!(areas[1].1.3, 75.0);
    }
}
