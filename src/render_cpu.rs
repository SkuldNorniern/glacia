//! CPU drawing of the terminal grid. Concrete implementation, no renderer
//! trait — see `PLAN.md`'s "UI Layering" note on when that's introduced.

use std::cell::RefCell;
use std::collections::HashMap;

use aurea::AureaResult;
use aurea::render::{
    Color, DrawingContext, Font, FontStyle, FontWeight, Paint, PaintStyle, Point, Rect,
};
use vanta::Color as TermColor;
use vanta::vt::Attrs;
use vanta::{Cell, CellKind};

use crate::config::CursorShape;
use crate::theme;
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
fn resolve_pair(cell: &Cell) -> (Color, Option<Color>) {
    let (mut fg, bg) = if cell.attrs.contains(Attrs::INVERSE) {
        (
            theme::resolve(cell.bg, theme::BACKGROUND),
            Some(theme::resolve(cell.fg, theme::FOREGROUND)),
        )
    } else {
        let bg = match cell.bg {
            TermColor::Default => None,
            other => Some(theme::resolve(other, theme::BACKGROUND)),
        };
        (theme::resolve(cell.fg, theme::FOREGROUND), bg)
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
    compose_hangul_jamo(&text)
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
    x_offset: f32,
    sel_cols: Option<(usize, usize)>,
) -> AureaResult<()> {
    // Pass 1: row background + per-cell overrides.
    // Paint one rect covering the full row first so we never rely on a global
    // ctx.clear() — that "blank → draw" cycle is the primary cause of flicker.
    ctx.draw_rect(
        Rect::new(
            x_offset,
            y_top,
            row.len() as f32 * metrics.width,
            metrics.height,
        ),
        &solid(theme::BACKGROUND),
    )?;
    for (i, cell) in row.iter().enumerate() {
        if let (_, Some(bg)) = resolve_pair(cell) {
            let x = i as f32 * metrics.width + x_offset;
            ctx.draw_rect(
                Rect::new(x, y_top, metrics.width, metrics.height),
                &solid(bg),
            )?;
        }
    }

    // Pass 2: selection highlight (drawn on top of any explicit cell BG)
    if let Some((sc, ec)) = sel_cols {
        for i in sc..=ec {
            if i >= row.len() {
                break;
            }
            let x = i as f32 * metrics.width + x_offset;
            ctx.draw_rect(
                Rect::new(x, y_top, metrics.width, metrics.height),
                &solid(theme::SELECTION),
            )?;
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
        let (fg, _) = resolve_pair(&row[i]);
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
                if resolve_pair(&row[i]).0 != fg
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
                let x = (start as f32 * metrics.width + x_offset).round();
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
            let x = (i as f32 * metrics.width + x_offset).round();
            draw_cell_text(ctx, &row[i], x, baseline, fonts, fg)?;
            i += 1;
            continue;
        }

        let bold = row[i].attrs.contains(Attrs::BOLD);
        let italic = row[i].attrs.contains(Attrs::ITALIC);
        let font_slot = fonts.font_slot_for_cell(ctx, &row[i]);
        let start = i;
        let mut text = String::new();
        while i < row.len()
            && resolve_pair(&row[i]).0 == fg
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
        let x = (start as f32 * metrics.width + x_offset).round();
        ctx.draw_text_with_font(
            &text,
            Point::new(x, baseline.round()),
            fonts.pick(bold, italic, font_slot),
            &solid(fg),
        )?;
    }

    // Pass 4: underlines and strikethroughs
    let ul_y = baseline + 2.0;
    let st_y = baseline - metrics.ascent * 0.35;
    for (i, cell) in row.iter().enumerate() {
        let x = i as f32 * metrics.width + x_offset;
        if cell.attrs.contains(Attrs::UNDERLINE) {
            let ul_color = match cell.underline_color {
                TermColor::Default => resolve_pair(cell).0,
                other => theme::resolve(other, theme::FOREGROUND),
            };
            ctx.draw_rect(Rect::new(x, ul_y, metrics.width, 1.5), &solid(ul_color))?;
        }
        if cell.attrs.contains(Attrs::STRIKE) {
            let (fg, _) = resolve_pair(cell);
            ctx.draw_rect(Rect::new(x, st_y, metrics.width, 1.0), &solid(fg))?;
        }
    }

    Ok(())
}

/// Draw the full visible grid, then the cursor on top if `cursor_visible`.
/// `padding` offsets every cell from the window edge.
/// `selection` highlights the covered cells with the selection background.
#[allow(clippy::too_many_arguments)]
pub fn draw_grid(
    ctx: &mut dyn DrawingContext,
    rows: &[Vec<Cell>],
    cursor: (usize, usize),
    cursor_visible: bool,
    metrics: &CellMetrics,
    fonts: &RowFonts,
    cursor_shape: &CursorShape,
    padding: f32,
    selection: Option<SelectionRange>,
) -> AureaResult<()> {
    let line_h = metrics.height;

    for (row_idx, row) in rows.iter().enumerate() {
        let y_top = row_idx as f32 * line_h + padding;
        let sel_cols = selection.and_then(|s| s.cols_for_row(row_idx, row.len()));
        draw_row(
            ctx,
            row,
            y_top,
            y_top + metrics.baseline_offset,
            metrics,
            fonts,
            padding,
            sel_cols,
        )?;
    }

    if cursor_visible {
        let (row, col) = cursor;
        if row < rows.len() {
            let (cursor_col, cursor_width_cells) = cursor_cell_span(&rows[row], col);
            let x = cursor_col as f32 * metrics.width + padding;
            let y = row as f32 * line_h + padding;
            let cursor_width = metrics.width * cursor_width_cells;
            match cursor_shape {
                CursorShape::Block => {
                    ctx.draw_rect(Rect::new(x, y, cursor_width, line_h), &solid(theme::CURSOR))?;
                    if cursor_col < rows[row].len() {
                        let cell = &rows[row][cursor_col];
                        let text = composed_cell_text(cell);
                        if !text.trim().is_empty() {
                            let bold = cell.attrs.contains(Attrs::BOLD);
                            let italic = cell.attrs.contains(Attrs::ITALIC);
                            let font_slot = fonts.font_slot_for_chars(ctx, text.chars());
                            let char_color = theme::resolve(cell.bg, theme::BACKGROUND);
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
                    ctx.draw_rect(Rect::new(x, y, 2.0, line_h), &solid(theme::CURSOR))?;
                }
                CursorShape::Underline => {
                    ctx.draw_rect(
                        Rect::new(x, y + line_h - 2.0, cursor_width, 2.0),
                        &solid(theme::CURSOR),
                    )?;
                }
            }
        }
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
