//! CPU drawing of the terminal grid. Concrete implementation, no renderer
//! trait — see `PLAN.md`'s "UI Layering" note on when that's introduced.

use std::collections::HashSet;

use aurea::AureaResult;
use aurea::render::{Color, DrawingContext, Font, FontWeight, Paint, PaintStyle, Point, Rect};
use vanta::Color as TermColor;
use vanta::vt::Attrs;
use vanta::{Cell, CellKind};

use crate::theme;

pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of a cell row to the text baseline, derived from
    /// the actual font ascent rather than a fixed fraction of `height`.
    pub baseline_offset: f32,
}

/// Font set for one rendered frame: primary, bold, and their fallback
/// counterparts for CJK/emoji runs.
///
/// Built once from the loaded `Font` objects and reused every frame — avoids
/// cloning font family strings on the hot path. Call [`RowFonts::probe`] inside
/// a canvas draw closure after construction to auto-detect characters the
/// primary font cannot render.
pub struct RowFonts {
    pub primary: Font,
    bold: Font,
    pub fallback: Option<Font>,
    bold_fallback: Option<Font>,
    /// Codepoints detected at startup as missing from the primary font.
    /// Populated by [`RowFonts::probe`]; only used when a fallback is configured.
    probed_fallback: HashSet<u32>,
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
            probed_fallback: HashSet::new(),
        }
    }

    /// Measure a sample of characters with the primary font and record those
    /// whose advance matches the `.notdef` sentinel (U+FFFE), indicating the
    /// primary font has no glyph for them. Only meaningful when a fallback font
    /// is configured; safe to call with no fallback (becomes a no-op).
    pub fn probe(&mut self, ctx: &mut dyn DrawingContext, cell_width: f32) {
        if self.fallback.is_none() {
            return;
        }

        // U+FFFE is a guaranteed non-character — every font maps it to .notdef.
        // Its advance becomes our reference for "font has no glyph here".
        let notdef_advance = ctx
            .measure_text("\u{FFFE}", &self.primary)
            .map(|m| m.advance)
            .unwrap_or(0.0);

        // Only meaningful if .notdef has a distinct advance we can compare against.
        // If notdef_advance == 0 the font face reports nothing — skip probing.
        if notdef_advance < 0.1 {
            return;
        }

        let probes: &[char] = &[
            // Braille Patterns (U+2800-U+28FF) — common in terminal UIs
            '⠀', '⠋', '⠿', // Miscellaneous Symbols (U+2600-U+26FF)
            '☀', '☁', '★', '☆', '☑', '☒', '♥', '♦', // Dingbats (U+2700-U+27BF)
            '✓', '✗', '✦', '✧', '➔', '➜',
            // Supplemental Arrows-B / Misc Math (U+27C0-U+27EF)
            '⟹', '⟺', // Miscellaneous Technical (U+2300-U+23FF)
            '⌨', '⌚', '⌛', '⏎', '⏳', // Mathematical Operators (U+2200-U+22FF)
            '∀', '∂', '∑', '∞', '∇', '∈', '∉', '≈', '≠', '≤', '≥',
            // Geometric Shapes (U+25A0-U+25FF)
            '◆', '◇', '◈', '▲', '▼', '◀', '▶',
            // Number Forms / Letterlike (U+2100-U+214F)
            '™', '©', '®', '℃', '℉',
        ];

        // Tolerance: glyphs whose advance is within 1px of notdef are considered missing.
        let tol = 1.0_f32;
        // Also treat glyphs much narrower than the cell width as substituted .notdef.
        let narrow_threshold = cell_width * 0.4;

        for &ch in probes {
            let Ok(m) = ctx.measure_text(&ch.to_string(), &self.primary) else {
                continue;
            };
            if (m.advance - notdef_advance).abs() <= tol || m.advance < narrow_threshold {
                self.probed_fallback.insert(ch as u32);
            }
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

    /// Whether a character should be rendered with the fallback font rather
    /// than the primary. Combines a static Unicode-range check for common
    /// blocks Western monospace fonts lack (CJK, Hangul, emoji, braille, …)
    /// with the startup probe results from [`RowFonts::probe`].
    pub fn char_needs_fallback(&self, c: char) -> bool {
        static_needs_fallback(c) || self.probed_fallback.contains(&(c as u32))
    }
}

/// Static Unicode ranges that are rarely present in Western monospace fonts.
/// Checked on every character; the probe set supplements this for less obvious
/// gaps specific to the user's chosen primary font.
fn static_needs_fallback(c: char) -> bool {
    let n = c as u32;
    matches!(
        n,
        0x1100..=0x11FF   // Hangul Jamo
        | 0x2800..=0x28FF // Braille Patterns
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

/// Whether a cell's content should use the fallback font, consulting both the
/// static Unicode-range table and the startup probe results.
fn cell_needs_fallback(cell: &Cell, fonts: &RowFonts) -> bool {
    match &cell.kind {
        CellKind::Char(c) => fonts.char_needs_fallback(*c),
        CellKind::Cluster(s) => s.chars().any(|c| fonts.char_needs_fallback(c)),
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
        let use_fallback = cell_needs_fallback(&row[i], fonts);
        let start = i;
        let mut text = String::new();
        while i < row.len()
            && resolve_pair(&row[i]).0 == fg
            && row[i].attrs.contains(Attrs::BOLD) == bold
            && cell_needs_fallback(&row[i], fonts) == use_fallback
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

    for (row_idx, row) in rows.iter().enumerate() {
        let y_top = row_idx as f32 * line_h;
        draw_row(
            ctx,
            row,
            y_top,
            y_top + metrics.baseline_offset,
            metrics,
            fonts,
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
