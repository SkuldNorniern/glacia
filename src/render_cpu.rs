//! CPU drawing of the terminal grid. Concrete implementation, no renderer
//! trait — see `PLAN.md`'s "UI Layering" note on when that's introduced.

use aurea::AureaResult;
use aurea::render::{Color, DrawingContext, Font, FontWeight, Paint, PaintStyle, Point, Rect};
use vanta::Color as TermColor;
use vanta::vt::Attrs;
use vanta::{Cell, CellKind};

use crate::theme;

pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
}

/// Font set for one rendered frame: primary, bold, and their fallback
/// counterparts for CJK/emoji runs.
///
/// Built once from the loaded `Font` objects and reused every frame — avoids
/// cloning font family strings on the hot path.
pub struct RowFonts {
    pub primary: Font,
    bold: Font,
    pub fallback: Option<Font>,
    bold_fallback: Option<Font>,
}

impl RowFonts {
    pub fn new(primary: Font, fallback: Option<Font>) -> Self {
        let bold = Font {
            weight: FontWeight::Bold,
            ..primary.clone()
        };
        let bold_fallback = fallback.as_ref().map(|f| Font {
            weight: FontWeight::Bold,
            ..f.clone()
        });
        Self {
            primary,
            bold,
            fallback,
            bold_fallback,
        }
    }

    fn pick(&self, bold: bool, use_fallback: bool) -> &Font {
        match (bold, use_fallback) {
            (true, true) => self.bold_fallback.as_ref().unwrap_or(&self.bold),
            (true, false) => &self.bold,
            (false, true) => self.fallback.as_ref().unwrap_or(&self.primary),
            (false, false) => &self.primary,
        }
    }
}

fn solid(color: Color) -> Paint {
    Paint {
        color,
        style: PaintStyle::Fill,
        ..Default::default()
    }
}

/// Resolve a cell's effective (fg, bg) pair, honoring reverse video.
fn resolve_pair(cell: &Cell) -> (Color, Option<Color>) {
    if cell.attrs.contains(Attrs::INVERSE) {
        return (
            theme::resolve(cell.bg, theme::BACKGROUND),
            Some(theme::resolve(cell.fg, theme::FOREGROUND)),
        );
    }
    let bg = match cell.bg {
        TermColor::Default => None,
        other => Some(theme::resolve(other, theme::BACKGROUND)),
    };
    (theme::resolve(cell.fg, theme::FOREGROUND), bg)
}

/// Append a cell's display text. Continuation cells (the right half of a
/// width-2 glyph) contribute nothing — they're never drawn directly.
fn push_cell_text(text: &mut String, cell: &Cell) {
    match &cell.kind {
        CellKind::Char(c) => text.push(*c),
        CellKind::Cluster(s) => text.push_str(s),
        CellKind::Empty => text.push(' '),
        CellKind::Continuation => {}
    }
}

/// Whether a character is in a Unicode range typically absent from Western
/// monospace fonts (CJK, Hangul, emoji, fullwidth forms, etc.). Used to
/// route such runs to the configured fallback font.
fn prefers_fallback(c: char) -> bool {
    let n = c as u32;
    matches!(
        n,
        0x1100..=0x11FF   // Hangul Jamo
        | 0x2E80..=0x303F // CJK Radicals, Kangxi, Symbols & Punctuation
        | 0x3040..=0x9FFF // Kana, Bopomofo, CJK unified block
        | 0xA000..=0xA4CF // Yi
        | 0xA960..=0xA97F // Hangul Jamo Extended-A
        | 0xAC00..=0xD7FF // Hangul Syllables + Jamo Extended-B
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F // CJK Compatibility Forms
        | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
        | 0x1B000..=0x1B0FF // Kana Supplement
        | 0x1F300..=0x1FAFF // Emoji and pictographs
    )
}

/// True if the cell's content should use the fallback font.
fn cell_prefers_fallback(cell: &Cell) -> bool {
    match &cell.kind {
        CellKind::Char(c) => prefers_fallback(*c),
        CellKind::Cluster(s) => s.chars().any(prefers_fallback),
        _ => false,
    }
}

fn draw_row(
    ctx: &mut dyn DrawingContext,
    row: &[Cell],
    y_top: f32,
    baseline: f32,
    metrics: &CellMetrics,
    fonts: &RowFonts,
) -> AureaResult<()> {
    for (i, cell) in row.iter().enumerate() {
        if let (_, Some(bg)) = resolve_pair(cell) {
            let x = i as f32 * metrics.width;
            ctx.draw_rect(
                Rect::new(x, y_top, metrics.width, metrics.height),
                &solid(bg),
            )?;
        }
    }

    let mut i = 0usize;
    while i < row.len() {
        let (fg, _) = resolve_pair(&row[i]);
        let bold = row[i].attrs.contains(Attrs::BOLD);
        let use_fallback = cell_prefers_fallback(&row[i]);
        let start = i;
        let mut text = String::new();
        while i < row.len()
            && resolve_pair(&row[i]).0 == fg
            && row[i].attrs.contains(Attrs::BOLD) == bold
            && cell_prefers_fallback(&row[i]) == use_fallback
        {
            push_cell_text(&mut text, &row[i]);
            i += 1;
        }
        if text.trim_end().is_empty() {
            continue;
        }
        let x = start as f32 * metrics.width;
        ctx.draw_text_with_font(
            &text,
            Point::new(x, baseline),
            fonts.pick(bold, use_fallback),
            &solid(fg),
        )?;
    }

    Ok(())
}

/// Draw the full visible grid, then the cursor on top if `cursor_visible`.
pub fn draw_grid(
    ctx: &mut dyn DrawingContext,
    rows: &[Vec<Cell>],
    cursor: (usize, usize),
    cursor_visible: bool,
    metrics: &CellMetrics,
    fonts: &RowFonts,
) -> AureaResult<()> {
    ctx.clear(theme::BACKGROUND)?;

    let line_h = metrics.height;
    let baseline_offset = line_h * 0.8;

    for (row_idx, row) in rows.iter().enumerate() {
        let y_top = row_idx as f32 * line_h;
        draw_row(ctx, row, y_top, y_top + baseline_offset, metrics, fonts)?;
    }

    if cursor_visible {
        let (row, col) = cursor;
        let x = col as f32 * metrics.width;
        let y = row as f32 * line_h;
        ctx.draw_rect(
            Rect::new(x, y, metrics.width, line_h),
            &solid(theme::CURSOR),
        )?;
    }

    Ok(())
}

/// Draw a one-line diagnostics banner pinned to the bottom of the window —
/// used for non-fatal startup notices (bad config, etc.).
pub fn draw_diagnostics_banner(
    ctx: &mut dyn DrawingContext,
    message: &str,
    width: f32,
    height: f32,
    font: &Font,
) -> AureaResult<()> {
    let banner_height = 24.0;
    let y = height - banner_height;
    ctx.draw_rect(
        Rect::new(0.0, y, width, banner_height),
        &solid(Color::rgb(60, 40, 20)),
    )?;
    ctx.draw_text_with_font(
        message,
        Point::new(8.0, y + banner_height * 0.7),
        font,
        &solid(Color::rgb(247, 207, 109)),
    )?;
    Ok(())
}

/// Full-window message for fatal startup failures (e.g. the shell couldn't
/// spawn) — there's no terminal grid to draw alongside it.
pub fn draw_fatal_message(
    ctx: &mut dyn DrawingContext,
    message: &str,
    font: &Font,
) -> AureaResult<()> {
    ctx.clear(theme::BACKGROUND)?;
    ctx.draw_text_with_font(
        message,
        Point::new(16.0, 32.0),
        font,
        &solid(Color::rgb(255, 107, 107)),
    )?;
    Ok(())
}
