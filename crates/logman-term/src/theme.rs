//! Color palettes for the terminal grid.
//!
//! `alacritty_terminal` deliberately leaves the actual color values to the
//! embedding application: [`alacritty_terminal::term::color::Colors`] starts out
//! completely empty and is only filled in by escape sequences such as `OSC 4`.
//! This module supplies the concrete palette that turns the abstract
//! [`Color`](alacritty_terminal::vte::ansi::Color) values stored in every grid
//! cell into renderable RGB triples.
//!
//! # Built-in schemes
//!
//! A handful of well known palettes ship with logman and can be selected from
//! the settings by their stable id; see [`TerminalTheme::builtin`] and
//! [`TerminalTheme::by_name`].
//!
//! Where a scheme's upstream specification only names semantic colors (such as
//! Solarized's `base01` or Dracula's `Comment`), the mapping onto the 16 ANSI
//! slots follows that project's own terminal implementation. The source used
//! for each scheme is documented on its constructor.

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

/// Identifier and display name of one built-in scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemeInfo {
    /// Stable id stored in settings, e.g. `"solarized-dark"`.
    pub id: &'static str,
    /// Human-readable name, e.g. `"Solarized Dark"`.
    pub name: &'static str,
    /// Whether the scheme is dark; lets the UI group or pair them.
    pub dark: bool,
}

/// Id of the default scheme.
const ID_ONE_DARK: &str = "one-dark";
/// Id of the light counterpart of [`ID_ONE_DARK`].
const ID_ONE_LIGHT: &str = "one-light";
/// Id of the dark Solarized scheme.
const ID_SOLARIZED_DARK: &str = "solarized-dark";
/// Id of the light Solarized scheme.
const ID_SOLARIZED_LIGHT: &str = "solarized-light";
/// Id of the dark Gruvbox scheme.
const ID_GRUVBOX_DARK: &str = "gruvbox-dark";
/// Id of the Dracula scheme.
const ID_DRACULA: &str = "dracula";

/// Every built-in scheme, in the order they should be presented.
const BUILTIN_SCHEMES: [SchemeInfo; 6] = [
    SchemeInfo {
        id: ID_ONE_DARK,
        name: "One Dark",
        dark: true,
    },
    SchemeInfo {
        id: ID_ONE_LIGHT,
        name: "One Light",
        dark: false,
    },
    SchemeInfo {
        id: ID_SOLARIZED_DARK,
        name: "Solarized Dark",
        dark: true,
    },
    SchemeInfo {
        id: ID_SOLARIZED_LIGHT,
        name: "Solarized Light",
        dark: false,
    },
    SchemeInfo {
        id: ID_GRUVBOX_DARK,
        name: "Gruvbox Dark",
        dark: true,
    },
    SchemeInfo {
        id: ID_DRACULA,
        name: "Dracula",
        dark: true,
    },
];

/// The shared Solarized accent + base palette laid out over the 16 ANSI slots.
///
/// Solarized defines a single palette for both of its variants; only the
/// foreground and background picks differ. The ANSI assignment below is the one
/// from Ethan Schoonover's official `solarized.xresources`:
/// `color0` = base02, `color8` = base03, `color9` = orange, `color10` = base01,
/// `color11` = base00, `color12` = base0, `color13` = violet, `color14` = base1,
/// `color7` = base2 and `color15` = base3.
const SOLARIZED_ANSI: [Rgb; 16] = [
    // Normal.
    Rgb::from_u32(0x073642), // black   -> base02
    Rgb::from_u32(0xdc322f), // red     -> red
    Rgb::from_u32(0x859900), // green   -> green
    Rgb::from_u32(0xb58900), // yellow  -> yellow
    Rgb::from_u32(0x268bd2), // blue    -> blue
    Rgb::from_u32(0xd33682), // magenta -> magenta
    Rgb::from_u32(0x2aa198), // cyan    -> cyan
    Rgb::from_u32(0xeee8d5), // white   -> base2
    // Bright.
    Rgb::from_u32(0x002b36), // bright black   -> base03
    Rgb::from_u32(0xcb4b16), // bright red     -> orange
    Rgb::from_u32(0x586e75), // bright green   -> base01
    Rgb::from_u32(0x657b83), // bright yellow  -> base00
    Rgb::from_u32(0x839496), // bright blue    -> base0
    Rgb::from_u32(0x6c71c4), // bright magenta -> violet
    Rgb::from_u32(0x93a1a1), // bright cyan    -> base1
    Rgb::from_u32(0xfdf6e3), // bright white   -> base3
];

impl TerminalTheme {
    /// A dark palette in the spirit of Zed's / Atom's "One Dark".
    ///
    /// This is the default scheme and is registered under the id `one-dark`.
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
    ///
    /// Registered under the id `one-light`.
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

    /// Solarized Dark by Ethan Schoonover.
    ///
    /// Background is `base03` (`#002b36`) and foreground is `base0`
    /// (`#839496`), with `base02` as the selection background. The 16 ANSI
    /// colors come from the project's official `solarized.xresources`.
    pub fn solarized_dark() -> Self {
        Self {
            foreground: Rgb::from_u32(0x839496), // base0
            background: Rgb::from_u32(0x002b36), // base03
            cursor: Rgb::from_u32(0x93a1a1),     // base1
            selection: Rgb::from_u32(0x073642),  // base02
            ansi: SOLARIZED_ANSI,
        }
    }

    /// Solarized Light by Ethan Schoonover.
    ///
    /// Shares its palette with [`TerminalTheme::solarized_dark`]; only the
    /// background (`base3`, `#fdf6e3`), foreground (`base00`, `#657b83`) and
    /// selection (`base2`) are flipped to the light end of the base ramp.
    pub fn solarized_light() -> Self {
        Self {
            foreground: Rgb::from_u32(0x657b83), // base00
            background: Rgb::from_u32(0xfdf6e3), // base3
            cursor: Rgb::from_u32(0x586e75),     // base01
            selection: Rgb::from_u32(0xeee8d5),  // base2
            ansi: SOLARIZED_ANSI,
        }
    }

    /// Gruvbox Dark by morhetz.
    ///
    /// Uses the medium contrast background `dark0` (`#282828`) and `light1`
    /// (`#ebdbb2`) as foreground. The ANSI assignment is the one from
    /// `gruvbox-contrib`'s `gruvbox-dark.xresources`: the neutral colors fill
    /// slots 1-6, their bright variants slots 9-14, `gray` is bright black and
    /// `light4` / `light1` are white and bright white.
    pub fn gruvbox_dark() -> Self {
        Self {
            foreground: Rgb::from_u32(0xebdbb2), // light1
            background: Rgb::from_u32(0x282828), // dark0
            cursor: Rgb::from_u32(0xebdbb2),     // light1
            selection: Rgb::from_u32(0x504945),  // dark2
            ansi: [
                // Normal.
                Rgb::from_u32(0x282828), // black   -> dark0
                Rgb::from_u32(0xcc241d), // red     -> neutral red
                Rgb::from_u32(0x98971a), // green   -> neutral green
                Rgb::from_u32(0xd79921), // yellow  -> neutral yellow
                Rgb::from_u32(0x458588), // blue    -> neutral blue
                Rgb::from_u32(0xb16286), // magenta -> neutral purple
                Rgb::from_u32(0x689d6a), // cyan    -> neutral aqua
                Rgb::from_u32(0xa89984), // white   -> light4
                // Bright.
                Rgb::from_u32(0x928374), // bright black   -> gray
                Rgb::from_u32(0xfb4934), // bright red
                Rgb::from_u32(0xb8bb26), // bright green
                Rgb::from_u32(0xfabd2f), // bright yellow
                Rgb::from_u32(0x83a598), // bright blue
                Rgb::from_u32(0xd3869b), // bright magenta -> bright purple
                Rgb::from_u32(0x8ec07c), // bright cyan    -> bright aqua
                Rgb::from_u32(0xebdbb2), // bright white   -> light1
            ],
        }
    }

    /// Dracula, the official palette from `draculatheme.com`.
    ///
    /// Background `#282a36`, foreground `#f8f8f2` and the `Current Line` color
    /// `#44475a` as selection background. The 16 ANSI colors are taken from the
    /// project's own `dracula/xresources` definition, which maps `Purple` onto
    /// blue, `Pink` onto magenta and keeps neutral black/white ramps rather than
    /// reusing the accent colors.
    pub fn dracula() -> Self {
        Self {
            foreground: Rgb::from_u32(0xf8f8f2),
            background: Rgb::from_u32(0x282a36),
            cursor: Rgb::from_u32(0xf8f8f2),
            selection: Rgb::from_u32(0x44475a), // current line
            ansi: [
                // Normal.
                Rgb::from_u32(0x000000), // black
                Rgb::from_u32(0xff5555), // red
                Rgb::from_u32(0x50fa7b), // green
                Rgb::from_u32(0xf1fa8c), // yellow
                Rgb::from_u32(0xbd93f9), // blue    -> purple
                Rgb::from_u32(0xff79c6), // magenta -> pink
                Rgb::from_u32(0x8be9fd), // cyan
                Rgb::from_u32(0xbfbfbf), // white
                // Bright.
                Rgb::from_u32(0x4d4d4d), // bright black
                Rgb::from_u32(0xff6e67), // bright red
                Rgb::from_u32(0x5af78e), // bright green
                Rgb::from_u32(0xf4f99d), // bright yellow
                Rgb::from_u32(0xcaa9fa), // bright blue    -> bright purple
                Rgb::from_u32(0xff92d0), // bright magenta -> bright pink
                Rgb::from_u32(0x9aedfe), // bright cyan
                Rgb::from_u32(0xe6e6e6), // bright white
            ],
        }
    }

    /// All built-in schemes, in presentation order.
    pub fn builtin() -> &'static [SchemeInfo] {
        &BUILTIN_SCHEMES
    }

    /// Look up a scheme by its stable id. Ids are case-insensitive.
    ///
    /// Returns `None` for ids that are not built in, which lets the settings
    /// layer distinguish a typo from a deliberate choice.
    pub fn by_name(id: &str) -> Option<Self> {
        if id.eq_ignore_ascii_case(ID_ONE_DARK) {
            Some(Self::dark())
        } else if id.eq_ignore_ascii_case(ID_ONE_LIGHT) {
            Some(Self::light())
        } else if id.eq_ignore_ascii_case(ID_SOLARIZED_DARK) {
            Some(Self::solarized_dark())
        } else if id.eq_ignore_ascii_case(ID_SOLARIZED_LIGHT) {
            Some(Self::solarized_light())
        } else if id.eq_ignore_ascii_case(ID_GRUVBOX_DARK) {
            Some(Self::gruvbox_dark())
        } else if id.eq_ignore_ascii_case(ID_DRACULA) {
            Some(Self::dracula())
        } else {
            None
        }
    }

    /// [`TerminalTheme::by_name`] with a fallback to the default scheme
    /// (`one-dark`), for settings that may hold a stale or misspelled id.
    pub fn by_name_or_default(id: &str) -> Self {
        Self::by_name(id).unwrap_or_default()
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
    fn builtin_lists_every_scheme_exactly_once() {
        let schemes = TerminalTheme::builtin();
        assert_eq!(schemes.len(), 6);

        let mut ids: Vec<&str> = schemes.iter().map(|scheme| scheme.id).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "duplicate scheme id");

        let mut names: Vec<&str> = schemes.iter().map(|scheme| scheme.name).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "duplicate scheme name");
    }

    #[test]
    fn every_advertised_id_resolves() {
        for scheme in TerminalTheme::builtin() {
            assert!(
                TerminalTheme::by_name(scheme.id).is_some(),
                "missing scheme {}",
                scheme.id
            );
        }
    }

    #[test]
    fn by_name_knows_all_six_schemes() {
        for id in [
            "one-dark",
            "one-light",
            "solarized-dark",
            "solarized-light",
            "gruvbox-dark",
            "dracula",
        ] {
            assert!(TerminalTheme::by_name(id).is_some(), "missing scheme {id}");
        }
    }

    #[test]
    fn by_name_is_case_insensitive() {
        let lower = TerminalTheme::by_name("solarized-dark").expect("scheme");
        let upper = TerminalTheme::by_name("SOLARIZED-DARK").expect("scheme");
        let mixed = TerminalTheme::by_name("Solarized-Dark").expect("scheme");
        assert_eq!(lower.background, upper.background);
        assert_eq!(lower.background, mixed.background);
        assert_eq!(lower.ansi, mixed.ansi);
    }

    #[test]
    fn by_name_rejects_unknown_ids() {
        assert!(TerminalTheme::by_name("nonsense").is_none());
        assert!(TerminalTheme::by_name("").is_none());
        assert!(TerminalTheme::by_name("solarized").is_none());
    }

    #[test]
    fn by_name_or_default_falls_back_to_one_dark() {
        let fallback = TerminalTheme::by_name_or_default("nonsense");
        let expected = TerminalTheme::dark();
        assert_eq!(fallback.foreground, expected.foreground);
        assert_eq!(fallback.background, expected.background);
        assert_eq!(fallback.cursor, expected.cursor);
        assert_eq!(fallback.selection, expected.selection);
        assert_eq!(fallback.ansi, expected.ansi);

        // A known id is still honoured.
        assert_eq!(
            TerminalTheme::by_name_or_default("dracula").background,
            TerminalTheme::dracula().background
        );
    }

    #[test]
    fn dark_and_light_are_the_one_dark_and_one_light_entries() {
        assert_eq!(
            TerminalTheme::by_name("one-dark").unwrap().ansi,
            TerminalTheme::dark().ansi
        );
        assert_eq!(
            TerminalTheme::by_name("one-light").unwrap().ansi,
            TerminalTheme::light().ansi
        );
        assert_eq!(
            TerminalTheme::default().background,
            TerminalTheme::dark().background
        );
    }

    #[test]
    fn scheme_darkness_flags_match_the_palettes() {
        for scheme in TerminalTheme::builtin() {
            let theme = TerminalTheme::by_name(scheme.id).expect("scheme");
            let brightness = |color: Rgb| color.r as u32 + color.g as u32 + color.b as u32;
            let is_dark = brightness(theme.background) < brightness(theme.foreground);
            assert_eq!(is_dark, scheme.dark, "wrong `dark` flag for {}", scheme.id);
        }
    }

    #[test]
    fn signature_colors_match_the_published_specs() {
        assert_eq!(
            TerminalTheme::solarized_dark().background,
            Rgb::from_u32(0x002b36),
            "solarized base03"
        );
        assert_eq!(
            TerminalTheme::solarized_dark().foreground,
            Rgb::from_u32(0x839496),
            "solarized base0"
        );
        assert_eq!(
            TerminalTheme::solarized_light().background,
            Rgb::from_u32(0xfdf6e3),
            "solarized base3"
        );
        assert_eq!(
            TerminalTheme::solarized_light().foreground,
            Rgb::from_u32(0x657b83),
            "solarized base00"
        );
        assert_eq!(TerminalTheme::dracula().foreground, Rgb::from_u32(0xf8f8f2));
        assert_eq!(TerminalTheme::dracula().background, Rgb::from_u32(0x282a36));
        assert_eq!(TerminalTheme::dracula().selection, Rgb::from_u32(0x44475a));
        assert_eq!(
            TerminalTheme::gruvbox_dark().background,
            Rgb::from_u32(0x282828)
        );
        assert_eq!(
            TerminalTheme::gruvbox_dark().foreground,
            Rgb::from_u32(0xebdbb2)
        );
    }

    #[test]
    fn solarized_variants_share_one_palette() {
        assert_eq!(
            TerminalTheme::solarized_dark().ansi,
            TerminalTheme::solarized_light().ansi
        );
        // Spot check the accent colors against the published palette.
        let ansi = TerminalTheme::solarized_dark().ansi;
        assert_eq!(ansi[1], Rgb::from_u32(0xdc322f), "red");
        assert_eq!(ansi[2], Rgb::from_u32(0x859900), "green");
        assert_eq!(ansi[3], Rgb::from_u32(0xb58900), "yellow");
        assert_eq!(ansi[4], Rgb::from_u32(0x268bd2), "blue");
        assert_eq!(ansi[5], Rgb::from_u32(0xd33682), "magenta");
        assert_eq!(ansi[6], Rgb::from_u32(0x2aa198), "cyan");
        assert_eq!(ansi[9], Rgb::from_u32(0xcb4b16), "orange");
        assert_eq!(ansi[13], Rgb::from_u32(0x6c71c4), "violet");
    }

    #[test]
    fn gruvbox_pairs_neutral_and_bright_variants() {
        let ansi = TerminalTheme::gruvbox_dark().ansi;
        let pairs = [
            (1usize, 0xcc241du32, 0xfb4934u32), // red
            (2, 0x98971a, 0xb8bb26),            // green
            (3, 0xd79921, 0xfabd2f),            // yellow
            (4, 0x458588, 0x83a598),            // blue
            (5, 0xb16286, 0xd3869b),            // purple
            (6, 0x689d6a, 0x8ec07c),            // aqua
        ];
        for (index, neutral, bright) in pairs {
            assert_eq!(ansi[index], Rgb::from_u32(neutral), "neutral slot {index}");
            assert_eq!(
                ansi[index + 8],
                Rgb::from_u32(bright),
                "bright slot {index}"
            );
        }
        assert_eq!(ansi[8], Rgb::from_u32(0x928374), "gray");
    }

    #[test]
    fn dracula_maps_purple_to_blue_and_pink_to_magenta() {
        let ansi = TerminalTheme::dracula().ansi;
        assert_eq!(ansi[1], Rgb::from_u32(0xff5555), "red");
        assert_eq!(ansi[2], Rgb::from_u32(0x50fa7b), "green");
        assert_eq!(ansi[3], Rgb::from_u32(0xf1fa8c), "yellow");
        assert_eq!(ansi[4], Rgb::from_u32(0xbd93f9), "purple");
        assert_eq!(ansi[5], Rgb::from_u32(0xff79c6), "pink");
        assert_eq!(ansi[6], Rgb::from_u32(0x8be9fd), "cyan");
        assert_eq!(ansi[12], Rgb::from_u32(0xcaa9fa), "bright purple");
        assert_eq!(ansi[13], Rgb::from_u32(0xff92d0), "bright pink");
    }

    #[test]
    fn every_scheme_resolves_named_colors_from_its_own_palette() {
        for scheme in TerminalTheme::builtin() {
            let theme = TerminalTheme::by_name(scheme.id).expect("scheme");
            let flags = RunFlags::empty();
            assert_eq!(
                theme.resolve(Color::Named(NamedColor::Background), false, flags),
                theme.background,
                "{}",
                scheme.id
            );
            assert_eq!(
                theme.resolve(Color::Named(NamedColor::Red), true, RunFlags::BOLD),
                theme.ansi[9],
                "{}",
                scheme.id
            );
        }
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
