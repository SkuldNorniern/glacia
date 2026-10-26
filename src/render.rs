//! Drawing of the terminal grid, the same on the CPU and the GPU canvas.

use std::cell::RefCell;
use std::collections::HashMap;

use aurea::AureaResult;
use aurea::render::{
    Color, DrawingContext, Font, FontStyle, FontWeight, LineJoin, Paint, PaintStyle, Path,
    PathCommand, Point, Rect,
};
use vanta::Color as TermColor;
use vanta::vt::Attrs;
use vanta::{Cell, CellKind};

use crate::config::CursorShape;
use crate::search::Piece;
use crate::tabs::Bar;
use crate::theme::{self, Colors};
use crate::unicode::{compose_hangul_jamo, is_hangul_jamo};

/// Normalized selection range in screen-cell coordinates.
/// `start` is always ≤ `end` in row-major order after construction via [`SelectionRange::new`].
#[derive(Clone, Copy)]
pub struct SelectionRange {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl SelectionRange {
    /// Build a normalized range: if `a > b` they are swapped so `start ≤ end`.
    pub fn new(a: (usize, usize), b: (usize, usize)) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    /// Column range `[start_col, end_col]` for a given row, or `None` if the
    /// row falls outside the selection.
    pub fn cols_for_row(&self, row: usize, row_len: usize) -> Option<(usize, usize)> {
        let (r1, c1) = self.start;
        let (r2, c2) = self.end;
        if row < r1 || row > r2 {
            return None;
        }
        let sc = if row == r1 { c1 } else { 0 };
        let ec = if row == r2 {
            c2
        } else {
            row_len.saturating_sub(1)
        };
        Some((sc, ec))
    }
}

pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of a cell row to the text baseline, derived from
    /// the actual font ascent rather than a fixed fraction of `height`.
    pub baseline_offset: f32,
    /// Raw font ascent (pixels), used to position underline and strikethrough.
    pub ascent: f32,
}

/// most cells a side; a tiny font in a huge window can't ask for more.
const MAX_CELLS: usize = 1000;

/// where the grid sits in the window: whole cells only, and the space left
/// past the last cell split evenly on both sides, so the margins match.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// left edge of column 0, whole pixels.
    pub x: f32,
    /// top edge of row 0, whole pixels.
    pub y: f32,
    pub cols: usize,
    pub rows: usize,
}

impl Layout {
    pub fn new(width: u32, height: u32, metrics: &CellMetrics, padding: u32) -> Self {
        let room_w = width.saturating_sub(2 * padding) as f32;
        let room_h = height.saturating_sub(2 * padding) as f32;
        let cols = ((room_w / metrics.width) as usize).clamp(1, MAX_CELLS);
        let rows = ((room_h / metrics.height) as usize).clamp(1, MAX_CELLS);
        let spare_w = (room_w - cols as f32 * metrics.width).max(0.0);
        let spare_h = (room_h - rows as f32 * metrics.height).max(0.0);
        Self {
            x: (padding as f32 + spare_w / 2.0).floor(),
            y: (padding as f32 + spare_h / 2.0).floor(),
            cols,
            rows,
        }
    }

    /// left edge of column `col`, snapped to a pixel so neighbouring cell
    /// backgrounds meet with no seam or blur.
    pub fn cell_x(&self, col: usize, metrics: &CellMetrics) -> f32 {
        (self.x + col as f32 * metrics.width).round()
    }

    pub fn row_y(&self, row: usize, metrics: &CellMetrics) -> f32 {
        self.y + row as f32 * metrics.height
    }

    /// whether a window point is on the cells, not in the margin.
    pub fn contains(&self, x: f64, y: f64, metrics: &CellMetrics) -> bool {
        let (x, y) = (x as f32, y as f32);
        x >= self.x
            && y >= self.y
            && x < self.cell_x(self.cols, metrics)
            && y < self.row_y(self.rows, metrics)
    }

    /// the `(row, col)` under a window point, held inside the grid.
    pub fn cell_at(&self, x: f64, y: f64, metrics: &CellMetrics) -> (usize, usize) {
        let col = ((x as f32 - self.x) / metrics.width).max(0.0) as usize;
        let row = ((y as f32 - self.y) / metrics.height).max(0.0) as usize;
        (row.min(self.rows - 1), col.min(self.cols - 1))
    }
}

/// Font set for one rendered frame: primary, bold, italic, bold-italic, and
/// their fallback counterparts for CJK/emoji runs.
///
/// Built once from the loaded `Font` objects and reused every frame — avoids
/// cloning font family strings on the hot path. Call [`RowFonts::probe`] inside
/// a canvas draw closure after construction to auto-detect characters the
/// primary font cannot render.
pub struct RowFonts {
    pub primary: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
    fallbacks: Vec<FontSet>,
    support_cache: RefCell<HashMap<(usize, u32), bool>>,
    notdef_cache: RefCell<HashMap<usize, Option<f32>>>,
}

struct FontSet {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

impl RowFonts {
    pub fn new(primary: Font, fallbacks: Vec<Font>) -> Self {
        let bold = Font {
            weight: FontWeight::Bold,
            ..primary.clone()
        };
        let italic = Font {
            style: FontStyle::Italic,
            ..primary.clone()
        };
        let bold_italic = Font {
            weight: FontWeight::Bold,
            style: FontStyle::Italic,
            ..primary.clone()
        };
        let fallbacks = fallbacks
            .into_iter()
            .map(|regular| FontSet {
                bold: Font {
                    weight: FontWeight::Bold,
                    ..regular.clone()
                },
                italic: Font {
                    style: FontStyle::Italic,
                    ..regular.clone()
                },
                bold_italic: Font {
                    weight: FontWeight::Bold,
                    style: FontStyle::Italic,
                    ..regular.clone()
                },
                regular,
            })
            .collect();
        Self {
            primary,
            bold,
            italic,
            bold_italic,
            fallbacks,
            support_cache: RefCell::new(HashMap::new()),
            notdef_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Prime glyph-support caches for common terminal and multilingual text.
    /// Any character not listed here is still resolved lazily during drawing.
    pub fn probe(&self, ctx: &mut dyn DrawingContext, _cell_width: f32) {
        for ch in [
            '\u{2500}',
            '\u{2502}',
            '\u{250C}',
            '\u{2510}',
            '\u{2514}',
            '\u{2518}',
            '\u{2588}',
            '\u{2591}',
            '\u{28FF}',
            '\u{2713}',
            '\u{03BB}',
            '\u{D55C}',
            '\u{AE00}',
            '\u{8A9E}',
            '\u{3042}',
            '\u{1F642}',
        ] {
            let _ = self.font_slot_for_chars(ctx, [ch]);
        }
    }

    fn pick(&self, bold: bool, italic: bool, slot: usize) -> &Font {
        if slot == 0 {
            return match (bold, italic) {
                (true, true) => &self.bold_italic,
                (false, true) => &self.italic,
                (true, false) => &self.bold,
                (false, false) => &self.primary,
            };
        }
        let set = &self.fallbacks[slot - 1];
        match (bold, italic) {
            (true, true) => &set.bold_italic,
            (false, true) => &set.italic,
            (true, false) => &set.bold,
            (false, false) => &set.regular,
        }
    }

    fn font_slot_for_cell(&self, ctx: &mut dyn DrawingContext, cell: &Cell) -> usize {
        match &cell.kind {
            CellKind::Char(c) => self.font_slot_for_chars(ctx, [*c]),
            CellKind::Cluster(s) => self.font_slot_for_chars(ctx, s.chars()),
            _ => 0,
        }
    }

    fn font_slot_for_chars<I>(&self, ctx: &mut dyn DrawingContext, chars: I) -> usize
    where
        I: IntoIterator<Item = char> + Clone,
    {
        let start_slot =
            if self.fallbacks.is_empty() || !chars.clone().into_iter().any(prefers_fallback_font) {
                0
            } else {
                1
            };

        for slot in start_slot..=self.fallbacks.len() {
            if chars
                .clone()
                .into_iter()
                .filter(|c| {
                    !c.is_control() && !is_zero_width_joiner(*c) && !is_variation_selector(*c)
                })
                .all(|c| self.font_supports(ctx, slot, c))
            {
                return slot;
            }
        }
        0
    }

    fn font_supports(&self, ctx: &mut dyn DrawingContext, slot: usize, ch: char) -> bool {
        if ch.is_ascii() {
            return true;
        }
        let key = (slot, ch as u32);
        if let Some(supported) = self.support_cache.borrow().get(&key) {
            return *supported;
        }

        let supported = self.measure_support(ctx, slot, ch);
        self.support_cache.borrow_mut().insert(key, supported);
        supported
    }

    fn measure_support(&self, ctx: &mut dyn DrawingContext, slot: usize, ch: char) -> bool {
        let text = ch.to_string();
        let Ok(metrics) = ctx.measure_text(&text, self.regular_font(slot)) else {
            return false;
        };
        #[cfg(target_os = "macos")]
        if slot > 0 && is_hangul_codepoint(ch) {
            return metrics.advance > 0.1;
        }
        let Some(notdef_advance) = self.notdef_advance(ctx, slot) else {
            return slot == 0 && !static_needs_fallback(ch);
        };
        metrics.advance > 0.1 && (metrics.advance - notdef_advance).abs() > 0.5
    }

    fn notdef_advance(&self, ctx: &mut dyn DrawingContext, slot: usize) -> Option<f32> {
        if let Some(advance) = self.notdef_cache.borrow().get(&slot) {
            return *advance;
        }
        let advance = ctx
            .measure_text("\u{FFFE}", self.regular_font(slot))
            .ok()
            .map(|metrics| metrics.advance)
            .filter(|advance| *advance > 0.1);
        self.notdef_cache.borrow_mut().insert(slot, advance);
        advance
    }

    fn regular_font(&self, slot: usize) -> &Font {
        if slot == 0 {
            &self.primary
        } else {
            &self.fallbacks[slot - 1].regular
        }
    }
}

fn is_variation_selector(c: char) -> bool {
    matches!(c as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

fn is_zero_width_joiner(c: char) -> bool {
    matches!(c as u32, 0x200C | 0x200D)
}

#[cfg(target_os = "macos")]
fn is_hangul_codepoint(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7AF | 0xD7B0..=0xD7FF
    )
}

/// Static Unicode ranges that are rarely present in Western monospace fonts.
/// Checked on every character; the probe set supplements this for less obvious
/// gaps specific to the user's chosen primary font.
fn static_needs_fallback(c: char) -> bool {
    let n = c as u32;
    matches!(
        n,
        0x1100..=0x11FF     // Hangul Jamo
        | 0x2100..=0x214F   // Letterlike Symbols
        | 0x2200..=0x22FF   // Mathematical Operators
        | 0x2300..=0x23FF   // Miscellaneous Technical
        | 0x2460..=0x24FF   // Enclosed Alphanumerics
        | 0x2500..=0x257F   // Box Drawing
        | 0x2580..=0x259F   // Block Elements
        | 0x25A0..=0x25FF   // Geometric Shapes
        | 0x2600..=0x26FF   // Miscellaneous Symbols
        | 0x2700..=0x27BF   // Dingbats
        | 0x27C0..=0x27EF   // Supplemental Arrows-B / math
        | 0x2800..=0x28FF   // Braille Patterns
        | 0x2E80..=0x303F   // CJK Radicals, Kangxi, Symbols & Punctuation
        | 0x3040..=0x9FFF   // Kana, Bopomofo, CJK unified block
        | 0xA000..=0xA4CF   // Yi
        | 0xA960..=0xA97F   // Hangul Jamo Extended-A
        | 0xAC00..=0xD7FF   // Hangul Syllables + Jamo Extended-B
        | 0xF900..=0xFAFF   // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F   // CJK Compatibility Forms
        | 0xFF00..=0xFFEF   // Halfwidth and Fullwidth Forms
        // ── Supplementary Multilingual Plane ───────────────────────────────
        | 0x1B000..=0x1B1FF // Kana Supplement + Kana Extended-A
        | 0x1F000..=0x1F02F // Mahjong Tiles
        | 0x1F0A0..=0x1F0FF // Playing Cards
        | 0x1F200..=0x1F2FF // Enclosed CJK Letters and Months
        | 0x1F300..=0x1FAFF // Emoji, pictographs, symbols (main block)
        | 0x1FB00..=0x1FBFF // Symbols for Legacy Computing (block sextants, etc.)
        // ── CJK Extension B–G (Supplementary Ideographic Plane) ───────────
        | 0x20000..=0x2A6DF // CJK Extension B
        | 0x2A700..=0x2CEAF // CJK Extension C / D / E
        | 0x2CEB0..=0x2EBEF // CJK Extension F
        | 0x30000..=0x3134F // CJK Extension G
    )
}

fn prefers_fallback_font(c: char) -> bool {
    let n = c as u32;
    matches!(
        n,
        0x1100..=0x11FF
            | 0x2E80..=0x303F
            | 0x3040..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x1B000..=0x1B1FF
            | 0x1F000..=0x1F02F
            | 0x1F0A0..=0x1F0FF
            | 0x1F200..=0x1F2FF
            | 0x1F300..=0x1FAFF
            | 0x1FB00..=0x1FBFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
            | 0x30000..=0x3134F
    )
}

fn solid(color: Color) -> Paint {
    Paint {
        color,
        style: PaintStyle::Fill,
        ..Default::default()
    }
}

/// Scale an RGB color towards black — used to render the DIM attribute.
fn dim_color(c: Color) -> Color {
    Color::rgb(
        (c.r as f32 * 0.55) as u8,
        (c.g as f32 * 0.55) as u8,
        (c.b as f32 * 0.55) as u8,
    )
}

/// Resolve a cell's effective (fg, bg) pair, honoring reverse video.
/// For DIM cells the foreground is further dimmed towards black.
fn resolve_pair(cell: &Cell, colors: &Colors) -> (Color, Option<Color>) {
    let (mut fg, bg) = if cell.attrs.contains(Attrs::INVERSE) {
        (
            colors.resolve(cell.bg, colors.background),
            Some(colors.resolve(cell.fg, colors.foreground)),
        )
    } else {
        let bg = match cell.bg {
            TermColor::Default => None,
            other => Some(colors.resolve(other, colors.background)),
        };
        (colors.resolve(cell.fg, colors.foreground), bg)
    };
    if cell.attrs.contains(Attrs::DIM) {
        fg = dim_color(fg);
    }
    (fg, bg)
}

/// Append a cell's display text into `text`.
/// HIDDEN cells render as a space so they occupy space but reveal nothing.
/// Continuation cells (right half of width-2 glyphs) contribute nothing.
fn push_cell_text(text: &mut String, cell: &Cell) {
    if cell.attrs.contains(Attrs::HIDDEN) {
        text.push(' ');
        return;
    }
    match &cell.kind {
        CellKind::Char(c) => text.push(*c),
        CellKind::Cluster(s) => text.push_str(s),
        CellKind::Empty => text.push(' '),
        CellKind::Continuation => {}
    }
}

fn composed_cell_text(cell: &Cell) -> String {
    let mut text = String::new();
    push_cell_text(&mut text, cell);
    compose_hangul_jamo(&text).into_owned()
}

fn cell_jamo_text(cell: &Cell) -> Option<String> {
    if cell.attrs.contains(Attrs::HIDDEN) || matches!(cell.kind, CellKind::Continuation) {
        return None;
    }
    let mut text = String::new();
    push_cell_text(&mut text, cell);
    (!text.is_empty() && text.chars().all(is_hangul_jamo)).then_some(text)
}

fn cell_uses_grid_origin(cell: &Cell) -> bool {
    cell.width != 1
        || matches!(cell.kind, CellKind::Cluster(_))
        || matches!(cell.kind, CellKind::Char(c) if !c.is_ascii())
}

fn cursor_cell_span(row: &[Cell], col: usize) -> (usize, f32) {
    if let Some(cell) = row.get(col) {
        if matches!(cell.kind, CellKind::Continuation) && col > 0 {
            let lead = &row[col - 1];
            return (col - 1, f32::from(lead.width.max(1)));
        }
        return (col, f32::from(cell.width.max(1)));
    }
    (col, 1.0)
}

fn draw_cell_text(
    ctx: &mut dyn DrawingContext,
    cell: &Cell,
    x: f32,
    baseline: f32,
    fonts: &RowFonts,
    fg: Color,
) -> AureaResult<()> {
    let text = composed_cell_text(cell);
    if text.trim().is_empty() {
        return Ok(());
    }

    let bold = cell.attrs.contains(Attrs::BOLD);
    let italic = cell.attrs.contains(Attrs::ITALIC);
    let font_slot = fonts.font_slot_for_chars(ctx, text.chars());
    ctx.draw_text_with_font(
        &text,
        Point::new(x.round(), baseline.round()),
        fonts.pick(bold, italic, font_slot),
        &solid(fg),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ctx: &mut dyn DrawingContext,
    row: &[Cell],
    y_top: f32,
    baseline: f32,
    metrics: &CellMetrics,
    fonts: &RowFonts,
    layout: &Layout,
    sel_cols: Option<(usize, usize)>,
    found: &[(usize, usize, bool)],
    colors: &Colors,
) -> AureaResult<()> {
    let cx = |col: usize| layout.cell_x(col, metrics);
    let cell_rect = |col: usize| Rect::new(cx(col), y_top, cx(col + 1) - cx(col), metrics.height);
    // Pass 1: row background + per-cell overrides.
    // Paint one rect covering the full row first so we never rely on a global
    // ctx.clear() — that "blank → draw" cycle is the primary cause of flicker.
    ctx.draw_rect(
        Rect::new(cx(0), y_top, cx(row.len()) - cx(0), metrics.height),
        &solid(colors.background),
    )?;
    for (i, cell) in row.iter().enumerate() {
        if let (_, Some(bg)) = resolve_pair(cell, colors) {
            ctx.draw_rect(cell_rect(i), &solid(bg))?;
        }
    }

    // search matches under the selection
    for &(a, b, current) in found {
        let paint = solid(if current {
            theme::FOUND_CURRENT
        } else {
            theme::FOUND
        });
        // a match ending on a wide glyph covers both its cells
        let b = match row.get(b.saturating_add(1)) {
            Some(cell) if matches!(cell.kind, CellKind::Continuation) => b + 1,
            _ => b,
        };
        for i in a..=b.min(row.len().saturating_sub(1)) {
            ctx.draw_rect(cell_rect(i), &paint)?;
        }
    }

    // Pass 2: selection highlight (drawn on top of any explicit cell BG)
    if let Some((sc, ec)) = sel_cols {
        for i in sc..=ec {
            if i >= row.len() {
                break;
            }
            ctx.draw_rect(cell_rect(i), &solid(colors.selection))?;
        }
    }

    // Pass 3: text runs — batched by (fg, bold, italic, fallback) for fewer draw calls.
    // Continuation cells (right half of width-2 glyphs) are skipped at the outer
    // level: including them in a run would mis-position every character that follows.
    let mut i = 0usize;
    while i < row.len() {
        // Skip continuation cells — they carry no content and would shift the
        // x-origin of the subsequent run one column to the left.
        if matches!(row[i].kind, CellKind::Continuation) {
            i += 1;
            continue;
        }
        let (fg, _) = resolve_pair(&row[i], colors);
        if let Some(mut raw) = cell_jamo_text(&row[i]) {
            let bold = row[i].attrs.contains(Attrs::BOLD);
            let italic = row[i].attrs.contains(Attrs::ITALIC);
            let start = i;
            i += 1;
            while i < row.len() {
                if matches!(row[i].kind, CellKind::Continuation) {
                    i += 1;
                    continue;
                }
                if resolve_pair(&row[i], colors).0 != fg
                    || row[i].attrs.contains(Attrs::BOLD) != bold
                    || row[i].attrs.contains(Attrs::ITALIC) != italic
                {
                    break;
                }
                let Some(text) = cell_jamo_text(&row[i]) else {
                    break;
                };
                raw.push_str(&text);
                i += 1;
            }

            let text = compose_hangul_jamo(&raw);
            if text == raw {
                i = start;
            } else {
                let x = cx(start);
                let font_slot = fonts.font_slot_for_chars(ctx, text.chars());
                ctx.draw_text_with_font(
                    &text,
                    Point::new(x, baseline.round()),
                    fonts.pick(bold, italic, font_slot),
                    &solid(fg),
                )?;
                continue;
            }
        }
        if cell_uses_grid_origin(&row[i]) {
            let x = cx(i);
            draw_cell_text(ctx, &row[i], x, baseline, fonts, fg)?;
            i += 1;
            continue;
        }

        let bold = row[i].attrs.contains(Attrs::BOLD);
        let italic = row[i].attrs.contains(Attrs::ITALIC);
        let font_slot = fonts.font_slot_for_cell(ctx, &row[i]);
        let start = i;
        let mut text = String::new();
        // Push the first cell (already known to match — avoids a duplicate
        // font_slot_for_cell call that would otherwise fire in the while condition
        // for cell `start` before push_cell_text advances `i`).
        push_cell_text(&mut text, &row[i]);
        i += 1;
        while i < row.len()
            && resolve_pair(&row[i], colors).0 == fg
            && row[i].attrs.contains(Attrs::BOLD) == bold
            && row[i].attrs.contains(Attrs::ITALIC) == italic
            && fonts.font_slot_for_cell(ctx, &row[i]) == font_slot
            && !cell_uses_grid_origin(&row[i])
            && !matches!(row[i].kind, CellKind::Continuation)
        {
            push_cell_text(&mut text, &row[i]);
            i += 1;
        }
        let text = compose_hangul_jamo(&text);
        if text.trim_end().is_empty() {
            continue;
        }
        // Round to the nearest pixel: Direct2D (Windows) produces crisper glyph
        // outlines when text origins land on integer device coordinates.
        let x = cx(start);
        ctx.draw_text_with_font(
            &text,
            Point::new(x, baseline.round()),
            fonts.pick(bold, italic, font_slot),
            &solid(fg),
        )?;
    }

    // Pass 4: underlines and strikethroughs
    // under the baseline, never past the bottom of the row
    let ul_y = (baseline + 2.0).min(y_top + metrics.height - 3.0).round();
    let st_y = baseline - metrics.ascent * 0.35;
    for (i, cell) in row.iter().enumerate() {
        let x = cx(i);
        let w = cx(i + 1) - x;
        if cell.attrs.contains(Attrs::UNDERLINE) {
            let ul_color = match cell.underline_color {
                TermColor::Default => resolve_pair(cell, colors).0,
                other => colors.resolve(other, colors.foreground),
            };
            draw_underline(ctx, cell.attrs, x, ul_y, w, ul_color)?;
        }
        if cell.attrs.contains(Attrs::STRIKE) {
            let (fg, _) = resolve_pair(cell, colors);
            ctx.draw_rect(Rect::new(x, st_y, w, 1.0), &solid(fg))?;
        }
    }

    Ok(())
}

/// One cell's underline in the shape SGR `4:n` asked for. A curly one is
/// a full wave per cell, so neighbours join into one line.
fn draw_underline(
    ctx: &mut dyn DrawingContext,
    attrs: Attrs,
    x: f32,
    y: f32,
    w: f32,
    color: Color,
) -> AureaResult<()> {
    let paint = solid(color);
    if attrs.contains(Attrs::CURLY_UNDERLINE) {
        let a = 1.5;
        let mut path = Path::new();
        path.commands.push(PathCommand::MoveTo(Point::new(x, y)));
        for (dx, dy) in [(0.25, -a), (0.5, 0.0), (0.75, a), (1.0, 0.0)] {
            path.commands
                .push(PathCommand::LineTo(Point::new(x + w * dx, y + dy)));
        }
        let stroke = Paint {
            color,
            style: PaintStyle::Stroke,
            stroke_width: 1.2,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        ctx.draw_path(&path, &stroke)
    } else if attrs.contains(Attrs::DOUBLE_UNDERLINE) {
        ctx.draw_rect(Rect::new(x, y - 1.0, w, 1.0), &paint)?;
        ctx.draw_rect(Rect::new(x, y + 1.0, w, 1.0), &paint)
    } else if attrs.contains(Attrs::DOTTED_UNDERLINE) {
        let mut dot = x;
        while dot < x + w {
            ctx.draw_rect(Rect::new(dot, y, 1.0, 1.0), &paint)?;
            dot += 2.0;
        }
        Ok(())
    } else if attrs.contains(Attrs::DASHED_UNDERLINE) {
        ctx.draw_rect(Rect::new(x, y, w * 0.4, 1.0), &paint)?;
        ctx.draw_rect(Rect::new(x + w * 0.5, y, w * 0.4, 1.0), &paint)
    } else {
        ctx.draw_rect(Rect::new(x, y, w, 1.0), &paint)
    }
}

/// Draw the full visible grid, then the cursor on top if `cursor_visible`.
/// `layout` places the cells in the window.
/// `selection` highlights the covered cells with the selection background.
#[allow(clippy::too_many_arguments)]
pub fn draw_grid(
    ctx: &mut dyn DrawingContext,
    rows: &[Vec<Cell>],
    cursor: Option<(usize, usize)>,
    metrics: &CellMetrics,
    fonts: &RowFonts,
    cursor_shape: &CursorShape,
    layout: &Layout,
    selection: Option<SelectionRange>,
    found: &[Piece],
    colors: &Colors,
) -> AureaResult<()> {
    let line_h = metrics.height;
    let mut row_found = Vec::new();

    for (row_idx, row) in rows.iter().enumerate() {
        let y_top = layout.row_y(row_idx, metrics);
        let sel_cols = selection.and_then(|s| s.cols_for_row(row_idx, row.len()));
        row_found.clear();
        row_found.extend(
            found
                .iter()
                .filter(|p| p.0 == row_idx)
                .map(|&(_, a, b, current)| (a, b, current)),
        );
        draw_row(
            ctx,
            row,
            y_top,
            y_top + metrics.baseline_offset,
            metrics,
            fonts,
            layout,
            sel_cols,
            &row_found,
            colors,
        )?;
    }

    if let Some((row, col)) = cursor {
        {
            if let Some(cursor_row) = rows.get(row) {
                let (cursor_col, cursor_width_cells) = cursor_cell_span(cursor_row, col);
                let x = layout.cell_x(cursor_col, metrics);
                let y = layout.row_y(row, metrics);
                let cursor_width =
                    layout.cell_x(cursor_col + cursor_width_cells as usize, metrics) - x;
                match cursor_shape {
                    CursorShape::Block => {
                        ctx.draw_rect(
                            Rect::new(x, y, cursor_width, line_h),
                            &solid(colors.cursor),
                        )?;
                        if let Some(cell) = cursor_row.get(cursor_col) {
                            let text = composed_cell_text(cell);
                            if !text.trim().is_empty() {
                                let bold = cell.attrs.contains(Attrs::BOLD);
                                let italic = cell.attrs.contains(Attrs::ITALIC);
                                let font_slot = fonts.font_slot_for_chars(ctx, text.chars());
                                let char_color = colors.resolve(cell.bg, colors.background);
                                ctx.draw_text_with_font(
                                    &text,
                                    Point::new(x.round(), (y + metrics.baseline_offset).round()),
                                    fonts.pick(bold, italic, font_slot),
                                    &solid(char_color),
                                )?;
                            }
                        }
                    }
                    CursorShape::Beam => {
                        ctx.draw_rect(Rect::new(x, y, 2.0, line_h), &solid(colors.cursor))?;
                    }
                    CursorShape::Hollow => {
                        let paint = solid(colors.cursor);
                        ctx.draw_rect(Rect::new(x, y, cursor_width, 1.0), &paint)?;
                        ctx.draw_rect(Rect::new(x, y + line_h - 1.0, cursor_width, 1.0), &paint)?;
                        ctx.draw_rect(Rect::new(x, y, 1.0, line_h), &paint)?;
                        ctx.draw_rect(Rect::new(x + cursor_width - 1.0, y, 1.0, line_h), &paint)?;
                    }
                    CursorShape::Underline => {
                        ctx.draw_rect(
                            Rect::new(x, y + line_h - 2.0, cursor_width, 2.0),
                            &solid(colors.cursor),
                        )?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// One tab in the bar.
pub struct TabLabel {
    pub text: String,
    pub active: bool,
    /// It rang while hidden.
    pub bell: bool,
    /// Where it is drawn now; it slides there when tabs move.
    pub x: f32,
    pub hover: bool,
    pub close_hover: bool,
    pub dragged: bool,
}

/// The tab bar across the top: tabs, their close buttons, and the new tab
/// button after them. A dragged tab draws last, lifted.
#[allow(clippy::too_many_arguments)]
pub fn draw_tab_bar(
    ctx: &mut dyn DrawingContext,
    bar: &Bar,
    tabs: &[TabLabel],
    new_hover: bool,
    width: f32,
    metrics: &CellMetrics,
    fonts: &RowFonts,
    colors: &Colors,
) -> AureaResult<()> {
    ctx.draw_rect(Rect::new(0.0, 0.0, width, bar.height), &solid(theme::BAR))?;
    let order = tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.dragged)
        .chain(tabs.iter().enumerate().filter(|(_, t)| t.dragged));
    for (i, tab) in order {
        draw_tab(ctx, bar, tab, i, tabs, metrics, fonts, colors)?;
    }
    // the new tab button: a plus
    let (x, y, w, h) = bar.new_button();
    let fg = if new_hover {
        ctx.draw_round_rect(
            Rect::new(x + 4.0, y + 4.0, w - 8.0, h - 8.0),
            4.0,
            &solid(theme::BAR_HOVER),
        )?;
        colors.foreground
    } else {
        theme::BAR_TEXT_DIM
    };
    let (cx, cy, arm) = (
        (x + w / 2.0).round(),
        (y + h / 2.0).round(),
        (h * 0.2).round(),
    );
    ctx.draw_rect(Rect::new(cx - arm, cy - 0.5, 2.0 * arm, 1.5), &solid(fg))?;
    ctx.draw_rect(Rect::new(cx - 0.5, cy - arm, 1.5, 2.0 * arm), &solid(fg))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_tab(
    ctx: &mut dyn DrawingContext,
    bar: &Bar,
    tab: &TabLabel,
    i: usize,
    tabs: &[TabLabel],
    metrics: &CellMetrics,
    fonts: &RowFonts,
    colors: &Colors,
) -> AureaResult<()> {
    let (x, w, h) = (tab.x.round(), bar.tab_w, bar.height);
    let pad = 10.0;
    if tab.dragged {
        ctx.draw_shadow(Rect::new(x, 0.0, w, h), 0.0, 8.0, Color::rgba(0, 0, 0, 110))?;
    }
    if tab.active || tab.dragged {
        ctx.draw_rect(Rect::new(x, 0.0, w, h), &solid(colors.background))?;
        ctx.draw_rect(Rect::new(x, 0.0, w, 2.0), &solid(theme::FOUND_CURRENT))?;
    } else if tab.hover {
        ctx.draw_rect(Rect::new(x, 0.0, w, h), &solid(theme::BAR_HOVER))?;
    } else if i > 0 && !tabs[i - 1].active && !tabs[i - 1].hover {
        ctx.draw_rect(Rect::new(x, h * 0.25, 1.0, h * 0.5), &solid(theme::DIVIDER))?;
    }
    let close = bar
        .close_button(x)
        .filter(|_| tab.active || tab.hover || tab.dragged);
    let mut text = if tab.bell {
        format!("• {}", tab.text)
    } else {
        tab.text.clone()
    };
    let slot = fonts.font_slot_for_chars(ctx, text.chars());
    let font = fonts.pick(false, false, slot);
    // cut long titles to the tab, with an ellipsis: grown from the start
    // while it fits
    let room = w - 2.0 * pad - close.map_or(0.0, |c| c.2 + 4.0);
    if ctx
        .measure_text(&text, font)
        .is_ok_and(|m| m.advance > room)
    {
        let mut cut = String::from("…");
        for c in text.chars() {
            let mut next = cut.clone();
            next.insert(next.len() - '…'.len_utf8(), c);
            if ctx
                .measure_text(&next, font)
                .is_ok_and(|m| m.advance > room)
            {
                break;
            }
            cut = next;
        }
        text = cut;
    }
    let fg = if tab.active || tab.dragged {
        colors.foreground
    } else {
        theme::BAR_TEXT_DIM
    };
    let baseline = ((h - metrics.height) / 2.0 + metrics.baseline_offset).round();
    ctx.draw_text_with_font(&text, Point::new(x + pad, baseline), font, &solid(fg))?;
    if let Some((cx, cy, cw, ch)) = close {
        if tab.close_hover {
            ctx.draw_round_rect(
                Rect::new(cx, cy, cw, ch),
                4.0,
                &solid(theme::BAR_HOVER_STRONG),
            )?;
        }
        // an x from two short strokes
        let (mx, my, arm) = (cx + cw / 2.0, cy + ch / 2.0, (cw * 0.22).round());
        let stroke = Paint {
            color: if tab.close_hover {
                colors.foreground
            } else {
                theme::BAR_TEXT_DIM
            },
            style: PaintStyle::Stroke,
            stroke_width: 1.4,
            ..Default::default()
        };
        ctx.draw_line(mx - arm, my - arm, mx + arm, my + arm, &stroke)?;
        ctx.draw_line(mx - arm, my + arm, mx + arm, my - arm, &stroke)?;
    }
    Ok(())
}

/// The find bar in the top right corner: the query with a caret, and
/// where the current match is among all of them.
pub fn draw_find_bar(
    ctx: &mut dyn DrawingContext,
    query: &str,
    (current, total): (usize, usize),
    (right, top): (f32, f32),
    metrics: &CellMetrics,
    fonts: &RowFonts,
    colors: &Colors,
) -> AureaResult<()> {
    let pad = 8.0;
    let h = metrics.height + 2.0 * pad;
    let w = (metrics.width * 36.0).min(right - 16.0).max(120.0);
    let x = (right - w - 8.0).round();
    let y = top + 8.0;
    ctx.draw_shadow(Rect::new(x, y, w, h), 6.0, 10.0, Color::rgba(0, 0, 0, 90))?;
    ctx.draw_round_rect(Rect::new(x, y, w, h), 6.0, &solid(theme::BAR))?;
    let baseline = (y + pad + metrics.baseline_offset).round();
    let count = if total == 0 && !query.is_empty() {
        "no match".to_owned()
    } else {
        format!("{current}/{total}")
    };
    let count_w = ctx
        .measure_text(&count, &fonts.primary)
        .map_or(0.0, |m| m.advance);
    ctx.draw_text_with_font(
        &count,
        Point::new((x + w - pad - count_w).round(), baseline),
        &fonts.primary,
        &solid(theme::BAR_TEXT_DIM),
    )?;
    let slot = fonts.font_slot_for_chars(ctx, query.chars());
    let font = fonts.pick(false, false, slot);
    let (shown, hint) = if query.is_empty() {
        ("find", true)
    } else {
        (query, false)
    };
    // the end of a long query, where typing happens
    // grown from the end while it fits: the cost stays with what shows
    let room = w - 3.0 * pad - count_w;
    let mut start = shown.len();
    for (i, _) in shown.char_indices().rev() {
        if ctx
            .measure_text(&shown[i..], font)
            .is_ok_and(|m| m.advance > room)
        {
            break;
        }
        start = i;
    }
    let shown = &shown[start..];
    let text_x = (x + pad).round();
    ctx.draw_text_with_font(
        shown,
        Point::new(text_x, baseline),
        font,
        &solid(if hint {
            theme::BAR_TEXT_DIM
        } else {
            colors.foreground
        }),
    )?;
    let caret_x = if hint {
        text_x
    } else {
        text_x
            + ctx
                .measure_text(shown, font)
                .map_or(0.0, |m| m.advance)
                .round()
    };
    ctx.draw_rect(
        Rect::new(caret_x, y + pad, 1.5, metrics.height),
        &solid(colors.cursor),
    )
}

/// Text an input method is still composing, over the cells at `at`:
/// on the background, underlined, so it reads as not typed yet.
pub fn draw_preedit(
    ctx: &mut dyn DrawingContext,
    text: &str,
    at: Point,
    metrics: &CellMetrics,
    fonts: &RowFonts,
    colors: &Colors,
) -> AureaResult<()> {
    let slot = fonts.font_slot_for_chars(ctx, text.chars());
    let font = fonts.pick(false, false, slot);
    let width = ctx
        .measure_text(text, font)
        .map_or(metrics.width, |m| m.advance)
        .max(metrics.width);
    ctx.draw_rect(
        Rect::new(at.x, at.y, width, metrics.height),
        &solid(colors.background),
    )?;
    ctx.draw_text_with_font(
        text,
        Point::new(at.x.round(), (at.y + metrics.baseline_offset).round()),
        font,
        &solid(colors.foreground),
    )?;
    ctx.draw_rect(
        Rect::new(at.x, at.y + metrics.height - 2.0, width, 1.0),
        &solid(colors.foreground),
    )
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
