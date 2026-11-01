//! Uncommitted composition hides the cells it covers, is underlined, and moves
//! the caret into itself, so the candidate window anchors where the text is.
//! Images are laid out into layers around the text, following their lines.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use axo_render::{
    CellSize, Frame, Preedit,
    glyphs::GlyphKey,
    palette::KITTY,
    quads::{self, AtlasGlyph, GlyphSource, MASK, SOLID},
};
use axo_term::{CursorShape, Pixels, Term};

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

    fn image(&mut self, pixels: &Arc<Pixels>) -> Option<u64> {
        Some(pixels.width.into())
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

/// Places a transparent `size`-pixels-square kitty image at the cursor.
fn image(term: &mut Term, size: usize, z: i32) {
    let group = "AAAA".repeat(size * size * 4 / 3 + 1);
    term.advance(format!("\x1b_Ga=T,f=32,s={size},v={size},z={z},C=1;{group}\x1b\\").as_bytes());
}

#[test]
fn images_sort_into_layers_around_the_text() {
    let mut term = Term::new(6, 4, 0);
    term.set_cell_size(8, 16);
    image(&mut term, 1, 5);
    image(&mut term, 2, -1);
    image(&mut term, 3, i32::MIN);
    image(&mut term, 4, 0);
    term.advance(b"text");
    let (out, _) = build(&term, None);
    let [below, under, over] = out.layers();
    assert_eq!(below.len(), 1);
    assert_eq!(below[0].texture, 3);
    assert_eq!(under[0].texture, 2);
    assert_eq!(
        over.iter().map(|image| image.texture).collect::<Vec<_>>(),
        [4, 1]
    );
    // Instances are written in drawing order: backgrounds after the lowest
    // layer, text after the middle one.
    let order: Vec<u32> = out.iter().map(|quad| quad.kind).collect();
    assert_eq!(order.len(), out.len());
    assert_eq!(*order.last().unwrap(), SOLID, "images end the list");
    assert!(order[..order.len() - 2].contains(&MASK));
}

#[test]
fn images_follow_the_view_and_are_culled_outside_it() {
    let mut term = Term::new(6, 2, 10);
    term.set_cell_size(8, 16);
    term.advance(b"\x1b[2;2H");
    image(&mut term, 4, 0);
    let rect = |term: &Term| {
        let (out, _) = build(term, None);
        out.images.first().map(|image| image.quad.rect)
    };
    assert_eq!(rect(&term), Some([8.0, 16.0, 4.0, 4.0]));
    // Two lines up, it has left the screen...
    term.advance(b"\n\n");
    assert_eq!(rect(&term), None);
    // ...and scrolling back brings it to where those lines are shown.
    term.scroll_display(1);
    assert_eq!(rect(&term), Some([8.0, 0.0, 4.0, 4.0]));
}
