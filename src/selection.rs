//! What the mouse selected. Lines are numbered from the very first line,
//! dropped ones included, so a selection stays on its text while output
//! pushes that text up into scrollback.

use vanta::{Cell, CellKind};

/// A line number counted that way, and a column.
pub type Spot = (u64, usize);

/// What a drag extends by: cells, whole words (double click) or whole
/// lines (triple click).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Cell,
    Word,
    Line,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub unit: Unit,
    /// What the press covered: one cell, its word or its line.
    pub anchor: (Spot, Spot),
    /// What the pointer covers now, the same way.
    pub head: (Spot, Spot),
    /// A plain click selects nothing until the pointer leaves its cell.
    pub shown: bool,
}

impl Selection {
    /// Starts at `spot`; `row` is that line's cells, for words.
    pub fn start(unit: Unit, spot: Spot, row: &[Cell]) -> Self {
        let span = span(unit, spot, row);
        Self {
            unit,
            anchor: span,
            head: span,
            shown: unit != Unit::Cell,
        }
    }

    /// Moves the free end to `spot`.
    pub fn extend(&mut self, spot: Spot, row: &[Cell]) {
        let head = span(self.unit, spot, row);
        if head != self.head {
            self.head = head;
            self.shown = true;
        }
    }

    /// First and last spot covered, both inclusive.
    pub fn range(&self) -> (Spot, Spot) {
        (
            self.anchor.0.min(self.head.0),
            self.anchor.1.max(self.head.1),
        )
    }
}

/// The spots `spot` covers in `unit`.
fn span(unit: Unit, (line, col): Spot, row: &[Cell]) -> (Spot, Spot) {
    match unit {
        Unit::Cell => ((line, col), (line, col)),
        Unit::Line => ((line, 0), (line, usize::MAX)),
        Unit::Word => {
            let (a, b) = word_span(row, col);
            ((line, a), (line, b))
        }
    }
}

/// Characters that hold a word together besides letters and digits, so a
/// path, a url or a `file:line` selects whole. kitty's set and `:`.
const WORD_CHARS: &str = "@-./_~?&=%+#:";

fn is_word(cell: &Cell) -> bool {
    match &cell.kind {
        CellKind::Char(c) => c.is_alphanumeric() || WORD_CHARS.contains(*c),
        CellKind::Cluster(s) => s.chars().next().is_some_and(char::is_alphanumeric),
        CellKind::Continuation => true,
        CellKind::Empty => false,
    }
}

/// The word around `col`: a run of word characters, or of whatever else
/// is there (spaces, punctuation) when `col` isn't on one.
pub fn word_span(row: &[Cell], col: usize) -> (usize, usize) {
    let Some(here) = row.get(col) else {
        return (col, col);
    };
    let kind = is_word(here);
    let same = |c: &Cell| is_word(c) == kind;
    let mut start = col;
    while start > 0 && same(&row[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < row.len() && same(&row[end + 1]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|c| Cell {
                kind: CellKind::Char(c),
                ..Cell::default()
            })
            .collect()
    }

    #[test]
    fn words_hold_paths_and_urls_together() {
        let r = row("cd ~/src/my-app && open https://x.dev/a?b=1");
        assert_eq!(word_span(&r, 5), (3, 14));
        assert_eq!(word_span(&r, 30), (24, 42));
        assert_eq!(word_span(&r, 15), (15, 15), "the space alone");
        assert_eq!(word_span(&r, 99), (99, 99), "past the text");
    }

    #[test]
    fn a_click_selects_nothing_until_dragged() {
        let r = row("hello world");
        let mut sel = Selection::start(Unit::Cell, (7, 2), &r);
        assert!(!sel.shown);
        sel.extend((7, 2), &r);
        assert!(!sel.shown);
        sel.extend((8, 1), &r);
        assert!(sel.shown);
        assert_eq!(sel.range(), ((7, 2), (8, 1)));
    }

    #[test]
    fn word_drags_cover_whole_words_both_ways() {
        let r = row("one two three");
        let mut sel = Selection::start(Unit::Word, (3, 5), &r);
        assert!(sel.shown);
        assert_eq!(sel.range(), ((3, 4), (3, 6)));
        sel.extend((3, 0), &r);
        assert_eq!(sel.range(), ((3, 0), (3, 6)));
        sel.extend((3, 10), &r);
        assert_eq!(sel.range(), ((3, 4), (3, 12)));
    }

    #[test]
    fn line_drags_cover_whole_lines() {
        let r = row("abc");
        let mut sel = Selection::start(Unit::Line, (2, 1), &r);
        sel.extend((4, 0), &r);
        assert_eq!(sel.range(), ((2, 0), (4, usize::MAX)));
    }
}
