//! The tab bar's skins: flat, quiet controls that only tint under the pointer.
//! They take their colors from the window theme, which follows the terminal
//! palette, so the bar blends into the terminal instead of reading as a row of
//! widgets.

use std::sync::LazyLock;

use aegle::{
    prelude::{Appearance, Color, Theme, VisualState},
    ui::{
        Decorator,
        control::PaintCx,
        scene::{Affine, FillRule, Path, PathBuilder, Point},
    },
};

/// The pill radius shared by tabs and bar buttons.
const RADIUS: f32 = 6.0;

/// Text away from the pointer: the palette's muted color.
fn quiet_text(theme: &Theme) -> Color {
    theme.muted.with_alpha(255)
}

/// The layer a pressed or hovered chrome control adds under its contents.
fn layer(theme: &Theme, state: VisualState) -> Color {
    match state.enabled {
        true if state.pressed => theme.pressed,
        true if state.hovered => theme.hover,
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

/// A standalone bar button: quiet icon, a faint wash under the pointer.
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

/// An inactive, unhovered tab's label: quiet text, transparent background.
pub fn tab_quiet(theme: &Theme, state: VisualState) -> Appearance {
    Appearance {
        background: Color::TRANSPARENT,
        foreground: quiet_text(theme),
        radius: RADIUS,
        focus_color: theme.foreground.with_alpha(140),
        ..Appearance::base(theme, state)
    }
}

/// An inactive, hovered tab's label: highlighted text, transparent background.
pub fn tab_hovered(theme: &Theme, state: VisualState) -> Appearance {
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

static PLUS_PATH: LazyLock<Path> = LazyLock::new(|| {
    let mut b = PathBuilder::new();
    let r = 4.5;
    let t = 0.75;
    b.move_to(Point::new(-t, -r));
    b.line_to(Point::new(t, -r));
    b.line_to(Point::new(t, -t));
    b.line_to(Point::new(r, -t));
    b.line_to(Point::new(r, t));
    b.line_to(Point::new(t, t));
    b.line_to(Point::new(t, r));
    b.line_to(Point::new(-t, r));
    b.line_to(Point::new(-t, t));
    b.line_to(Point::new(-r, t));
    b.line_to(Point::new(-r, -t));
    b.line_to(Point::new(-t, -t));
    b.close();
    b.finish(FillRule::NonZero)
});

static GEAR_PATH: LazyLock<Path> = LazyLock::new(|| {
    let mut b = PathBuilder::new();
    let r_out = 5.8;
    let r_in = 4.2;
    let r_hole = 1.9;

    let polar = |r: f32, deg: f32| {
        let rad = deg.to_radians();
        Point::new(r * rad.cos(), r * rad.sin())
    };

    // 6-tooth gear perimeter.
    b.move_to(polar(r_out, -10.0));
    for i in 0..6 {
        let base = i as f32 * 60.0;
        b.line_to(polar(r_out, base + 10.0));
        b.line_to(polar(r_in, base + 18.0));
        b.line_to(polar(r_in, base + 30.0));
        b.line_to(polar(r_in, base + 42.0));
        b.line_to(polar(r_out, base + 50.0));
    }
    b.close();

    // Center circular hole via 4 cubic Beziers.
    let k = r_hole * 0.552_284_8;
    b.move_to(Point::new(0.0, -r_hole));
    b.cubic_to(
        Point::new(k, -r_hole),
        Point::new(r_hole, -k),
        Point::new(r_hole, 0.0),
    );
    b.cubic_to(
        Point::new(r_hole, k),
        Point::new(k, r_hole),
        Point::new(0.0, r_hole),
    );
    b.cubic_to(
        Point::new(-k, r_hole),
        Point::new(-r_hole, k),
        Point::new(-r_hole, 0.0),
    );
    b.cubic_to(
        Point::new(-r_hole, -k),
        Point::new(-k, -r_hole),
        Point::new(0.0, -r_hole),
    );
    b.close();

    b.finish(FillRule::EvenOdd)
});

/// Cross-platform vector "+" icon for the new tab button.
pub struct PlusIcon;

impl Decorator for PlusIcon {
    fn over(&mut self, cx: &mut PaintCx<'_>) {
        let x = (cx.size.width * 0.5).round();
        let y = (cx.size.height * 0.5).round();
        cx.builder.push_transform(Affine::translation(x, y));
        cx.builder.fill_path(&PLUS_PATH, cx.appearance.foreground);
        cx.builder.pop();
    }
}

/// Cross-platform vector gear icon for the settings button.
pub struct GearIcon;

impl Decorator for GearIcon {
    fn over(&mut self, cx: &mut PaintCx<'_>) {
        let x = (cx.size.width * 0.5).round();
        let y = (cx.size.height * 0.5).round();
        cx.builder.push_transform(Affine::translation(x, y));
        cx.builder.fill_path(&GEAR_PATH, cx.appearance.foreground);
        cx.builder.pop();
    }
}
