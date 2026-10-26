//! The "glacia-dark" palette, and the colours a frame draws with once apps
//! have changed some of it.

use std::array::from_fn;

use aurea::render::Color;
use vanta::Color as TermColor;
use vanta::Palette;

pub const BACKGROUND: Color = Color::rgb(16, 18, 24);
pub const FOREGROUND: Color = Color::rgb(216, 222, 233);
pub const CURSOR: Color = Color::rgb(255, 255, 255);
pub const SELECTION: Color = Color::rgb(58, 79, 138);
/// search matches, and the current one.
pub const FOUND: Color = Color::rgb(92, 76, 28);
pub const FOUND_CURRENT: Color = Color::rgb(176, 112, 24);
/// the find bar.
pub const BAR: Color = Color::rgb(34, 38, 48);
pub const BAR_TEXT_DIM: Color = Color::rgb(130, 138, 152);
/// the line between split panes.
pub const DIVIDER: Color = Color::rgb(52, 58, 72);

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

/// The colours a frame draws with: the theme's, with whatever an app
/// changed through OSC 4 / 10 / 11 / 12.
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub selection: Color,
    ansi: [Color; 256],
}

impl Colors {
    pub fn theme() -> Self {
        Self {
            background: BACKGROUND,
            foreground: FOREGROUND,
            cursor: CURSOR,
            selection: SELECTION,
            ansi: from_fn(|i| xterm256(i as u8)),
        }
    }

    /// The theme as vanta keeps it, so apps that ask get these colours.
    pub fn palette(&self) -> Palette {
        let rgb = |c: Color| (c.r, c.g, c.b);
        Palette {
            fg: rgb(self.foreground),
            bg: rgb(self.background),
            cursor: rgb(self.cursor),
            ansi: self.ansi.map(rgb),
        }
    }

    /// Colours after an app changed some.
    pub fn from_palette(palette: &Palette) -> Self {
        let color = |(r, g, b): (u8, u8, u8)| Color::rgb(r, g, b);
        Self {
            background: color(palette.bg),
            foreground: color(palette.fg),
            cursor: color(palette.cursor),
            selection: SELECTION,
            ansi: palette.ansi.map(color),
        }
    }

    /// A cell colour as pixels; `default` is the slot's own (foreground or
    /// background).
    pub fn resolve(&self, color: TermColor, default: Color) -> Color {
        match color {
            TermColor::Default => default,
            TermColor::Indexed(idx) => self.ansi[idx as usize],
            TermColor::Rgb(r, g, b) => Color::rgb(r, g, b),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_goes_to_vanta_and_back() {
        let colors = Colors::theme();
        assert_eq!(Colors::from_palette(&colors.palette()), colors);
        let palette = colors.palette();
        assert_eq!(palette.bg, (16, 18, 24));
        assert_eq!(palette.ansi[1], (255, 107, 107));
        assert_eq!(
            colors.resolve(TermColor::Indexed(196), FOREGROUND),
            Color::rgb(255, 0, 0)
        );
    }
}
