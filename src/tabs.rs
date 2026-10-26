//! Tabs. The one on show lives in `App`'s own fields; the others wait here
//! with their shell and how they were being looked at, and swap in.

use std::io;

use crate::search::Search;
use crate::selection::Selection;
use crate::terminal::TerminalSession;
use crate::theme::Colors;

/// Starts a shell at `cols` x `rows`, in a directory when one is given.
pub type Spawner = Box<dyn Fn(u16, u16, Option<&str>) -> io::Result<TerminalSession>>;

/// A tab not on show.
pub struct Parked {
    pub term: TerminalSession,
    pub scroll_offset: usize,
    pub selection: Option<Selection>,
    pub search: Option<Search>,
    pub colors: Colors,
    pub palette_version: u64,
    /// It rang while hidden.
    pub bell: bool,
}

impl Parked {
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

/// How wide each tab is drawn: equal shares, at most `max`.
pub fn tab_width(window_width: f32, count: usize, max: f32) -> f32 {
    (window_width / count.max(1) as f32).min(max).floor()
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
        assert_eq!(tab_at(450.0, 1000.0, 2, 220.0), None, "past the last tab");
        assert_eq!(tab_at(230.0, 1000.0, 2, 220.0), Some(1));
        assert_eq!(tab_at(-1.0, 1000.0, 2, 220.0), None);
    }
}
