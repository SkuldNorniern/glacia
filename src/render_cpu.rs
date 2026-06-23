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

fn draw_row(
    ctx: &mut dyn DrawingContext,
    row: &[Cell],
    y_top: f32,
    baseline: f32,
    metrics: &CellMetrics,
    font: &Font,
    bold_font: &Font,
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
        let start = i;
        let mut text = String::new();
        while i < row.len()
            && resolve_pair(&row[i]).0 == fg
            && row[i].attrs.contains(Attrs::BOLD) == bold
        {
            push_cell_text(&mut text, &row[i]);
            i += 1;
        }
        if text.trim_end().is_empty() {
            continue;
        }
        let x = start as f32 * metrics.width;
        let run_font = if bold { bold_font } else { font };
        ctx.draw_text_with_font(&text, Point::new(x, baseline), run_font, &solid(fg))?;
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
    font: &Font,
) -> AureaResult<()> {
    ctx.clear(theme::BACKGROUND)?;

    let bold_font = Font {
        weight: FontWeight::Bold,
        ..font.clone()
    };
    let line_h = metrics.height;
    let baseline_offset = line_h * 0.8;

    for (row_idx, row) in rows.iter().enumerate() {
        let y_top = row_idx as f32 * line_h;
        draw_row(
            ctx,
            row,
            y_top,
            y_top + baseline_offset,
            metrics,
            font,
            &bold_font,
        )?;
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
