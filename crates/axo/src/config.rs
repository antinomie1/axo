//! User settings in the platform's config directory (`$XDG_CONFIG_HOME/axo` on
//! Unix, `%APPDATA%\axo` on Windows) as `key = value` lines.

use std::{fs, io, path::PathBuf};

use aegle::prelude::{Color, Theme};
use axo_render::{
    CursorShape, FontSpec, Palette, Rgb,
    palette::{self, SCHEMES},
};

/// Font-size bounds in logical pixels, shared by the settings, keyboard
/// shortcuts and Ctrl+wheel zoom.
pub const MIN_FONT_SIZE: f32 = 4.0;
pub const MAX_FONT_SIZE: f32 = 96.0;

/// The shell used when the settings name none: the login shell on Unix, the
/// command processor on Windows.
fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font_family: String,
    /// Logical pixels.
    pub font_size: f32,
    pub line_height: f32,
    /// Color scheme: a bundled name, a name in the themes directory, or the
    /// path of a scheme file.
    pub theme: String,
    /// Colors applied over the scheme, `(key, value)` as in the file.
    pub colors: Vec<(String, String)>,
    /// Window opacity in `[0.3, 1]`; below 1 needs a transparent window at startup.
    pub opacity: f32,
    /// Terminal padding in logical pixels.
    pub padding: f32,
    pub cursor: CursorShape,
    pub scrollback: usize,
    /// Whether the tab bar is shown; when off, the workspace holds a single
    /// tab and new tabs are disabled.
    pub tab_bar: bool,
    /// Empty: the platform's default shell (`$SHELL`, or `%COMSPEC%`).
    pub shell: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: "monospace".into(),
            font_size: 14.0,
            line_height: 1.0,
            theme: "kitty".into(),
            colors: Vec::new(),
            opacity: 1.0,
            padding: 6.0,
            cursor: CursorShape::Block,
            scrollback: 10_000,
            tab_bar: true,
            shell: String::new(),
        }
    }
}

/// The user's home directory, which `~` abbreviates and expands to.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The Axo configuration directory: `$XDG_CONFIG_HOME/axo` on Unix,
/// `%APPDATA%\axo` on Windows, where roaming settings belong.
fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("axo"))
    }
    #[cfg(unix)]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".config")))
            .map(|base| base.join("axo"))
    }
}

/// Where custom color schemes live: `themes` inside the configuration directory.
pub fn theme_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("themes"))
}

fn path() -> Option<PathBuf> {
    Some(config_dir()?.join("axo.conf"))
}

/// The schemes the settings offer: bundled names first, then each `.conf` file
/// in the themes directory, alphabetically.
pub fn scheme_names() -> Vec<String> {
    let mut names: Vec<String> = SCHEMES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    if let Some(dir) = theme_dir()
        && let Ok(entries) = fs::read_dir(dir)
    {
        let mut custom: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("conf"))
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .collect();
        custom.sort();
        for name in custom {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// `~/…` paths are relative to the home directory.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), home()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb[0], rgb[1], rgb[2])
}

/// The scheme last reported as unreadable, so a broken `theme` warns once
/// instead of on every settings change.
static WARNED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn warn_unreadable(theme: &str, error: &str) {
    let mut warned = WARNED.lock().unwrap_or_else(|error| error.into_inner());
    if warned.as_deref() == Some(theme) {
        return;
    }
    eprintln!("axo: color scheme `{theme}`: {error}; using the default");
    *warned = Some(theme.to_string());
}

/// Records a validated color override, replacing an earlier one for the key.
fn override_color(config: &mut Config, key: &str, value: &str) {
    if palette::parse_color(value).is_none() {
        return;
    }
    match config.colors.iter_mut().find(|(name, _)| name == key) {
        Some((_, current)) => *current = value.into(),
        None => config.colors.push((key.into(), value.into())),
    }
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
                    config.font_size =
                        float().map_or(config.font_size, |v| v.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE))
                }
                "line_height" => {
                    config.line_height = float().map_or(config.line_height, |v| v.clamp(0.5, 3.0))
                }
                "theme" if !value.is_empty() => config.theme = value.into(),
                // `cursor` is the shape, unless it names a color, which makes
                // it a cursor color override.
                "cursor" if palette::parse_color(value).is_some() => {
                    override_color(&mut config, key, value);
                }
                "cursor" => {
                    config.cursor = match value {
                        "bar" => CursorShape::Bar,
                        "underline" => CursorShape::Underline,
                        _ => CursorShape::Block,
                    }
                }
                key if palette::is_color_key(key) => override_color(&mut config, key, value),
                "opacity" => config.opacity = float().map_or(config.opacity, |v| v.clamp(0.3, 1.0)),
                "padding" => {
                    config.padding = float().map_or(config.padding, |v| v.clamp(0.0, 64.0))
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
        let path = path().ok_or_else(|| io::Error::other("no configuration directory"))?;
        fs::create_dir_all(path.parent().unwrap())?;
        let cursor = match self.cursor {
            CursorShape::Block => "block",
            CursorShape::Bar => "bar",
            CursorShape::Underline => "underline",
        };
        let mut text = format!(
            "# Axo Terminal settings\nfont_family = {}\nfont_size = {}\nline_height = {}\ntheme = {}\nopacity = {}\npadding = {}\ncursor = {cursor}\nscrollback = {}\ntab_bar = {}\nshell = {}\n",
            self.font_family,
            self.font_size,
            self.line_height,
            self.theme,
            self.opacity,
            self.padding,
            self.scrollback,
            self.tab_bar,
            self.shell,
        );
        for (key, value) in &self.colors {
            text.push_str(&format!("{key} = {value}\n"));
        }
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

    /// The color scheme named by `theme`, then the per-color overrides on top.
    pub fn palette(&self) -> Palette {
        let mut scheme = match palette::builtin(&self.theme) {
            Some(scheme) => scheme,
            None if self.theme.trim().is_empty() => Palette::default(),
            None => match self.read_theme() {
                Ok(scheme) => scheme,
                Err(error) => {
                    warn_unreadable(&self.theme, &error);
                    Palette::default()
                }
            },
        };
        for (key, value) in &self.colors {
            if let Some(rgb) = palette::parse_color(value) {
                scheme.set_color(key, rgb);
            }
        }
        scheme
    }

    fn read_theme(&self) -> Result<Palette, String> {
        let path = self
            .theme_path()
            .ok_or_else(|| "no themes directory".to_string())?;
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        Palette::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// Where `theme` points: a path (contains `/` or ends in `.conf`) or
    /// `<themes dir>/<name>.conf`.
    fn theme_path(&self) -> Option<PathBuf> {
        let name = self.theme.trim();
        if name.is_empty() {
            return None;
        }
        if name.contains('/') || name.ends_with(".conf") {
            return Some(expand_home(name));
        }
        Some(theme_dir()?.join(format!("{name}.conf")))
    }

    pub fn shell(&self) -> String {
        match self.shell.trim() {
            "" => default_shell(),
            shell => shell.into(),
        }
    }

    /// The window theme: the scheme's background behind every block, with the
    /// surrounding chrome shaded from the same colors, so the whole window
    /// follows the terminal scheme.
    pub fn theme(&self, transparent: bool) -> Theme {
        let scheme = self.palette();
        let light = scheme.is_light();
        let (bg, fg) = (scheme.background, scheme.foreground);
        let accent = scheme.ansi[4];
        let alpha = match transparent {
            true => (self.opacity * 255.0).round() as u8,
            false => 255,
        };
        let shade = |amount: f32| color(palette::mix(bg, fg, amount));
        let on_accent = match palette::contrast(accent, bg) >= palette::contrast(accent, fg) {
            true => bg,
            false => fg,
        };
        Theme {
            background: Color::rgba(bg[0], bg[1], bg[2], alpha),
            surface: shade(if light { 0.05 } else { 0.09 }),
            control: shade(if light { 0.11 } else { 0.16 }),
            foreground: color(fg),
            muted: shade(0.6),
            border: shade(if light { 0.48 } else { 0.55 }),
            divider: shade(if light { 0.18 } else { 0.24 }),
            accent: color(accent),
            on_accent: color(on_accent),
            hover: Color::rgba(fg[0], fg[1], fg[2], 15),
            pressed: Color::rgba(fg[0], fg[1], fg[2], 30),
            radius: 4.0,
            ..match light {
                true => Theme::light(),
                false => Theme::dark(),
            }
        }
    }

    /// The tab bar's fill: the scheme background nudged toward the text color,
    /// so the bar separates from the terminal even for pure black or pure
    /// white backgrounds, and keeps the window's opacity.
    pub fn bar_color(&self, transparent: bool) -> Color {
        let scheme = self.palette();
        let amount = match scheme.is_light() {
            true => 0.06,
            false => 0.09,
        };
        let [r, g, b] = palette::mix(scheme.background, scheme.foreground, amount);
        let alpha = match transparent {
            true => (self.opacity * 255.0).round() as u8,
            false => 255,
        };
        Color::rgba(r, g, b, alpha)
    }

    /// The active tab's fill: a wash of the scheme's accent (ANSI blue) over
    /// the bar.
    pub fn tab_fill(&self) -> Color {
        let scheme = self.palette();
        let [r, g, b] = scheme.ansi[4];
        let alpha = match scheme.is_light() {
            true => 26,
            false => 38,
        };
        Color::rgba(r, g, b, alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_theme(theme: &str) -> Config {
        Config {
            theme: theme.into(),
            ..Config::default()
        }
    }

    fn luminance(color: Color) -> f32 {
        let [r, g, b, _] = color.to_rgba();
        palette::luminance([r, g, b])
    }

    #[test]
    fn the_default_config_uses_kitty() {
        let config = Config::default();
        assert_eq!(config.theme, "kitty");
        assert!(config.colors.is_empty());
        assert_eq!(config.palette(), palette::KITTY);
        let theme = config.theme(false);
        let [r, g, b] = palette::KITTY.background;
        assert_eq!(theme.background, Color::rgba(r, g, b, 255));
        assert_eq!(theme.foreground, color(palette::KITTY.foreground));
        assert_eq!(theme.accent, color(palette::KITTY.ansi[4]));
        // The tab bar must separate from a pure black background.
        assert_ne!(config.bar_color(false), theme.background);
    }

    #[test]
    fn legacy_theme_names_keep_the_old_look() {
        assert_eq!(with_theme("dark").palette(), palette::AXO_DARK);
        assert_eq!(with_theme("light").palette(), palette::AXO_LIGHT);
        assert_eq!(
            with_theme("Solarized_Light").palette(),
            palette::SOLARIZED_LIGHT
        );
    }

    #[test]
    fn window_theme_follows_the_scheme_brightness() {
        let dark = Config::default().theme(false);
        assert!(luminance(dark.surface) > luminance(dark.background));
        assert!(luminance(dark.surface) < luminance(dark.foreground));
        let light = with_theme("solarized-light").theme(false);
        assert!(luminance(light.surface) < luminance(light.background));
        assert!(luminance(light.foreground) < luminance(light.surface));
    }

    #[test]
    fn opacity_applies_to_the_window_background_only() {
        let config = Config {
            opacity: 0.5,
            ..Config::default()
        };
        assert_eq!(config.theme(true).background.to_rgba()[3], 128);
        assert_eq!(config.bar_color(true).to_rgba()[3], 128);
        assert_eq!(config.theme(false).background.to_rgba()[3], 255);
    }
}
