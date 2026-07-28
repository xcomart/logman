//! Color palettes for the terminal grid.
//!
//! `alacritty_terminal` deliberately leaves the actual color values to the
//! embedding application: [`alacritty_terminal::term::color::Colors`] starts out
//! completely empty and is only filled in by escape sequences such as `OSC 4`.
//! This module supplies the concrete palette that turns the abstract
//! [`Color`](alacritty_terminal::vte::ansi::Color) values stored in every grid
//! cell into renderable RGB triples.

use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::snapshot::RunFlags;

/// A 24-bit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Create a color from its individual channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Create a color from a packed `0xRRGGBB` value.
    pub const fn from_u32(value: u32) -> Self {
        Self {
            r: ((value >> 16) & 0xff) as u8,
            g: ((value >> 8) & 0xff) as u8,
            b: (value & 0xff) as u8,
        }
    }

    /// Pack the color into a `0xRRGGBB` value.
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Darken the color, used to render the `SGR 2` (dim/faint) attribute.
    const fn dimmed(self) -> Self {
        Self {
            r: (self.r as u16 * 2 / 3) as u8,
            g: (self.g as u16 * 2 / 3) as u8,
            b: (self.b as u16 * 2 / 3) as u8,
        }
    }
}

/// Colors used to render a terminal surface.
#[derive(Debug, Clone)]
pub struct TerminalTheme {
    /// Default text color.
    pub foreground: Rgb,
    /// Default background color.
    pub background: Rgb,
    /// Color of the text cursor.
    pub cursor: Rgb,
    /// Background color of selected text.
    pub selection: Rgb,
    /// The 16 ANSI colors: indices `0..8` are the normal colors, `8..16` the
    /// bright variants.
    pub ansi: [Rgb; 16],
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self::dark()
    }
}

impl TerminalTheme {
    /// A dark palette in the spirit of Zed's / Atom's "One Dark".
    pub fn dark() -> Self {
        Self {
            foreground: Rgb::from_u32(0xabb2bf),
            background: Rgb::from_u32(0x282c34),
            cursor: Rgb::from_u32(0x528bff),
            selection: Rgb::from_u32(0x3e4451),
            ansi: [
                // Normal.
                Rgb::from_u32(0x1e2127), // black
                Rgb::from_u32(0xe06c75), // red
                Rgb::from_u32(0x98c379), // green
                Rgb::from_u32(0xd19a66), // yellow
                Rgb::from_u32(0x61afef), // blue
                Rgb::from_u32(0xc678dd), // magenta
                Rgb::from_u32(0x56b6c2), // cyan
                Rgb::from_u32(0xabb2bf), // white
                // Bright.
                Rgb::from_u32(0x5c6370), // bright black
                Rgb::from_u32(0xff7b86), // bright red
                Rgb::from_u32(0xb5e890), // bright green
                Rgb::from_u32(0xe5c07b), // bright yellow
                Rgb::from_u32(0x7cc3ff), // bright blue
                Rgb::from_u32(0xd7a3ea), // bright magenta
                Rgb::from_u32(0x70d4e0), // bright cyan
                Rgb::from_u32(0xffffff), // bright white
            ],
        }
    }

    /// A light palette in the spirit of Zed's / Atom's "One Light".
    pub fn light() -> Self {
        Self {
            foreground: Rgb::from_u32(0x383a42),
            background: Rgb::from_u32(0xfafafa),
            cursor: Rgb::from_u32(0x526fff),
            selection: Rgb::from_u32(0xd4d7dd),
            ansi: [
                // Normal.
                Rgb::from_u32(0x383a42), // black
                Rgb::from_u32(0xe45649), // red
                Rgb::from_u32(0x50a14f), // green
                Rgb::from_u32(0xc18401), // yellow
                Rgb::from_u32(0x4078f2), // blue
                Rgb::from_u32(0xa626a4), // magenta
                Rgb::from_u32(0x0184bc), // cyan
                Rgb::from_u32(0xa0a1a7), // white
                // Bright.
                Rgb::from_u32(0x4f525e), // bright black
                Rgb::from_u32(0xff5a4e), // bright red
                Rgb::from_u32(0x5cbf5b), // bright green
                Rgb::from_u32(0xe39d02), // bright yellow
                Rgb::from_u32(0x5c8cff), // bright blue
                Rgb::from_u32(0xc435c1), // bright magenta
                Rgb::from_u32(0x019fdf), // bright cyan
                Rgb::from_u32(0xffffff), // bright white
            ],
        }
    }

    /// Turn a grid cell color into a concrete [`Rgb`] value.
    ///
    /// `is_foreground` tells the palette whether the color is used as text or
    /// as background color; only foreground colors are brightened by
    /// [`RunFlags::BOLD`] or darkened by [`RunFlags::DIM`], which is the
    /// behaviour users expect from `xterm`-like terminals.
    ///
    /// Indexed colors follow the usual xterm-256 layout: `0..=15` map onto
    /// [`TerminalTheme::ansi`], `16..=231` onto the 6x6x6 color cube and
    /// `232..=255` onto the 24 step grayscale ramp.
    pub fn resolve(&self, color: Color, is_foreground: bool, flags: RunFlags) -> Rgb {
        match color {
            Color::Spec(rgb) => Rgb::new(rgb.r, rgb.g, rgb.b),
            Color::Indexed(index) => self.resolve_indexed(index, is_foreground, flags),
            Color::Named(named) => self.resolve_named(named, is_foreground, flags),
        }
    }

    fn resolve_indexed(&self, index: u8, is_foreground: bool, flags: RunFlags) -> Rgb {
        match index {
            // Normal ANSI colors; bold text is promoted to the bright variant.
            0..=7 => {
                let base = index as usize;
                if is_foreground && flags.contains(RunFlags::BOLD) {
                    self.ansi[base + 8]
                } else if is_foreground && flags.contains(RunFlags::DIM) {
                    self.ansi[base].dimmed()
                } else {
                    self.ansi[base]
                }
            }
            // Bright ANSI colors.
            8..=15 => self.ansi[index as usize],
            // 6x6x6 color cube.
            16..=231 => {
                let index = index - 16;
                let level = |value: u8| if value == 0 { 0 } else { value * 40 + 55 };
                Rgb::new(level(index / 36), level((index / 6) % 6), level(index % 6))
            }
            // Grayscale ramp.
            232..=255 => {
                let level = (index - 232) * 10 + 8;
                Rgb::new(level, level, level)
            }
        }
    }

    fn resolve_named(&self, named: NamedColor, is_foreground: bool, flags: RunFlags) -> Rgb {
        match named {
            NamedColor::Foreground => {
                if is_foreground && flags.contains(RunFlags::DIM) && !flags.contains(RunFlags::BOLD)
                {
                    self.foreground.dimmed()
                } else {
                    self.foreground
                }
            }
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::BrightForeground => self.foreground,
            NamedColor::DimForeground => self.foreground.dimmed(),
            NamedColor::DimBlack => self.ansi[0].dimmed(),
            NamedColor::DimRed => self.ansi[1].dimmed(),
            NamedColor::DimGreen => self.ansi[2].dimmed(),
            NamedColor::DimYellow => self.ansi[3].dimmed(),
            NamedColor::DimBlue => self.ansi[4].dimmed(),
            NamedColor::DimMagenta => self.ansi[5].dimmed(),
            NamedColor::DimCyan => self.ansi[6].dimmed(),
            NamedColor::DimWhite => self.ansi[7].dimmed(),
            // Everything left over is one of the 16 ANSI colors.
            other => self.resolve_indexed(other as u8, is_foreground, flags),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alacritty_terminal::vte::ansi::Rgb as VteRgb;

    #[test]
    fn packed_roundtrip() {
        let color = Rgb::from_u32(0x123456);
        assert_eq!(color, Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(color.to_u32(), 0x123456);
    }

    #[test]
    fn spec_colors_pass_through() {
        let theme = TerminalTheme::dark();
        let color = Color::Spec(VteRgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            theme.resolve(color, true, RunFlags::empty()),
            Rgb::new(1, 2, 3)
        );
    }

    #[test]
    fn bold_promotes_named_colors_to_bright() {
        let theme = TerminalTheme::dark();
        let red = Color::Named(NamedColor::Red);
        assert_eq!(theme.resolve(red, true, RunFlags::empty()), theme.ansi[1]);
        assert_eq!(theme.resolve(red, true, RunFlags::BOLD), theme.ansi[9]);
        // Background colors are never brightened.
        assert_eq!(theme.resolve(red, false, RunFlags::BOLD), theme.ansi[1]);
    }

    #[test]
    fn dim_darkens_foreground() {
        let theme = TerminalTheme::dark();
        let dimmed = theme.resolve(Color::Named(NamedColor::Foreground), true, RunFlags::DIM);
        assert!(dimmed.r < theme.foreground.r);
    }

    #[test]
    fn indexed_color_cube() {
        let theme = TerminalTheme::dark();
        // 16 is the first cube entry and is pure black.
        assert_eq!(
            theme.resolve(Color::Indexed(16), true, RunFlags::empty()),
            Rgb::new(0, 0, 0)
        );
        // 231 is the last cube entry and is pure white.
        assert_eq!(
            theme.resolve(Color::Indexed(231), true, RunFlags::empty()),
            Rgb::new(255, 255, 255)
        );
    }

    #[test]
    fn indexed_grayscale_ramp() {
        let theme = TerminalTheme::dark();
        assert_eq!(
            theme.resolve(Color::Indexed(232), true, RunFlags::empty()),
            Rgb::new(8, 8, 8)
        );
        assert_eq!(
            theme.resolve(Color::Indexed(255), true, RunFlags::empty()),
            Rgb::new(238, 238, 238)
        );
    }

    #[test]
    fn named_special_colors() {
        let theme = TerminalTheme::light();
        let flags = RunFlags::empty();
        assert_eq!(
            theme.resolve(Color::Named(NamedColor::Background), false, flags),
            theme.background
        );
        assert_eq!(
            theme.resolve(Color::Named(NamedColor::Cursor), true, flags),
            theme.cursor
        );
    }
}
