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

/// How a tab's panes share its area. Leaves are pane ids.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Node {
    Leaf(usize),
    Split(Dir, Box<Node>, Box<Node>),
}

impl Node {
    /// The pane ids, in reading order.
    pub fn panes(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split(_, a, b) => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn contains(&self, id: usize) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split(_, a, b) => a.contains(id) || b.contains(id),
        }
    }

    /// Each pane's area inside `area`, halves split by a `gap` wide line.
    pub fn areas(&self, area: Area, gap: f32) -> Vec<(usize, Area)> {
        let mut out = Vec::new();
        self.lay_out(area, gap, &mut out);
        out
    }

    fn lay_out(&self, (x, y, w, h): Area, gap: f32, out: &mut Vec<(usize, Area)>) {
        match self {
            Node::Leaf(id) => out.push((*id, (x, y, w, h))),
            Node::Split(Dir::Right, a, b) => {
                let left = ((w - gap) / 2.0).floor().max(0.0);
                a.lay_out((x, y, left, h), gap, out);
                b.lay_out((x + left + gap, y, (w - left - gap).max(0.0), h), gap, out);
            }
            Node::Split(Dir::Down, a, b) => {
                let top = ((h - gap) / 2.0).floor().max(0.0);
                a.lay_out((x, y, w, top), gap, out);
                b.lay_out((x, y + top + gap, w, (h - top - gap).max(0.0)), gap, out);
            }
        }
    }

    /// Splits pane `id` in two, `new` going right of or under it.
    pub fn split(&mut self, id: usize, new: usize, dir: Dir) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == id => {
                *self = Node::Split(dir, Box::new(Node::Leaf(id)), Box::new(Node::Leaf(new)));
                true
            }
            Node::Leaf(_) => false,
            Node::Split(_, a, b) => a.split(id, new, dir) || b.split(id, new, dir),
        }
    }

    /// Takes pane `id` out; its sibling gets its space. None when `id` was
    /// the only pane, so the whole tree is gone.
    pub fn remove(self, id: usize) -> Option<Node> {
        match self {
            Node::Leaf(leaf) if leaf == id => None,
            Node::Leaf(_) => Some(self),
            Node::Split(dir, a, b) => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split(dir, Box::new(a), Box::new(b))),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
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

/// How wide each tab is drawn: equal shares, at most `max`.
pub fn tab_width(window_width: f32, count: usize, max: f32) -> f32 {
    (window_width / count.max(1) as f32)
        .min(max)
        .floor()
        .max(1.0)
}

/// The tab under `x`, if any.
pub fn tab_at(x: f64, window_width: f32, count: usize, max: f32) -> Option<usize> {
    let i = (x as f32 / tab_width(window_width, count, max)).floor();
    (i >= 0.0 && (i as usize) < count).then_some(i as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_share_the_width_up_to_a_cap() {
        assert_eq!(tab_width(1000.0, 2, 220.0), 220.0);
        assert_eq!(tab_width(300.0, 3, 220.0), 100.0);
        assert_eq!(tab_width(0.0, 3, 220.0), 1.0, "never zero");
        assert_eq!(tab_at(450.0, 1000.0, 2, 220.0), None, "past the last tab");
        assert_eq!(tab_at(230.0, 1000.0, 2, 220.0), Some(1));
        assert_eq!(tab_at(-1.0, 1000.0, 2, 220.0), None);
    }

    #[test]
    fn splits_share_the_area_with_a_gap() {
        let mut tree = Node::Leaf(0);
        assert!(tree.split(0, 1, Dir::Right));
        assert!(tree.split(1, 2, Dir::Down));
        assert!(!tree.split(9, 3, Dir::Down));
        assert_eq!(tree.panes(), [0, 1, 2]);
        let areas = tree.areas((0.0, 0.0, 101.0, 51.0), 1.0);
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
            Node::Split(Dir::Right, Box::new(Node::Leaf(0)), Box::new(Node::Leaf(2)))
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
}
