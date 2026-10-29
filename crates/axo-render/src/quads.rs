//! Backend-neutral layout of a terminal frame into colored and textured quads.

use axo_term::{CursorShape, Flags};

use crate::{CellSize, Frame, Rgb, glyphs::GlyphKey};

pub const SOLID: u32 = 0;
pub const MASK: u32 = 1;
pub const COLOR: u32 = 2;

/// One instanced quad, the GPU vertex layout: 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Quad {
    /// x, y, width, height in device pixels.
    pub rect: [f32; 4],
    /// Atlas texel rectangle x, y, width, height for glyph kinds.
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

/// A renderer's glyph cache, rasterizing on a miss.
pub trait GlyphSource {
    fn glyph(&mut self, key: GlyphKey) -> Option<AtlasGlyph>;
}

/// Reused quad lists: backgrounds draw first, then glyphs and decorations.
#[derive(Default)]
pub struct Quads {
    pub background: Vec<Quad>,
    pub foreground: Vec<Quad>,
}

impl Quads {
    pub fn len(&self) -> usize {
        self.background.len() + self.foreground.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = &Quad> {
        self.background.iter().chain(&self.foreground)
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

/// Lays out `frame`; the default background stays transparent so the window
/// background (and its opacity) shows through.
pub fn build(frame: &Frame<'_>, cell: CellSize, glyphs: &mut dyn GlyphSource, out: &mut Quads) {
    out.background.clear();
    out.foreground.clear();
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
    let block = frame.focused && shape == CursorShape::Block;
    let line_width = (ch / 16.0).round().max(1.0);

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
            let c = row.cells.get(x).copied().unwrap_or_default();
            let mut fg = palette.resolve(c.fg, true).unwrap();
            let mut bg = palette.resolve(c.bg, false);
            if c.flags.has(Flags::INVERSE) {
                (fg, bg) = (bg.unwrap_or(palette.background), Some(fg));
            }
            if selection.is_some_and(|range| range.contains(line, x)) {
                bg = Some(palette.selection);
            }
            if c.flags.has(Flags::DIM) {
                fg = fg.map(|v| (v as u16 * 2 / 3) as u8);
            }
            if block && cursor == Some((x, y)) {
                bg = Some(palette.cursor);
                fg = palette.background;
            }
            if run.map(|r| r.1) != bg {
                flush(&mut run, x, out);
                run = bg.map(|rgb| (x, rgb));
            }
            let left = pad + x as f32 * cw;
            if c.ch != ' ' && !c.flags.has(Flags::SPACER) && !c.flags.has(Flags::HIDDEN) {
                let key = GlyphKey {
                    ch: c.ch,
                    bold: c.flags.has(Flags::BOLD),
                    italic: c.flags.has(Flags::ITALIC),
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
            let span = if c.flags.has(Flags::WIDE) { 2.0 } else { 1.0 };
            if c.flags.has(Flags::UNDERLINE) {
                let y = top + (cell.baseline as f32 + line_width).min(ch - line_width);
                out.foreground
                    .push(solid(left, y, cw * span, line_width, fg));
            }
            if c.flags.has(Flags::STRIKE) {
                let y = top + (ch / 2.0).round();
                out.foreground
                    .push(solid(left, y, cw * span, line_width, fg));
            }
        }
        flush(&mut run, cols, out);
    }

    // Non-block cursors draw over the glyphs.
    if let Some((x, y)) = cursor.filter(|_| !block) {
        let wide = grid
            .line(term.view_line(y))
            .and_then(|row| row.cells.get(x))
            .is_some_and(|c| c.flags.has(Flags::WIDE));
        let (left, top) = (pad + x as f32 * cw, pad + y as f32 * ch);
        let w = if wide { cw * 2.0 } else { cw };
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
