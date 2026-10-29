//! The tab bar's skins: flat, quiet controls that only tint under the pointer.
//! They take their colors from the window theme, which follows the terminal
//! palette, so the bar blends into the terminal instead of reading as a row of
//! widgets.

use aegle::prelude::{Appearance, Color, Theme, VisualState};

/// The pill radius shared by tabs and bar buttons.
const RADIUS: f32 = 6.0;

/// Text away from the pointer: the palette foreground quieted toward the
/// window background.
fn quiet_text(theme: &Theme) -> Color {
    let base = theme.background.with_alpha(255);
    let text = theme.foreground.with_alpha(255);
    base.mix(text, 0.62)
}

/// The layer a pressed or hovered chrome control adds under its contents.
fn layer(theme: &Theme, state: VisualState) -> Color {
    match state.enabled {
        true if state.pressed => theme.foreground.with_alpha(24),
        true if state.hovered => theme.foreground.with_alpha(10),
        _ => Color::TRANSPARENT,
    }
}

fn skin(theme: &Theme, state: VisualState, rest: Color, radius: f32) -> Appearance {
    Appearance {
        background: layer(theme, state),
        foreground: match state.enabled && (state.hovered || state.pressed) {
            true => theme.foreground,
            false => rest,
        },
        radius,
        focus_color: theme.foreground.with_alpha(140),
        ..Appearance::base(theme, state)
    }
}

/// An inactive tab or a bar button: quiet text, a faint wash under the pointer.
pub fn quiet(theme: &Theme, state: VisualState) -> Appearance {
    skin(theme, state, quiet_text(theme), RADIUS)
}

/// The active tab's label: full contrast, since the tab's own fill marks it.
pub fn active(theme: &Theme, state: VisualState) -> Appearance {
    Appearance {
        background: Color::TRANSPARENT,
        foreground: theme.foreground,
        radius: RADIUS,
        focus_color: theme.foreground.with_alpha(140),
        ..Appearance::base(theme, state)
    }
}

/// A tab's close glyph, quieter than its label until pointed at.
pub fn close(theme: &Theme, state: VisualState) -> Appearance {
    skin(theme, state, quiet_text(theme).with_alpha(150), 4.0)
}

/// A terminal canvas: no frame and no focus outline; the text cursor is what
/// shows where typing goes, so panes never gain a border.
pub fn canvas(theme: &Theme, state: VisualState) -> Appearance {
    Appearance {
        focus_width: 0.0,
        ..Appearance::base(theme, state)
    }
}
