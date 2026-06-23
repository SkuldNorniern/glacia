//! Hardcoded "glacia-dark" palette. Becomes config/theme-file driven once the
//! config loading task lands; until then these are the only colors Glacia
//! knows about.

use aurea::render::Color;
use vanta::Color as TermColor;

pub const BACKGROUND: Color = Color::rgb(16, 18, 24);
pub const FOREGROUND: Color = Color::rgb(216, 222, 233);
pub const CURSOR: Color = Color::rgb(255, 255, 255);
pub const SELECTION: Color = Color::rgb(58, 79, 138);

const ANSI16: [Color; 16] = [
    Color::rgb(27, 29, 36),    // black
    Color::rgb(255, 107, 107), // red
    Color::rgb(123, 216, 143), // green
    Color::rgb(247, 207, 109), // yellow
    Color::rgb(106, 169, 255), // blue
    Color::rgb(199, 146, 234), // magenta
    Color::rgb(93, 228, 199),  // cyan
    Color::rgb(216, 222, 233), // white
    Color::rgb(92, 99, 112),   // bright black
    Color::rgb(255, 135, 135), // bright red
    Color::rgb(166, 227, 161), // bright green
    Color::rgb(255, 224, 130), // bright yellow
    Color::rgb(142, 197, 255), // bright blue
    Color::rgb(214, 162, 255), // bright magenta
    Color::rgb(137, 245, 221), // bright cyan
    Color::rgb(255, 255, 255), // bright white
];

fn xterm256(idx: u8) -> Color {
    match idx {
        0..=15 => ANSI16[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb(scale(i / 36), scale((i % 36) / 6), scale(i % 6))
        }
        _ => {
            let v = 8 + (idx - 232) * 10;
            Color::rgb(v, v, v)
        }
    }
}

/// Resolve a Vanta cell color to a renderer color, given the default to use
/// for `Color::Default` (the terminal's foreground or background pen).
pub fn resolve(color: TermColor, default: Color) -> Color {
    match color {
        TermColor::Default => default,
        TermColor::Indexed(idx) => xterm256(idx),
        TermColor::Rgb(r, g, b) => Color::rgb(r, g, b),
    }
}
