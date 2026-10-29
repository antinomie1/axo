use axo_render::{
    CellSize, Frame, Palette,
    glyphs::GlyphKey,
    palette::{self, KITTY, SCHEMES},
    quads::{self, GlyphSource, MASK},
};
use axo_term::{CursorShape, Point, Selection, SelectionKind, Term};

#[test]
fn the_default_scheme_is_kitty() {
    assert_eq!(Palette::default(), KITTY);
    assert_eq!(
        palette::builtin("kitty").as_ref(),
        Some(&KITTY),
        "the default scheme resolves by name"
    );
    assert_eq!(KITTY.foreground, [0xdd, 0xdd, 0xdd]);
    assert_eq!(KITTY.background, [0, 0, 0]);
    assert_eq!(KITTY.cursor_text, Some([0x11, 0x11, 0x11]));
    assert_eq!(KITTY.selection, [0xff, 0xfa, 0xcd]);
    assert_eq!(KITTY.selection_text, Some([0, 0, 0]));
    assert_eq!(KITTY.ansi[1], [0xcc, 0x04, 0x03]);
    assert_eq!(KITTY.ansi[14], [0x14, 0xff, 0xff]);
}

#[test]
fn indexed_colors_use_the_cube_and_gray_ramp() {
    assert_eq!(KITTY.indexed(1), KITTY.ansi[1]);
    assert_eq!(KITTY.indexed(16), [0, 0, 0]);
    assert_eq!(KITTY.indexed(21), [0, 0, 255]);
    assert_eq!(KITTY.indexed(46), [0, 255, 0]);
    assert_eq!(KITTY.indexed(196), [255, 0, 0]);
    assert_eq!(KITTY.indexed(231), [255, 255, 255]);
    assert_eq!(KITTY.indexed(232), [8, 8, 8]);
    assert_eq!(KITTY.indexed(255), [238, 238, 238]);
}

#[test]
fn schemes_are_valid_and_distinct() {
    let mut names = Vec::new();
    for (name, scheme) in SCHEMES {
        assert!(!names.contains(name), "duplicate scheme name `{name}`");
        names.push(*name);
        assert!(
            palette::contrast(scheme.foreground, scheme.background) >= 3.0,
            "`{name}` has too little text contrast"
        );
    }
    assert_eq!(names[0], "kitty", "kitty leads the settings list");
}

#[test]
fn names_are_case_insensitive_with_legacy_aliases() {
    assert_eq!(palette::builtin("KITTY").as_ref(), Some(&KITTY));
    assert_eq!(palette::builtin(" Default ").as_ref(), Some(&KITTY));
    assert_eq!(palette::builtin("").as_ref(), Some(&KITTY));
    assert_eq!(palette::builtin("dark"), Some(palette::AXO_DARK));
    assert_eq!(
        palette::builtin("Solarized_Light"),
        Some(palette::SOLARIZED_LIGHT)
    );
    assert!(palette::builtin("no-such-scheme").is_none());
}

#[test]
fn scheme_files_use_kitty_syntax() {
    let text = "\
# a kitty theme
foreground #eeeeee
background = #101010
cursor_text_color #000000
selection_background #444444
selection_foreground #ffffff
color1 #ff0000
color9 ff5555
font_size 12
url_color #123456
";
    let scheme = Palette::parse(text).unwrap();
    assert_eq!(scheme.foreground, [0xee, 0xee, 0xee]);
    assert_eq!(scheme.background, [0x10, 0x10, 0x10]);
    assert_eq!(scheme.cursor_text, Some([0, 0, 0]));
    assert_eq!(scheme.selection, [0x44, 0x44, 0x44]);
    assert_eq!(scheme.selection_text, Some([0xff, 0xff, 0xff]));
    assert_eq!(scheme.ansi[1], [0xff, 0, 0]);
    assert_eq!(scheme.ansi[9], [0xff, 0x55, 0x55]);
    // Keys the file leaves out keep the default.
    assert_eq!(scheme.ansi[0], KITTY.ansi[0]);
    assert_eq!(scheme.cursor, KITTY.cursor);

    assert!(Palette::parse("color0 #zzzzzz").is_err());
    assert!(Palette::parse("foreground {0,0,0}").is_err());
    // `none` only applies to the two text colors on top of cursor and selection.
    assert!(Palette::parse("background none").is_err());
    let mut scheme = Palette::default();
    scheme
        .apply("cursor_text_color none\nselection_foreground none")
        .unwrap();
    assert_eq!(scheme.cursor_text, None);
    assert_eq!(scheme.selection_text, None);
}

#[test]
fn colors_parse_with_or_without_a_hash() {
    assert_eq!(palette::parse_color("#a1b2c3"), Some([0xa1, 0xb2, 0xc3]));
    assert_eq!(palette::parse_color("a1b2c3"), Some([0xa1, 0xb2, 0xc3]));
    assert_eq!(palette::parse_color("0xA1B2C3"), Some([0xa1, 0xb2, 0xc3]));
    assert_eq!(palette::parse_color("#abc"), Some([0xaa, 0xbb, 0xcc]));
    assert_eq!(palette::parse_color("#a1b2c3ff"), Some([0xa1, 0xb2, 0xc3]));
    assert_eq!(palette::parse_color("#ab"), None);
    assert_eq!(palette::parse_color("red"), None);
}

#[test]
fn overrides_set_single_colors() {
    let mut scheme = KITTY;
    assert!(scheme.set_color("color4", [1, 2, 3]));
    assert!(scheme.set_color("selection", [4, 5, 6]));
    assert!(scheme.set_color("background-color", [7, 8, 9]));
    assert!(!scheme.set_color("color16", [0, 0, 0]));
    assert!(!scheme.set_color("font_size", [0, 0, 0]));
    assert_eq!(scheme.ansi[4], [1, 2, 3]);
    assert_eq!(scheme.selection, [4, 5, 6]);
    assert_eq!(scheme.background, [7, 8, 9]);
    assert!(palette::is_color_key("cursor_text_color"));
    assert!(!palette::is_color_key("tab_bar"));
}

#[test]
fn light_schemes_are_detected() {
    assert!(!KITTY.is_light());
    assert!(palette::SOLARIZED_LIGHT.is_light());
    assert!(palette::GRUVBOX_LIGHT.is_light());
    assert!(palette::contrast([0, 0, 0], [0xff, 0xff, 0xff]) > 20.0);
}

#[test]
fn dimming_pulls_text_toward_the_background() {
    let fg = KITTY.foreground;
    let dim = KITTY.dim(fg);
    assert_ne!(dim, fg);
    assert!(palette::luminance(dim) > palette::luminance(KITTY.background));
    assert!(palette::luminance(dim) < palette::luminance(fg));
}

struct ProbeGlyphs;

impl GlyphSource for ProbeGlyphs {
    fn glyph(&mut self, _key: GlyphKey) -> Option<quads::AtlasGlyph> {
        Some(quads::AtlasGlyph {
            uv: [0, 0, 8, 16],
            left: 0,
            top: 0,
            color: false,
        })
    }
}

fn glyph_colors(frame: &Frame<'_>) -> Vec<[u8; 4]> {
    let mut out = quads::Quads::default();
    quads::build(
        frame,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        &mut ProbeGlyphs,
        &mut out,
    );
    out.foreground
        .iter()
        .filter(|quad| quad.kind == MASK)
        .map(|quad| quad.color)
        .collect()
}

#[test]
fn the_block_cursor_uses_its_own_text_color() {
    let mut term = Term::new(2, 1, 0);
    term.advance(b"a\x1b[D");
    let palette = KITTY;
    let frame = Frame {
        term: &term,
        palette: &palette,
        cursor: CursorShape::Block,
        focused: true,
        padding: 0,
    };
    let colors = glyph_colors(&frame);
    assert_eq!(colors, vec![[0x11, 0x11, 0x11, 255]]);
}

#[test]
fn selection_text_color_overrides_the_cell() {
    let mut term = Term::new(4, 1, 0);
    term.advance(b"ab\x1b[1;1H");
    term.start_selection(Selection::new(
        Point { line: 0, col: 0 },
        SelectionKind::Line,
    ));
    let palette = KITTY;
    let frame = Frame {
        term: &term,
        palette: &palette,
        cursor: CursorShape::Bar,
        focused: false,
        padding: 0,
    };
    let mut out = quads::Quads::default();
    quads::build(
        &frame,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        &mut ProbeGlyphs,
        &mut out,
    );
    let text: Vec<[u8; 4]> = out
        .foreground
        .iter()
        .filter(|quad| quad.kind == MASK)
        .map(|quad| quad.color)
        .collect();
    assert_eq!(
        text,
        vec![[0, 0, 0, 255]; 2],
        "kitty selects black on cream"
    );
    let fills: Vec<[u8; 4]> = out.background.iter().map(|quad| quad.color).collect();
    assert!(
        fills.contains(&[0xff, 0xfa, 0xcd, 255]),
        "selection fill {fills:?}"
    );
}
