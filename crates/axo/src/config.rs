//! User settings in `$XDG_CONFIG_HOME/axo/axo.conf`: `key = value` lines.

use std::{fs, io, path::PathBuf};

use aegle::prelude::{Color, Theme};
use axo_render::{CursorShape, FontSpec, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font_family: String,
    /// Logical pixels.
    pub font_size: f32,
    pub line_height: f32,
    pub theme: ThemeName,
    /// Window opacity in `[0.3, 1]`; below 1 needs a transparent window at startup.
    pub opacity: f32,
    /// Terminal padding in logical pixels.
    pub padding: f32,
    pub cursor: CursorShape,
    pub scrollback: usize,
    /// Whether the tab bar is shown; when off, the workspace holds a single
    /// tab and new tabs are disabled.
    pub tab_bar: bool,
    /// Empty: `$SHELL`.
    pub shell: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: "monospace".into(),
            font_size: 14.0,
            line_height: 1.0,
            theme: ThemeName::Dark,
            opacity: 1.0,
            padding: 6.0,
            cursor: CursorShape::Block,
            scrollback: 10_000,
            tab_bar: true,
            shell: String::new(),
        }
    }
}

const DARK: Palette = Palette {
    foreground: [0xc8, 0xd0, 0xe0],
    background: [0x16, 0x18, 0x20],
    cursor: [0xe0, 0xa0, 0x60],
    selection: [0x34, 0x3c, 0x52],
    ansi: [
        [0x20, 0x24, 0x30],
        [0xe0, 0x6c, 0x75],
        [0x98, 0xc3, 0x79],
        [0xe5, 0xc0, 0x7b],
        [0x61, 0xaf, 0xef],
        [0xc6, 0x78, 0xdd],
        [0x56, 0xb6, 0xc2],
        [0xc8, 0xd0, 0xe0],
        [0x5c, 0x63, 0x70],
        [0xff, 0x7a, 0x85],
        [0xb5, 0xe8, 0x90],
        [0xff, 0xd6, 0x8a],
        [0x7c, 0xc4, 0xff],
        [0xdd, 0x95, 0xf0],
        [0x70, 0xd8, 0xe4],
        [0xff, 0xff, 0xff],
    ],
};

const LIGHT: Palette = Palette {
    foreground: [0x30, 0x34, 0x40],
    background: [0xfa, 0xfa, 0xf8],
    cursor: [0x52, 0x6f, 0xff],
    selection: [0xd0, 0xdc, 0xf4],
    ansi: [
        [0x30, 0x34, 0x40],
        [0xc8, 0x28, 0x29],
        [0x40, 0x8a, 0x2a],
        [0x9a, 0x6a, 0x00],
        [0x20, 0x60, 0xd0],
        [0x9a, 0x2f, 0xb0],
        [0x10, 0x84, 0x90],
        [0xd8, 0xd8, 0xd4],
        [0x70, 0x76, 0x80],
        [0xe0, 0x40, 0x40],
        [0x50, 0xa1, 0x4f],
        [0xc1, 0x84, 0x01],
        [0x40, 0x78, 0xf2],
        [0xa6, 0x26, 0xa4],
        [0x01, 0x84, 0xbc],
        [0xff, 0xff, 0xff],
    ],
};

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("axo").join("axo.conf"))
}

impl Config {
    /// Reads the file over the defaults; unknown keys and bad values are ignored.
    pub fn load() -> Self {
        let mut config = Self::default();
        let Some(text) = path().and_then(|path| fs::read_to_string(path).ok()) else {
            return config;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            let float = || value.parse::<f32>().ok().filter(|v| v.is_finite());
            match key {
                "font_family" if !value.is_empty() => config.font_family = value.into(),
                "font_size" => {
                    config.font_size = float().map_or(config.font_size, |v| v.clamp(4.0, 96.0))
                }
                "line_height" => {
                    config.line_height = float().map_or(config.line_height, |v| v.clamp(0.5, 3.0))
                }
                "theme" => {
                    config.theme = if value == "light" {
                        ThemeName::Light
                    } else {
                        ThemeName::Dark
                    }
                }
                "opacity" => config.opacity = float().map_or(config.opacity, |v| v.clamp(0.3, 1.0)),
                "padding" => {
                    config.padding = float().map_or(config.padding, |v| v.clamp(0.0, 64.0))
                }
                "cursor" => {
                    config.cursor = match value {
                        "bar" => CursorShape::Bar,
                        "underline" => CursorShape::Underline,
                        _ => CursorShape::Block,
                    }
                }
                "scrollback" => {
                    config.scrollback = value
                        .parse()
                        .map_or(config.scrollback, |v: usize| v.min(1_000_000))
                }
                "tab_bar" => {
                    config.tab_bar = match value {
                        "true" | "1" | "yes" | "on" => true,
                        "false" | "0" | "no" | "off" => false,
                        _ => config.tab_bar,
                    }
                }
                "shell" => config.shell = value.into(),
                _ => {}
            }
        }
        config
    }

    pub fn save(&self) -> io::Result<PathBuf> {
        let path = path().ok_or_else(|| io::Error::other("no HOME or XDG_CONFIG_HOME"))?;
        fs::create_dir_all(path.parent().unwrap())?;
        let theme = match self.theme {
            ThemeName::Dark => "dark",
            ThemeName::Light => "light",
        };
        let cursor = match self.cursor {
            CursorShape::Block => "block",
            CursorShape::Bar => "bar",
            CursorShape::Underline => "underline",
        };
        let text = format!(
            "# Axo Terminal settings\nfont_family = {}\nfont_size = {}\nline_height = {}\ntheme = {theme}\nopacity = {}\npadding = {}\ncursor = {cursor}\nscrollback = {}\ntab_bar = {}\nshell = {}\n",
            self.font_family,
            self.font_size,
            self.line_height,
            self.opacity,
            self.padding,
            self.scrollback,
            self.tab_bar,
            self.shell,
        );
        fs::write(&path, text)?;
        Ok(path)
    }

    pub fn font(&self) -> FontSpec {
        FontSpec {
            family: self.font_family.clone(),
            size: self.font_size,
            line_height: self.line_height,
        }
    }

    pub fn palette(&self) -> Palette {
        match self.theme {
            ThemeName::Dark => DARK,
            ThemeName::Light => LIGHT,
        }
    }

    pub fn shell(&self) -> String {
        match self.shell.trim() {
            "" => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            shell => shell.into(),
        }
    }

    /// The window theme: the terminal background, at `opacity` when the window is
    /// transparent, behind every block.
    pub fn theme(&self, transparent: bool) -> Theme {
        let palette = self.palette();
        let base = match self.theme {
            ThemeName::Dark => Theme::dark(),
            ThemeName::Light => Theme::light(),
        };
        let alpha = if transparent {
            (self.opacity * 255.0).round() as u8
        } else {
            255
        };
        let [r, g, b] = palette.background;
        let [fr, fg, fb] = palette.foreground;
        Theme {
            background: Color::rgba(r, g, b, alpha),
            foreground: Color::rgb(fr, fg, fb),
            radius: 4.0,
            ..base
        }
    }

    /// The tab bar's fill: the window background pushed a shade darker, so the
    /// chrome reads as a strip above the terminal in both themes and keeps the
    /// window's opacity.
    pub fn bar_color(&self, transparent: bool) -> Color {
        let [r, g, b] = self.palette().background;
        let shade = match self.theme {
            ThemeName::Dark => 0.70,
            ThemeName::Light => 0.92,
        };
        let channel = |value: u8| (f32::from(value) * shade).round() as u8;
        let alpha = match transparent {
            true => (self.opacity * 255.0).round() as u8,
            false => 255,
        };
        Color::rgba(channel(r), channel(g), channel(b), alpha)
    }

    /// The active tab's fill: a faint foreground wash over the bar.
    pub fn tab_fill(&self) -> Color {
        let [r, g, b] = self.palette().foreground;
        Color::rgba(r, g, b, 18)
    }
}
