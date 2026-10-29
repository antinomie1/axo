//! Terminal color schemes: the 256-color table, named palettes and the
//! kitty-style scheme file format.
//!
//! A [`Palette`] is the whole color system a terminal frame needs: default
//! text and background, cursor and selection (each with an optional text
//! color), and the 16 ANSI colors. Indices 16–255 always come from the
//! standard 6×6×6 cube and gray ramp, as in every other terminal.
//!
//! Schemes can be written as plain text, one `key value` (or `key = value`)
//! pair per line, which is exactly the kitty theme format, so existing theme
//! collections work unchanged.

use axo_term::Color;

use crate::Rgb;

/// A color scheme.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Text drawn under a block cursor; `None` keeps each cell's own color
    /// (kitty's `cursor_text_color none`).
    pub cursor_text: Option<Rgb>,
    /// Fill behind selected cells.
    pub selection: Rgb,
    /// Text over the selection fill; `None` keeps each cell's own foreground.
    pub selection_text: Option<Rgb>,
    /// The 16 ANSI colors; 16–255 are the standard cube and gray ramp.
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    /// kitty's default colors.
    fn default() -> Self {
        KITTY
    }
}

impl Palette {
    /// The color of index `i` in the 256-color table.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [level(i / 36), level(i / 6 % 6), level(i % 6)]
            }
            _ => [8 + (i - 232) * 10; 3],
        }
    }

    /// A cell color; `None` is the default background, left transparent.
    #[inline]
    pub fn resolve(&self, color: Color, foreground: bool) -> Option<Rgb> {
        match color {
            Color::Default if foreground => Some(self.foreground),
            Color::Default => None,
            Color::Indexed(i) => Some(self.indexed(i)),
            Color::Rgb(r, g, b) => Some([r, g, b]),
        }
    }

    /// A dim (SGR 2) text color: pulled toward the background, which keeps the
    /// hue readable in light and dark schemes alike.
    pub fn dim(&self, rgb: Rgb) -> Rgb {
        mix(rgb, self.background, 0.4)
    }

    /// Whether the scheme paints on a light background.
    pub fn is_light(&self) -> bool {
        luminance(self.background) > 0.5
    }

    /// Sets one color by its scheme-file key (`foreground`, `cursor_text_color`,
    /// `selection_background`, `color0`…`color15`); `false` if it is not a color
    /// key, so callers can ignore the rest of a scheme file.
    pub fn set_color(&mut self, key: &str, rgb: Rgb) -> bool {
        match slot(key) {
            Some(Slot::Foreground) => self.foreground = rgb,
            Some(Slot::Background) => self.background = rgb,
            Some(Slot::Cursor) => self.cursor = rgb,
            Some(Slot::CursorText) => self.cursor_text = Some(rgb),
            Some(Slot::Selection) => self.selection = rgb,
            Some(Slot::SelectionText) => self.selection_text = Some(rgb),
            Some(Slot::Ansi(index)) => self.ansi[index] = rgb,
            None => return false,
        }
        true
    }

    /// Applies the color lines of a scheme file, kitty syntax: `key value`,
    /// `key = value`, `#` comment lines, and any non-color keys ignored.
    pub fn apply(&mut self, text: &str) -> Result<(), String> {
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let Some((key, value)) = split_pair(line) else {
                continue;
            };
            if !is_color_key(key) {
                continue;
            }
            if value.eq_ignore_ascii_case("none") {
                match slot(key) {
                    Some(Slot::CursorText) => self.cursor_text = None,
                    Some(Slot::SelectionText) => self.selection_text = None,
                    _ => {
                        return Err(format!("line {}: `none` is not a color", number + 1));
                    }
                }
                continue;
            }
            let Some(rgb) = parse_color(value) else {
                return Err(format!("line {}: `{value}` is not a color", number + 1));
            };
            self.set_color(key, rgb);
        }
        Ok(())
    }

    /// A scheme file parsed over the default palette; keys the file leaves out
    /// keep their default (kitty) color.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut palette = Self::default();
        palette.apply(text)?;
        Ok(palette)
    }
}

/// The scheme names bundled with Axo, in the order the settings offer them.
pub const SCHEMES: &[(&str, Palette)] = &[
    ("kitty", KITTY),
    ("axo-dark", AXO_DARK),
    ("axo-light", AXO_LIGHT),
    ("catppuccin-mocha", CATPPUCCIN_MOCHA),
    ("dracula", DRACULA),
    ("gruvbox-dark", GRUVBOX_DARK),
    ("gruvbox-light", GRUVBOX_LIGHT),
    ("nord", NORD),
    ("one-dark", ONE_DARK),
    ("solarized-dark", SOLARIZED_DARK),
    ("solarized-light", SOLARIZED_LIGHT),
    ("tokyo-night", TOKYO_NIGHT),
];

/// `default` and the legacy `dark`/`light` names resolve to a bundled scheme.
/// Names ignore case and treat `_` as `-`.
pub fn builtin(name: &str) -> Option<Palette> {
    let key = normalize(name);
    if key.is_empty() || key == "default" {
        return Some(KITTY);
    }
    if key == "dark" {
        return Some(AXO_DARK);
    }
    if key == "light" {
        return Some(AXO_LIGHT);
    }
    SCHEMES
        .iter()
        .find(|(scheme, _)| *scheme == key)
        .map(|(_, palette)| palette.clone())
}

/// Parses `#rgb`, `#rrggbb` and `#rrggbbaa` (alpha ignored), with or without
/// the leading `#`.
pub fn parse_color(value: &str) -> Option<Rgb> {
    let value = value.trim().trim_matches('"');
    let hex = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    match hex.len() {
        3 => {
            let mut digits = hex
                .bytes()
                .map(|b| (b as char).to_digit(16).unwrap() as u8 * 17);
            Some([digits.next()?, digits.next()?, digits.next()?])
        }
        6 | 8 => Some([byte(0..2)?, byte(2..4)?, byte(4..6)?]),
        _ => None,
    }
}

/// Whether `key` names a color in the scheme format.
pub fn is_color_key(key: &str) -> bool {
    slot(key).is_some()
}

/// Blends `from` toward `to` by `amount` in `0..=1`.
pub fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
    ]
}

/// The relative luminance of `rgb` in `0..=1`.
pub fn luminance(rgb: Rgb) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        match v <= 0.04045 {
            true => v / 12.92,
            false => ((v + 0.055) / 1.055).powf(2.4),
        }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

/// The contrast ratio of two colors, from 1 (equal) to 21 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    let (high, low) = match a > b {
        true => (a, b),
        false => (b, a),
    };
    (high + 0.05) / (low + 0.05)
}

/// The color a scheme-file key addresses.
enum Slot {
    Foreground,
    Background,
    Cursor,
    CursorText,
    Selection,
    SelectionText,
    Ansi(usize),
}

fn slot(key: &str) -> Option<Slot> {
    match normalize(key).as_str() {
        "foreground" | "foreground-color" => Some(Slot::Foreground),
        "background" | "background-color" => Some(Slot::Background),
        "cursor" | "cursor-color" => Some(Slot::Cursor),
        "cursor-text" | "cursor-text-color" => Some(Slot::CursorText),
        "selection" | "selection-background" | "selection-background-color" => {
            Some(Slot::Selection)
        }
        "selection-foreground" | "selection-foreground-color" => Some(Slot::SelectionText),
        key => ansi_index(key).map(Slot::Ansi),
    }
}

/// Lowercase, with `_` read as `-`.
fn normalize(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('_', "-")
}

fn ansi_index(key: &str) -> Option<usize> {
    let digits = key.strip_prefix("color")?;
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let index: usize = digits.parse().ok()?;
    (index < 16).then_some(index)
}

/// Splits `key value`, `key = value` or `key=value`, dropping trailing comments.
fn split_pair(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, |c: char| c == '=' || c.is_whitespace());
    let key = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim();
    let value = rest.strip_prefix('=').unwrap_or(rest).trim();
    let value = value.split(" #").next().unwrap_or(value).trim();
    (!key.is_empty() && !value.is_empty()).then_some((key, value))
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    [r, g, b]
}

/// kitty's default colors: `color0`–`color15` with `#dddddd` on `#000000`.
pub const KITTY: Palette = Palette {
    foreground: rgb(0xdd, 0xdd, 0xdd),
    background: rgb(0x00, 0x00, 0x00),
    cursor: rgb(0xdd, 0xdd, 0xdd),
    cursor_text: Some(rgb(0x11, 0x11, 0x11)),
    selection: rgb(0xff, 0xfa, 0xcd),
    selection_text: Some(rgb(0x00, 0x00, 0x00)),
    ansi: [
        rgb(0x00, 0x00, 0x00),
        rgb(0xcc, 0x04, 0x03),
        rgb(0x19, 0xcb, 0x00),
        rgb(0xce, 0xcb, 0x00),
        rgb(0x0d, 0x73, 0xcc),
        rgb(0xcb, 0x1e, 0xd1),
        rgb(0x0d, 0xcd, 0xcd),
        rgb(0xdd, 0xdd, 0xdd),
        rgb(0x76, 0x76, 0x76),
        rgb(0xf2, 0x20, 0x1f),
        rgb(0x23, 0xfd, 0x00),
        rgb(0xff, 0xfd, 0x00),
        rgb(0x1a, 0x8f, 0xff),
        rgb(0xfd, 0x28, 0xff),
        rgb(0x14, 0xff, 0xff),
        rgb(0xff, 0xff, 0xff),
    ],
};

/// The original Axo dark palette, kept so old `theme = dark` files look the same.
pub const AXO_DARK: Palette = Palette {
    foreground: rgb(0xc8, 0xd0, 0xe0),
    background: rgb(0x16, 0x18, 0x20),
    cursor: rgb(0xe0, 0xa0, 0x60),
    cursor_text: Some(rgb(0x16, 0x18, 0x20)),
    selection: rgb(0x34, 0x3c, 0x52),
    selection_text: None,
    ansi: [
        rgb(0x20, 0x24, 0x30),
        rgb(0xe0, 0x6c, 0x75),
        rgb(0x98, 0xc3, 0x79),
        rgb(0xe5, 0xc0, 0x7b),
        rgb(0x61, 0xaf, 0xef),
        rgb(0xc6, 0x78, 0xdd),
        rgb(0x56, 0xb6, 0xc2),
        rgb(0xc8, 0xd0, 0xe0),
        rgb(0x5c, 0x63, 0x70),
        rgb(0xff, 0x7a, 0x85),
        rgb(0xb5, 0xe8, 0x90),
        rgb(0xff, 0xd6, 0x8a),
        rgb(0x7c, 0xc4, 0xff),
        rgb(0xdd, 0x95, 0xf0),
        rgb(0x70, 0xd8, 0xe4),
        rgb(0xff, 0xff, 0xff),
    ],
};

/// The original Axo light palette, kept for `theme = light`.
pub const AXO_LIGHT: Palette = Palette {
    foreground: rgb(0x30, 0x34, 0x40),
    background: rgb(0xfa, 0xfa, 0xf8),
    cursor: rgb(0x52, 0x6f, 0xff),
    cursor_text: Some(rgb(0xfa, 0xfa, 0xf8)),
    selection: rgb(0xd0, 0xdc, 0xf4),
    selection_text: None,
    ansi: [
        rgb(0x30, 0x34, 0x40),
        rgb(0xc8, 0x28, 0x29),
        rgb(0x40, 0x8a, 0x2a),
        rgb(0x9a, 0x6a, 0x00),
        rgb(0x20, 0x60, 0xd0),
        rgb(0x9a, 0x2f, 0xb0),
        rgb(0x10, 0x84, 0x90),
        rgb(0xd8, 0xd8, 0xd4),
        rgb(0x70, 0x76, 0x80),
        rgb(0xe0, 0x40, 0x40),
        rgb(0x50, 0xa1, 0x4f),
        rgb(0xc1, 0x84, 0x01),
        rgb(0x40, 0x78, 0xf2),
        rgb(0xa6, 0x26, 0xa4),
        rgb(0x01, 0x84, 0xbc),
        rgb(0xff, 0xff, 0xff),
    ],
};

pub const CATPPUCCIN_MOCHA: Palette = Palette {
    foreground: rgb(0xcd, 0xd6, 0xf4),
    background: rgb(0x1e, 0x1e, 0x2e),
    cursor: rgb(0xf5, 0xe0, 0xdc),
    cursor_text: Some(rgb(0x1e, 0x1e, 0x2e)),
    selection: rgb(0xf5, 0xe0, 0xdc),
    selection_text: Some(rgb(0x1e, 0x1e, 0x2e)),
    ansi: [
        rgb(0x45, 0x47, 0x5a),
        rgb(0xf3, 0x8b, 0xa8),
        rgb(0xa6, 0xe3, 0xa1),
        rgb(0xf9, 0xe2, 0xaf),
        rgb(0x89, 0xb4, 0xfa),
        rgb(0xf5, 0xc2, 0xe7),
        rgb(0x94, 0xe2, 0xd5),
        rgb(0xba, 0xc2, 0xde),
        rgb(0x58, 0x5b, 0x70),
        rgb(0xf3, 0x8b, 0xa8),
        rgb(0xa6, 0xe3, 0xa1),
        rgb(0xf9, 0xe2, 0xaf),
        rgb(0x89, 0xb4, 0xfa),
        rgb(0xf5, 0xc2, 0xe7),
        rgb(0x94, 0xe2, 0xd5),
        rgb(0xa6, 0xad, 0xc8),
    ],
};

pub const DRACULA: Palette = Palette {
    foreground: rgb(0xf8, 0xf8, 0xf2),
    background: rgb(0x28, 0x2a, 0x36),
    cursor: rgb(0xf8, 0xf8, 0xf2),
    cursor_text: Some(rgb(0x28, 0x2a, 0x36)),
    selection: rgb(0x44, 0x47, 0x5a),
    selection_text: None,
    ansi: [
        rgb(0x21, 0x22, 0x2c),
        rgb(0xff, 0x55, 0x55),
        rgb(0x50, 0xfa, 0x7b),
        rgb(0xf1, 0xfa, 0x8c),
        rgb(0xbd, 0x93, 0xf9),
        rgb(0xff, 0x79, 0xc6),
        rgb(0x8b, 0xe9, 0xfd),
        rgb(0xf8, 0xf8, 0xf2),
        rgb(0x62, 0x72, 0xa4),
        rgb(0xff, 0x6e, 0x6e),
        rgb(0x69, 0xff, 0x94),
        rgb(0xff, 0xff, 0xa5),
        rgb(0xd6, 0xac, 0xff),
        rgb(0xff, 0x92, 0xdf),
        rgb(0xa4, 0xff, 0xff),
        rgb(0xff, 0xff, 0xff),
    ],
};

pub const GRUVBOX_DARK: Palette = Palette {
    foreground: rgb(0xeb, 0xdb, 0xb2),
    background: rgb(0x28, 0x28, 0x28),
    cursor: rgb(0xeb, 0xdb, 0xb2),
    cursor_text: Some(rgb(0x28, 0x28, 0x28)),
    selection: rgb(0x50, 0x49, 0x45),
    selection_text: None,
    ansi: [
        rgb(0x28, 0x28, 0x28),
        rgb(0xcc, 0x24, 0x1d),
        rgb(0x98, 0x97, 0x1a),
        rgb(0xd7, 0x99, 0x21),
        rgb(0x45, 0x85, 0x88),
        rgb(0xb1, 0x62, 0x86),
        rgb(0x68, 0x9d, 0x6a),
        rgb(0xa8, 0x99, 0x84),
        rgb(0x92, 0x83, 0x74),
        rgb(0xfb, 0x49, 0x34),
        rgb(0xb8, 0xbb, 0x26),
        rgb(0xfa, 0xbd, 0x2f),
        rgb(0x83, 0xa5, 0x98),
        rgb(0xd3, 0x86, 0x9b),
        rgb(0x8e, 0xc0, 0x7c),
        rgb(0xeb, 0xdb, 0xb2),
    ],
};

pub const GRUVBOX_LIGHT: Palette = Palette {
    foreground: rgb(0x3c, 0x38, 0x36),
    background: rgb(0xfb, 0xf1, 0xc7),
    cursor: rgb(0x3c, 0x38, 0x36),
    cursor_text: Some(rgb(0xfb, 0xf1, 0xc7)),
    selection: rgb(0xd5, 0xc4, 0xa1),
    selection_text: None,
    ansi: [
        rgb(0xfb, 0xf1, 0xc7),
        rgb(0xcc, 0x24, 0x1d),
        rgb(0x98, 0x97, 0x1a),
        rgb(0xd7, 0x99, 0x21),
        rgb(0x45, 0x85, 0x88),
        rgb(0xb1, 0x62, 0x86),
        rgb(0x68, 0x9d, 0x6a),
        rgb(0x7c, 0x6f, 0x64),
        rgb(0x92, 0x83, 0x74),
        rgb(0x9d, 0x00, 0x06),
        rgb(0x79, 0x74, 0x0e),
        rgb(0xb5, 0x76, 0x14),
        rgb(0x07, 0x66, 0x78),
        rgb(0x8f, 0x3f, 0x71),
        rgb(0x42, 0x7b, 0x58),
        rgb(0x3c, 0x38, 0x36),
    ],
};

pub const NORD: Palette = Palette {
    foreground: rgb(0xd8, 0xde, 0xe9),
    background: rgb(0x2e, 0x34, 0x40),
    cursor: rgb(0xd8, 0xde, 0xe9),
    cursor_text: Some(rgb(0x2e, 0x34, 0x40)),
    selection: rgb(0x4c, 0x56, 0x6a),
    selection_text: None,
    ansi: [
        rgb(0x3b, 0x42, 0x52),
        rgb(0xbf, 0x61, 0x6a),
        rgb(0xa3, 0xbe, 0x8c),
        rgb(0xeb, 0xcb, 0x8b),
        rgb(0x81, 0xa1, 0xc1),
        rgb(0xb4, 0x8e, 0xad),
        rgb(0x88, 0xc0, 0xd0),
        rgb(0xe5, 0xe9, 0xf0),
        rgb(0x4c, 0x56, 0x6a),
        rgb(0xbf, 0x61, 0x6a),
        rgb(0xa3, 0xbe, 0x8c),
        rgb(0xeb, 0xcb, 0x8b),
        rgb(0x81, 0xa1, 0xc1),
        rgb(0xb4, 0x8e, 0xad),
        rgb(0x8f, 0xbc, 0xbb),
        rgb(0xec, 0xef, 0xf4),
    ],
};

pub const ONE_DARK: Palette = Palette {
    foreground: rgb(0xab, 0xb2, 0xbf),
    background: rgb(0x28, 0x2c, 0x34),
    cursor: rgb(0x52, 0x8b, 0xff),
    cursor_text: Some(rgb(0x28, 0x2c, 0x34)),
    selection: rgb(0x3e, 0x44, 0x51),
    selection_text: None,
    ansi: [
        rgb(0x28, 0x2c, 0x34),
        rgb(0xe0, 0x6c, 0x75),
        rgb(0x98, 0xc3, 0x79),
        rgb(0xe5, 0xc0, 0x7b),
        rgb(0x61, 0xaf, 0xef),
        rgb(0xc6, 0x78, 0xdd),
        rgb(0x56, 0xb6, 0xc2),
        rgb(0xab, 0xb2, 0xbf),
        rgb(0x5c, 0x63, 0x70),
        rgb(0xe0, 0x6c, 0x75),
        rgb(0x98, 0xc3, 0x79),
        rgb(0xe5, 0xc0, 0x7b),
        rgb(0x61, 0xaf, 0xef),
        rgb(0xc6, 0x78, 0xdd),
        rgb(0x56, 0xb6, 0xc2),
        rgb(0xff, 0xff, 0xff),
    ],
};

pub const SOLARIZED_DARK: Palette = Palette {
    foreground: rgb(0x83, 0x94, 0x96),
    background: rgb(0x00, 0x2b, 0x36),
    cursor: rgb(0x83, 0x94, 0x96),
    cursor_text: Some(rgb(0x00, 0x2b, 0x36)),
    selection: rgb(0x07, 0x36, 0x42),
    selection_text: None,
    ansi: [
        rgb(0x07, 0x36, 0x42),
        rgb(0xdc, 0x32, 0x2f),
        rgb(0x85, 0x99, 0x00),
        rgb(0xb5, 0x89, 0x00),
        rgb(0x26, 0x8b, 0xd2),
        rgb(0xd3, 0x36, 0x82),
        rgb(0x2a, 0xa1, 0x98),
        rgb(0xee, 0xe8, 0xd5),
        rgb(0x00, 0x2b, 0x36),
        rgb(0xcb, 0x4b, 0x16),
        rgb(0x58, 0x6e, 0x75),
        rgb(0x65, 0x7b, 0x83),
        rgb(0x83, 0x94, 0x96),
        rgb(0x6c, 0x71, 0xc4),
        rgb(0x93, 0xa1, 0xa1),
        rgb(0xfd, 0xf6, 0xe3),
    ],
};

pub const SOLARIZED_LIGHT: Palette = Palette {
    foreground: rgb(0x65, 0x7b, 0x83),
    background: rgb(0xfd, 0xf6, 0xe3),
    cursor: rgb(0x65, 0x7b, 0x83),
    cursor_text: Some(rgb(0xfd, 0xf6, 0xe3)),
    selection: rgb(0xee, 0xe8, 0xd5),
    selection_text: None,
    ansi: [
        rgb(0x07, 0x36, 0x42),
        rgb(0xdc, 0x32, 0x2f),
        rgb(0x85, 0x99, 0x00),
        rgb(0xb5, 0x89, 0x00),
        rgb(0x26, 0x8b, 0xd2),
        rgb(0xd3, 0x36, 0x82),
        rgb(0x2a, 0xa1, 0x98),
        rgb(0xee, 0xe8, 0xd5),
        rgb(0x00, 0x2b, 0x36),
        rgb(0xcb, 0x4b, 0x16),
        rgb(0x58, 0x6e, 0x75),
        rgb(0x65, 0x7b, 0x83),
        rgb(0x83, 0x94, 0x96),
        rgb(0x6c, 0x71, 0xc4),
        rgb(0x93, 0xa1, 0xa1),
        rgb(0xfd, 0xf6, 0xe3),
    ],
};

pub const TOKYO_NIGHT: Palette = Palette {
    foreground: rgb(0xc0, 0xca, 0xf5),
    background: rgb(0x1a, 0x1b, 0x26),
    cursor: rgb(0xc0, 0xca, 0xf5),
    cursor_text: Some(rgb(0x1a, 0x1b, 0x26)),
    selection: rgb(0x28, 0x34, 0x57),
    selection_text: None,
    ansi: [
        rgb(0x15, 0x16, 0x1e),
        rgb(0xf7, 0x76, 0x8e),
        rgb(0x9e, 0xce, 0x6a),
        rgb(0xe0, 0xaf, 0x68),
        rgb(0x7a, 0xa2, 0xf7),
        rgb(0xbb, 0x9a, 0xf7),
        rgb(0x7d, 0xcf, 0xff),
        rgb(0xa9, 0xb1, 0xd6),
        rgb(0x41, 0x48, 0x68),
        rgb(0xf7, 0x76, 0x8e),
        rgb(0x9e, 0xce, 0x6a),
        rgb(0xe0, 0xaf, 0x68),
        rgb(0x7a, 0xa2, 0xf7),
        rgb(0xbb, 0x9a, 0xf7),
        rgb(0x7d, 0xcf, 0xff),
        rgb(0xc0, 0xca, 0xf5),
    ],
};
