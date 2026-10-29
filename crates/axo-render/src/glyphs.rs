//! Terminal glyphs through the aegle text stack: system fonts with fallback
//! (CJK, symbols, emoji) shaped by `aegle-text`, rasterized by `aegle-glyph`.
//!
//! Each distinct character and style is shaped and rasterized once; renderers
//! keep the result in their own atlas, so this runs only on cache misses.

use aegle::ui::scene::{Color, SceneBuilder};
use aegle_glyph::{CacheLimits, Content, GlyphCache, RasterOptions};
use aegle_text::{LineHeight, Slant, TextStyle, TextSystem};

use crate::{CellSize, FontSpec, Result};

/// Identity of a cached terminal glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub ch: char,
    pub bold: bool,
    pub italic: bool,
}

/// A rasterized glyph, positioned against the baseline origin.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Offset of the left edge from the cell's left edge.
    pub left: i32,
    /// Distance of the top edge above the baseline.
    pub top: i32,
    /// Color glyphs carry straight sRGB; masks carry coverage in alpha.
    pub color: bool,
    /// Tightly packed RGBA8.
    pub rgba: Vec<u8>,
}

pub struct Glyphs {
    text: TextSystem,
    cache: GlyphCache,
    family: String,
    /// Font size in device pixels.
    size: f32,
    cell: CellSize,
}

impl Glyphs {
    /// Discovers system fonts. The glyph cache holds little: renderers cache
    /// the uploaded bitmaps themselves.
    pub fn new() -> Self {
        Self {
            text: TextSystem::system(),
            cache: GlyphCache::with_limits(CacheLimits {
                image_bytes: 1 << 20,
                ..CacheLimits::default()
            }),
            family: "monospace".into(),
            size: 14.0,
            cell: CellSize::default(),
        }
    }

    pub fn cell(&self) -> CellSize {
        self.cell
    }
}

fn style(family: &str, size: f32, key: GlyphKey) -> TextStyle<'_> {
    TextStyle {
        families: family,
        size,
        weight: if key.bold { 700.0 } else { 400.0 },
        slant: if key.italic {
            Slant::Italic
        } else {
            Slant::Normal
        },
        color: Color::WHITE,
        line_height: LineHeight::MetricsRelative(1.0),
        ..TextStyle::default()
    }
}

impl Glyphs {
    /// Selects the font and measures the cell from the regular face's advance
    /// and line metrics.
    pub fn set_font(&mut self, font: &FontSpec, scale: f32) -> Result<CellSize> {
        if !(font.size.is_finite() && font.size > 0.0 && scale > 0.0) {
            return Err("font size and scale must be positive".into());
        }
        self.family = format!("{}, monospace", font.family.trim().trim_end_matches(','));
        self.size = font.size * scale;
        self.cache.clear();
        let regular = GlyphKey {
            ch: 'M',
            bold: false,
            italic: false,
        };
        let sample = "M".repeat(16);
        let paragraph = self
            .text
            .paragraph(sample, &style(&self.family, self.size, regular))?;
        let size = paragraph.size();
        let baseline = paragraph.first_baseline().unwrap_or(size.height * 0.8);
        let height = (size.height * font.line_height.max(0.5)).round().max(1.0);
        self.cell = CellSize {
            width: (size.width / 16.0).round().max(1.0) as u32,
            height: height as u32,
            baseline: (baseline + (height - size.height) / 2.0).round() as i32,
        };
        Ok(self.cell)
    }

    /// Shapes `key.ch` with fallback and composites its glyphs into one bitmap;
    /// `None` for blank glyphs or characters no font covers.
    pub fn rasterize(&mut self, key: GlyphKey) -> Option<Bitmap> {
        if let Some(bitmap) = crate::boxdraw::draw(key.ch, self.cell) {
            return Some(bitmap);
        }
        let mut text = [0u8; 4];
        let paragraph = self
            .text
            .paragraph(
                key.ch.encode_utf8(&mut text).to_owned(),
                &style(&self.family, self.size, key),
            )
            .ok()?;
        if paragraph.missing_glyphs() > 0 {
            return None;
        }
        let baseline = paragraph.first_baseline()?;
        let mut builder = SceneBuilder::new();
        paragraph.paint(&mut builder);
        let scene = builder.finish();

        // Placed glyph images in paragraph pixels: (x, y, width, height, content, data).
        let mut placed = Vec::new();
        for run in scene.glyph_runs() {
            for glyph in run.glyphs() {
                let options = RasterOptions {
                    size: run.size(),
                    normalized_coords: run.normalized_coords(),
                    foreground: [255; 4],
                    embolden: run.embolden(),
                    skew: run.skew(),
                    ..RasterOptions::default()
                };
                let Ok(image) = self.cache.rasterize(run.font(), glyph.id, options) else {
                    continue;
                };
                let p = image.placement;
                if p.width == 0 || p.height == 0 {
                    continue;
                }
                let x = glyph.position.x.round() as i32 + p.left;
                let y = glyph.position.y.round() as i32 - p.top;
                placed.push((x, y, p.width, p.height, image.content, image.data.to_vec()));
            }
        }
        if placed.is_empty() {
            return None;
        }
        let x0 = placed.iter().map(|g| g.0).min().unwrap();
        let y0 = placed.iter().map(|g| g.1).min().unwrap();
        let x1 = placed.iter().map(|g| g.0 + g.2 as i32).max().unwrap();
        let y1 = placed.iter().map(|g| g.1 + g.3 as i32).max().unwrap();
        let (width, height) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let color = placed.iter().any(|g| g.4 == Content::Color);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for (x, y, w, h, content, data) in placed {
            for row in 0..h as usize {
                for col in 0..w as usize {
                    let dst =
                        (((y - y0) as usize + row) * width as usize + (x - x0) as usize + col) * 4;
                    let src = row * w as usize + col;
                    let pixel = match content {
                        Content::Mask => [255, 255, 255, data[src]],
                        Content::Color => [
                            data[src * 4],
                            data[src * 4 + 1],
                            data[src * 4 + 2],
                            data[src * 4 + 3],
                        ],
                    };
                    // Overlapping glyphs (marks over a base) keep the strongest coverage.
                    if pixel[3] >= rgba[dst + 3] {
                        rgba[dst..dst + 4].copy_from_slice(&pixel);
                    }
                }
            }
        }
        Some(Bitmap {
            width,
            height,
            left: x0,
            top: baseline.round() as i32 - y0,
            color,
            rgba,
        })
    }
}

impl Default for Glyphs {
    fn default() -> Self {
        Self::new()
    }
}
