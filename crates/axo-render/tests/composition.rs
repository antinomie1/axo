//! Uncommitted composition hides the cells it covers, is underlined, and moves
//! the caret into itself, so the candidate window anchors where the text is.

use std::{cell::RefCell, rc::Rc};

use axo_render::{
    CellSize, Frame, Preedit,
    glyphs::GlyphKey,
    palette::KITTY,
    quads::{self, AtlasGlyph, GlyphSource, MASK, SOLID},
};
use axo_term::{CursorShape, Term};

/// Records the characters asked for, so a test can see which cells drew.
struct Probe(Rc<RefCell<Vec<char>>>);

impl GlyphSource for Probe {
    fn glyph(&mut self, key: GlyphKey) -> Option<AtlasGlyph> {
        self.0.borrow_mut().push(key.ch);
        Some(AtlasGlyph {
            uv: [0, 0, 8, 16],
            left: 0,
            top: 0,
            color: false,
        })
    }
}

fn build(term: &Term, preedit: Option<Preedit<'_>>) -> (quads::Quads, Vec<char>) {
    let chars = Rc::new(RefCell::new(Vec::new()));
    let palette = KITTY;
    let frame = Frame {
        term,
        palette: &palette,
        cursor: CursorShape::Block,
        focused: true,
        padding: 0,
        preedit,
    };
    let mut out = quads::Quads::default();
    quads::build(
        &frame,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        &mut Probe(chars.clone()),
        &mut out,
    );
    let chars = chars.borrow().clone();
    (out, chars)
}

#[test]
fn composition_hides_the_cells_it_covers() {
    let mut term = Term::new(6, 1, 0);
    term.advance(b"abcdef\x1b[1;1H");
    let (out, chars) = build(
        &term,
        Some(Preedit {
            text: "に",
            cursor: None,
        }),
    );
    // The wide character takes two columns, so `a` and `b` are not drawn.
    assert_eq!(chars, vec!['に', 'c', 'd', 'e', 'f']);
    // It is underlined across both of its columns.
    let underlines: Vec<[f32; 4]> = out
        .foreground
        .iter()
        .filter(|quad| quad.kind == SOLID)
        .map(|quad| quad.rect)
        .collect();
    assert_eq!(underlines.len(), 1);
    assert_eq!(underlines[0][0], 0.0);
    assert_eq!(underlines[0][2], 16.0, "two cells wide");
    // Composition text keeps the cell's foreground color.
    let composed = out
        .foreground
        .iter()
        .find(|quad| quad.kind == MASK)
        .expect("the composed glyph");
    let [r, g, b] = KITTY.foreground;
    assert_eq!(composed.color, [r, g, b, 255]);
}

#[test]
fn the_caret_sits_at_the_composition_caret() {
    let mut term = Term::new(6, 1, 0);
    term.advance(b"abcdef\x1b[1;1H");
    // Two wide characters, caret after the first one: column 2.
    let (out, _) = build(
        &term,
        Some(Preedit {
            text: "にほ",
            cursor: Some((0, 3)),
        }),
    );
    let cursor: Vec<[f32; 4]> = out
        .background
        .iter()
        .filter(|quad| quad.color == [KITTY.cursor[0], KITTY.cursor[1], KITTY.cursor[2], 255])
        .map(|quad| quad.rect)
        .collect();
    assert_eq!(cursor.len(), 1);
    assert_eq!(cursor[0][0], 16.0, "the second column of a 8px cell");
    assert_eq!(cursor[0][2], 16.0, "a block cursor covers a wide character");
}

#[test]
fn a_caret_at_the_end_follows_the_last_character() {
    let mut term = Term::new(8, 1, 0);
    term.advance(b"abcdefgh\x1b[1;1H");
    let (out, _) = build(
        &term,
        Some(Preedit {
            text: "あい",
            cursor: Some((0, 6)),
        }),
    );
    let cursor: Vec<[f32; 4]> = out
        .background
        .iter()
        .filter(|quad| quad.color == [KITTY.cursor[0], KITTY.cursor[1], KITTY.cursor[2], 255])
        .map(|quad| quad.rect)
        .collect();
    assert_eq!(cursor.len(), 1);
    assert_eq!(cursor[0][0], 32.0, "two wide characters, four columns");
}
