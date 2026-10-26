//! Links in the text: an app's osc 8 link on a cell, or a plain url
//! written out. Ctrl+click opens them.

use vanta::{Cell, CellKind};

/// Schemes a plain url is spotted by; only web and mail links open.
const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

/// The link at `col` of `row`: its first and last column and where it
/// goes.
pub fn link_at(row: &[Cell], col: usize) -> Option<(usize, usize, String)> {
    let here = row.get(col)?;
    if let Some(uri) = &here.hyperlink {
        // the run of cells the app gave the same link
        let same = |c: &Cell| c.hyperlink.as_ref() == Some(uri);
        let mut a = col;
        while a > 0 && same(&row[a - 1]) {
            a -= 1;
        }
        let mut b = col;
        while b + 1 < row.len() && same(&row[b + 1]) {
            b += 1;
        }
        return Some((a, b, uri.to_string()));
    }
    plain_url_at(row, col)
}

/// A url written out in the text around `col`.
fn plain_url_at(row: &[Cell], col: usize) -> Option<(usize, usize, String)> {
    // the row's text, one char per cell (wide glyphs keep their lead cell)
    let mut text: Vec<char> = Vec::with_capacity(row.len());
    let mut cells: Vec<usize> = Vec::with_capacity(row.len());
    for (i, cell) in row.iter().enumerate() {
        let c = match &cell.kind {
            CellKind::Char(c) => *c,
            CellKind::Cluster(s) => s.chars().next().unwrap_or(' '),
            CellKind::Empty => ' ',
            CellKind::Continuation => continue,
        };
        text.push(c);
        cells.push(i);
    }
    let at = cells.iter().rposition(|&i| i <= col)?;
    // back to where the token starts, then find a scheme in it
    let mut start = at;
    while start > 0 && is_url_char(text[start - 1]) {
        start -= 1;
    }
    let token: String = text[start..].iter().collect();
    let (offset, _) = SCHEMES
        .iter()
        .filter_map(|s| token.find(s).map(|i| (i, s)))
        .filter(|(i, _)| token[..*i].chars().count() + start <= at)
        .min_by_key(|(i, _)| *i)?;
    let begin = start + token[..offset].chars().count();
    let mut end = begin;
    while end < text.len() && is_url_char(text[end]) {
        end += 1;
    }
    // trailing punctuation belongs to the sentence, and a closing bracket
    // only when the url opened one
    while end > begin {
        let last = text[end - 1];
        let opened = |open: char| {
            text[begin..end].iter().filter(|&&c| c == open).count()
                >= text[begin..end].iter().filter(|&&c| c == last).count()
        };
        let keep = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => false,
            ')' => opened('('),
            ']' => opened('['),
            _ => true,
        };
        if keep {
            break;
        }
        end -= 1;
    }
    if at >= end {
        return None;
    }
    let url: String = text[begin..end].iter().collect();
    let scheme_only = SCHEMES.iter().any(|s| url == *s);
    (!scheme_only).then(|| (cells[begin], cells[end - 1], url))
}

fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^')
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
    fn plain_urls_are_found_without_their_punctuation() {
        let r = row("see https://x.dev/a?b=1, then (https://y.dev/p_(q)) ok");
        assert_eq!(link_at(&r, 10), Some((4, 22, "https://x.dev/a?b=1".into())));
        assert_eq!(link_at(&r, 4), link_at(&r, 22));
        assert_eq!(
            link_at(&r, 35).map(|l| l.2),
            Some("https://y.dev/p_(q)".into())
        );
        assert_eq!(link_at(&r, 23), None, "the comma");
        assert_eq!(link_at(&r, 0), None);
        assert_eq!(link_at(&r, 200), None, "past the text");
    }

    #[test]
    fn mail_links_and_bare_schemes() {
        let r = row("mail mailto:me@x.dev or https:// alone");
        assert_eq!(link_at(&r, 8).map(|l| l.2), Some("mailto:me@x.dev".into()));
        assert_eq!(link_at(&r, 26), None, "a scheme alone is no link");
    }

    #[test]
    fn app_links_cover_their_run() {
        let mut r = row("open docs now");
        for cell in &mut r[5..9] {
            cell.hyperlink = Some("https://docs.dev".into());
        }
        assert_eq!(link_at(&r, 6), Some((5, 8, "https://docs.dev".into())));
        assert_eq!(link_at(&r, 2), None);
    }
}
