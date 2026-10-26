//! Finding text in scrollback: the query, every match on its lines, and
//! which one is current. Lines are numbered like a selection's, with
//! vanta's `dropped` added, so matches stay put while output arrives.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use vanta::Found;

use crate::selection::Spot;

/// Least time between searches run because output came in.
pub const RESEARCH_EVERY: Duration = Duration::from_millis(300);

pub struct Search {
    pub query: String,
    found: Vec<(Spot, Spot)>,
    current: Option<usize>,
    /// A search running on vanta's side.
    pub pending: Option<Receiver<(u64, Vec<Found>)>>,
    /// The matches are behind: the query, the text or its wrapping changed.
    pub stale: bool,
    /// Show the current match once the results are in (the query changed).
    pub jump: bool,
    pub done_at: Instant,
    /// Bumped when the query changes, and what the running search was
    /// started for: results for an older query are dropped.
    pub query_gen: u64,
    pub pending_gen: u64,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            query: String::new(),
            found: Vec::new(),
            current: None,
            pending: None,
            stale: false,
            jump: false,
            done_at: Instant::now(),
            query_gen: 0,
            pending_gen: 0,
        }
    }
}

/// A piece of a match on one row of the view: row, first and last column,
/// and whether it is the current match.
pub type Piece = (usize, usize, usize, bool);

impl Search {
    /// Takes new matches from vanta (found when `dropped` lines were gone).
    /// The current match stays current when it is still there; else the
    /// newest one is, the one nearest the prompt.
    pub fn set_found(&mut self, dropped: u64, found: &[Found]) {
        let was = self.current();
        let spot = |(line, col): (usize, usize)| (dropped + line as u64, col);
        self.found = found.iter().map(|f| (spot(f.start), spot(f.end))).collect();
        self.current = was
            .filter(|_| !self.jump)
            .and_then(|was| self.found.iter().position(|m| *m == was))
            .or_else(|| self.found.len().checked_sub(1));
        self.done_at = Instant::now();
    }

    /// Whether a new search should start now.
    pub fn due(&self) -> bool {
        self.stale
            && self.pending.is_none()
            && (self.jump || self.done_at.elapsed() >= RESEARCH_EVERY)
    }

    /// Steps to the next older match, or newer; wraps round.
    pub fn step(&mut self, older: bool) {
        let n = self.found.len();
        self.current = self
            .current
            .map(|i| if older { (i + n - 1) % n } else { (i + 1) % n });
    }

    /// The current match.
    pub fn current(&self) -> Option<(Spot, Spot)> {
        self.found.get(self.current?).copied()
    }

    /// `3/17`, `0/0`: where the current match is among all of them,
    /// counted from the oldest.
    pub fn count(&self) -> (usize, usize) {
        (self.current.map_or(0, |i| i + 1), self.found.len())
    }

    /// The matches on view, cut into one piece per row. `top` is the line
    /// number of the view's first row.
    pub fn pieces(&self, top: u64, rows: usize) -> Vec<Piece> {
        let bottom = top + rows as u64;
        let mut out = Vec::new();
        // matches are oldest first: skip to the first that can show
        let from = self.found.partition_point(|(_, end)| end.0 < top);
        for (i, &((sl, sc), (el, ec))) in self.found.iter().enumerate().skip(from) {
            if sl >= bottom {
                break;
            }
            let current = Some(i) == self.current;
            for line in sl.max(top)..=el.min(bottom - 1) {
                let a = if line == sl { sc } else { 0 };
                let b = if line == el { ec } else { usize::MAX };
                out.push(((line - top) as usize, a, b, current));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(start: (usize, usize), end: (usize, usize)) -> Found {
        Found { start, end }
    }

    #[test]
    fn the_newest_match_comes_first_and_steps_wrap() {
        let mut s = Search::default();
        s.set_found(10, &[found((0, 0), (0, 2)), found((5, 1), (5, 3))]);
        assert_eq!(s.current(), Some(((15, 1), (15, 3))));
        assert_eq!(s.count(), (2, 2));
        s.step(true);
        assert_eq!(s.count(), (1, 2));
        s.step(true);
        assert_eq!(s.count(), (2, 2), "wrapped round");
        s.step(false);
        assert_eq!(s.count(), (1, 2));
        // new results keep the current match when it is still there
        s.set_found(
            10,
            &[
                found((0, 0), (0, 2)),
                found((5, 1), (5, 3)),
                found((7, 0), (7, 1)),
            ],
        );
        assert_eq!(s.count(), (1, 3));
        s.jump = true;
        s.set_found(10, &[found((0, 0), (0, 2)), found((5, 1), (5, 3))]);
        assert_eq!(s.count(), (2, 2), "a new query starts from the newest");
        s.set_found(0, &[]);
        assert_eq!((s.current(), s.count()), (None, (0, 0)));
        s.step(true);
    }

    #[test]
    fn pieces_cut_matches_to_the_view() {
        let mut s = Search::default();
        s.set_found(
            0,
            &[
                found((1, 0), (1, 1)),
                found((4, 6), (5, 2)),
                found((9, 0), (9, 0)),
            ],
        );
        // view of lines 3..7
        assert_eq!(
            s.pieces(3, 4),
            [(1, 6, usize::MAX, false), (2, 0, 2, false)]
        );
        assert_eq!(s.pieces(8, 4), [(1, 0, 0, true)]);
        assert!(s.pieces(2, 1).is_empty());
    }
}
