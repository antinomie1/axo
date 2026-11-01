//! Backend-neutral layout of a terminal frame into colored and textured quads.

use std::sync::Arc;

use axo_term::{CursorShape, Flags, Pixels};
use unicode_width::UnicodeWidthChar;

use crate::{CellSize, Frame, Preedit, Rgb, glyphs::GlyphKey};

pub const SOLID: u32 = 0;
pub const MASK: u32 = 1;
pub const COLOR: u32 = 2;

/// Images below this z draw under cell backgrounds; the rest of the negative
/// range draws over backgrounds and under text. Both are kitty's layers.
pub const BELOW_BACKGROUND: i32 = i32::MIN / 2;

/// One instanced quad, the GPU vertex layout: 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Quad {
    /// x, y, width, height in device pixels.
    pub rect: [f32; 4],
    /// Texel rectangle x, y, width, height for glyphs and images.
    pub uv: [u16; 4],
    /// Straight sRGB RGBA.
    pub color: [u8; 4],
    /// [`SOLID`], [`MASK`] or [`COLOR`].
    pub kind: u32,
}

/// Where a glyph sits in a renderer's atlas.
#[derive(Clone, Copy, Debug)]
pub struct AtlasGlyph {
    pub uv: [u16; 4],
    pub left: i16,
    pub top: i16,
    pub color: bool,
}

/// A renderer's glyph and image caches, filling on a miss.
pub trait GlyphSource {
    fn glyph(&mut self, key: GlyphKey) -> Option<AtlasGlyph>;
    /// A handle the renderer draws `pixels` with, in whatever form it keeps
    /// them; `None` skips the image.
    fn image(&mut self, _pixels: &Arc<Pixels>) -> Option<u64> {
        None
    }
}

/// An image to draw: one textured quad.
#[derive(Clone, Copy, Debug)]
pub struct ImageQuad {
    pub quad: Quad,
    /// The handle from [`GlyphSource::image`].
    pub texture: u64,
    pub z: i32,
}

/// Reused quad lists. Backgrounds draw first, then glyphs and decorations, with
/// images in three layers around them.
#[derive(Default)]
pub struct Quads {
    pub background: Vec<Quad>,
    pub foreground: Vec<Quad>,
    /// Sorted by z; older images first within one z.
    pub images: Vec<ImageQuad>,
}

impl Quads {
    pub fn len(&self) -> usize {
        self.background.len() + self.foreground.len() + self.images.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The images under backgrounds, between backgrounds and text, and over text.
    pub fn layers(&self) -> [&[ImageQuad]; 3] {
        let (under, over) = self
            .images
            .split_at(self.images.partition_point(|image| image.z < 0));
        let (below, under) =
            under.split_at(under.partition_point(|image| image.z < BELOW_BACKGROUND));
        [below, under, over]
    }
    /// Every quad in drawing order, which is the order instances are written.
    pub fn iter(&self) -> impl Iterator<Item = &Quad> {
        fn quads(images: &[ImageQuad]) -> impl Iterator<Item = &Quad> {
            images.iter().map(|image| &image.quad)
        }
        let [below, under, over] = self.layers();
        quads(below)
            .chain(&self.background)
            .chain(quads(under))
            .chain(&self.foreground)
            .chain(quads(over))
    }
}

fn solid(x: f32, y: f32, w: f32, h: f32, rgb: Rgb) -> Quad {
    Quad {
        rect: [x, y, w, h],
        color: [rgb[0], rgb[1], rgb[2], 255],
        kind: SOLID,
        ..Quad::default()
    }
}

/// Composition text laid over the cursor's row, one entry per column.
struct Overlay {
    /// The column the text starts at.
    start: usize,
    /// The character drawn in a column, or `None` for the trailing column of a
    /// double-width one.
    columns: Vec<Option<char>>,
    /// The column of the composition caret, relative to the row.
    caret: usize,
}

impl Overlay {
    fn new(preedit: &Preedit<'_>, start: usize, cols: usize) -> Self {
        let mut columns = Vec::new();
        let mut caret = None;
        for (offset, ch) in preedit.text.char_indices() {
            if Some(offset) == preedit.cursor.map(|(_, focus)| focus) {
                caret = Some(columns.len());
            }
            columns.push(Some(ch));
            for _ in 1..width(ch) {
                columns.push(None);
            }
        }
        // A caret past the last character follows it, as an input method's does.
        let caret = caret.unwrap_or(columns.len());
        columns.truncate(cols.saturating_sub(start));
        Self {
            start,
            caret,
            columns,
        }
    }

    /// The entry for column `x`, when the composition covers it.
    fn at(&self, x: usize) -> Option<Option<char>> {
        let index = x.checked_sub(self.start)?;
        self.columns.get(index).copied()
    }

    /// Cells the caret character covers, so a block cursor covers both halves.
    fn caret_span(&self) -> usize {
        match self.columns.get(self.caret) {
            Some(Some(ch)) => width(*ch),
            _ => 1,
        }
    }
}

/// Columns a character occupies in the grid.
fn width(ch: char) -> usize {
    ch.width().unwrap_or(1).clamp(1, 2)
}

/// Lays out the visible images, in pixels and pixel-aligned so that an image at
/// its natural size maps texels to pixels one to one.
fn images(frame: &Frame<'_>, cell: CellSize, source: &mut dyn GlyphSource, out: &mut Quads) {
    let term = frame.term;
    let grid = term.grid();
    let view = grid.base() - term.display_offset() as i64;
    let pad = i64::from(frame.padding);
    let bottom = term.rows() as i64 * i64::from(cell.height);
    for placement in grid.images().iter() {
        let [width, height] = placement.size([cell.width, cell.height]);
        let y = (placement.line - view) * i64::from(cell.height) + i64::from(placement.offset[1]);
        if y + i64::from(height) <= 0 || y >= bottom {
            continue;
        }
        let x = placement.col as i64 * i64::from(cell.width) + i64::from(placement.offset[0]);
        let Some(texture) = source.image(&placement.pixels) else {
            continue;
        };
        out.images.push(ImageQuad {
            quad: Quad {
                rect: [
                    (pad + x) as f32,
                    (pad + y) as f32,
                    width as f32,
                    height as f32,
                ],
                uv: placement.crop.map(|side| side as u16),
                color: [255; 4],
                kind: SOLID,
            },
            texture,
            z: placement.z,
        });
    }
    out.images.sort_by_key(|image| image.z);
}

/// Lays out `frame`; the default background stays transparent so the window
/// background (and its opacity) shows through.
pub fn build(frame: &Frame<'_>, cell: CellSize, glyphs: &mut dyn GlyphSource, out: &mut Quads) {
    out.background.clear();
    out.foreground.clear();
    out.images.clear();
    let term = frame.term;
    let palette = frame.palette;
    let grid = term.grid();
    let (cols, rows) = (term.cols(), term.rows());
    let (cw, ch) = (cell.width as f32, cell.height as f32);
    let pad = frame.padding as f32;
    let selection = term.selection_range();
    let shape = term.cursor_shape.unwrap_or(frame.cursor);
    let cursor_row = term.cursor.y + term.display_offset();
    let cursor =
        (term.modes.show_cursor && cursor_row < rows).then_some((term.cursor.x, cursor_row));
    // Uncommitted composition hides the cells it covers and moves the caret
    // into itself, which is where the candidate window points.
    let overlay = frame
        .preedit
        .filter(|preedit| !preedit.text.is_empty())
        .map(|preedit| Overlay::new(&preedit, term.cursor.x, cols));
    let cursor = match (&overlay, cursor) {
        (Some(overlay), _) => {
            (term.modes.show_cursor && cursor_row < rows && !overlay.columns.is_empty()).then_some((
                (overlay.start + overlay.caret).min(cols.saturating_sub(1)),
                cursor_row,
            ))
        }
        (None, cursor) => cursor,
    };
    let block = frame.focused && shape == CursorShape::Block;
    let cursor_span = overlay.as_ref().map_or(1, Overlay::caret_span);
    let line_width = (ch / 16.0).round().max(1.0);
    if !grid.images().is_empty() {
        images(frame, cell, glyphs, out);
    }

    for y in 0..rows {
        let line = term.view_line(y);
        let Some(row) = grid.line(line) else { continue };
        let top = pad + y as f32 * ch;
        let mut run: Option<(usize, Rgb)> = None;
        let flush = |run: &mut Option<(usize, Rgb)>, end: usize, out: &mut Quads| {
            if let Some((start, rgb)) = run.take() {
                out.background.push(solid(
                    pad + start as f32 * cw,
                    top,
                    (end - start) as f32 * cw,
                    ch,
                    rgb,
                ));
            }
        };
        for x in 0..cols {
            // The attributes are one run lookup, the character one word.
            let style = row.style_at(x);
            let flags = style.flags;
            // A composition column keeps the cell's colors and hides its
            // character; `None` is the trailing half of a wide one.
            let composing = overlay
                .as_ref()
                .and_then(|overlay| (y == cursor_row).then(|| overlay.at(x)).flatten());
            let (text, wide) = match composing {
                Some(Some(text)) => (Some(text), width(text) == 2),
                Some(None) => (None, false),
                None => (Some(row.ch(x)), row.is_wide(x)),
            };
            let mut fg = palette.resolve(style.fg, true).unwrap();
            let mut bg = palette.resolve(style.bg, false);
            if flags.has(Flags::INVERSE) {
                (fg, bg) = (bg.unwrap_or(palette.background), Some(fg));
            }
            let selected = selection.is_some_and(|range| range.contains(line, x));
            if selected {
                bg = Some(palette.selection);
                if let Some(text) = palette.selection_text {
                    fg = text;
                }
            }
            if flags.has(Flags::DIM) && !selected {
                fg = palette.dim(fg);
            }
            let under_cursor = block
                && cursor.is_some_and(|(cx, cy)| cy == y && x >= cx && x - cx < cursor_span);
            if under_cursor {
                bg = Some(palette.cursor);
                if let Some(text) = palette.cursor_text {
                    fg = text;
                }
            }
            if run.map(|r| r.1) != bg {
                flush(&mut run, x, out);
                run = bg.map(|rgb| (x, rgb));
            }
            let left = pad + x as f32 * cw;
            let drawn = match composing {
                Some(_) => text,
                None if !row.is_spacer(x) && !flags.has(Flags::HIDDEN) => text,
                None => None,
            };
            if let Some(ch) = drawn.filter(|ch| *ch != ' ') {
                let key = GlyphKey {
                    ch,
                    bold: flags.has(Flags::BOLD),
                    italic: flags.has(Flags::ITALIC),
                };
                if let Some(glyph) = glyphs.glyph(key) {
                    out.foreground.push(Quad {
                        rect: [
                            left + glyph.left as f32,
                            top + (cell.baseline - glyph.top as i32) as f32,
                            glyph.uv[2] as f32,
                            glyph.uv[3] as f32,
                        ],
                        uv: glyph.uv,
                        color: [fg[0], fg[1], fg[2], 255],
                        kind: if glyph.color { COLOR } else { MASK },
                    });
                }
            }
            let span = if wide { 2.0 } else { 1.0 };
            // Composition is underlined until the input method commits it.
            if flags.has(Flags::UNDERLINE) || matches!(composing, Some(Some(_))) {
                let y = top + (cell.baseline as f32 + line_width).min(ch - line_width);
                out.foreground
                    .push(solid(left, y, cw * span, line_width, fg));
            }
            if flags.has(Flags::STRIKE) {
                let y = top + (ch / 2.0).round();
                out.foreground
                    .push(solid(left, y, cw * span, line_width, fg));
            }
        }
        flush(&mut run, cols, out);
    }

    // Non-block cursors draw over the glyphs.
    if let Some((x, y)) = cursor.filter(|_| !block) {
        let (left, top) = (pad + x as f32 * cw, pad + y as f32 * ch);
        let w = cw * cursor_span as f32;
        let t = (cw / 8.0).round().max(1.0);
        let color = palette.cursor;
        match (frame.focused, shape) {
            (true, CursorShape::Bar) => out.foreground.push(solid(left, top, t, ch, color)),
            (true, _) => out.foreground.push(solid(left, top + ch - t, w, t, color)),
            (false, _) => {
                out.foreground.push(solid(left, top, w, t, color));
                out.foreground.push(solid(left, top + ch - t, w, t, color));
                out.foreground.push(solid(left, top, t, ch, color));
                out.foreground.push(solid(left + w - t, top, t, ch, color));
            }
        }
    }
}
