//! Box-drawing (U+2500–U+257F) and block (U+2580–U+259F) characters drawn
//! to the cell, so lines join seamlessly whatever the font provides.

use crate::{CellSize, glyphs::Bitmap};

/// Arms of each box-drawing character: left, right, up and down in 2-bit
/// fields (0 none, 1 light, 2 heavy, 3 double). Diagonals are left to fonts.
const ARMS: [u8; 128] = [
    0x05, 0x0a, 0x50, 0xa0, 0x05, 0x0a, 0x50, 0xa0, 0x05, 0x0a, 0x50, 0xa0, 0x44, 0x48, 0x84, 0x88,
    0x41, 0x42, 0x81, 0x82, 0x14, 0x18, 0x24, 0x28, 0x11, 0x12, 0x21, 0x22, 0x54, 0x58, 0x64, 0x94,
    0xa4, 0x68, 0x98, 0xa8, 0x51, 0x52, 0x61, 0x91, 0xa1, 0x62, 0x92, 0xa2, 0x45, 0x46, 0x49, 0x4a,
    0x85, 0x86, 0x89, 0x8a, 0x15, 0x16, 0x19, 0x1a, 0x25, 0x26, 0x29, 0x2a, 0x55, 0x56, 0x59, 0x5a,
    0x65, 0x95, 0xa5, 0x66, 0x69, 0x96, 0x99, 0x6a, 0x9a, 0xa6, 0xa9, 0xaa, 0x05, 0x0a, 0x50, 0xa0,
    0x0f, 0xf0, 0x4c, 0xc4, 0xcc, 0x43, 0xc1, 0xc3, 0x1c, 0x34, 0x3c, 0x13, 0x31, 0x33, 0x5c, 0xf4,
    0xfc, 0x53, 0xf1, 0xf3, 0x4f, 0xc5, 0xcf, 0x1f, 0x35, 0x3f, 0x5f, 0xf5, 0xff, 0x44, 0x41, 0x11,
    0x14, 0x00, 0x00, 0x00, 0x01, 0x10, 0x04, 0x40, 0x02, 0x20, 0x08, 0x80, 0x09, 0x90, 0x06, 0x60,
];

struct Mask {
    width: usize,
    height: usize,
    alpha: Vec<u8>,
}

impl Mask {
    fn new(cell: CellSize) -> Self {
        let (width, height) = (cell.width as usize, cell.height as usize);
        Self {
            width,
            height,
            alpha: vec![0; width * height],
        }
    }

    /// Fills `[x0, x1) × [y0, y1)` with `alpha`, clamped to the cell.
    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let clamp = |v: i32, max: usize| v.clamp(0, max as i32) as usize;
        let (x0, x1) = (clamp(x0, self.width), clamp(x1, self.width));
        for y in clamp(y0, self.height)..clamp(y1, self.height) {
            self.alpha[y * self.width + x0..y * self.width + x1].fill(alpha);
        }
    }

    fn bitmap(self, cell: CellSize) -> Bitmap {
        Bitmap {
            width: self.width as u32,
            height: self.height as u32,
            left: 0,
            top: cell.baseline,
            color: false,
            rgba: self
                .alpha
                .iter()
                .flat_map(|&a| [255, 255, 255, a])
                .collect(),
        }
    }
}

/// The cell-sized mask for `ch`, or `None` when a font should draw it.
pub fn draw(ch: char, cell: CellSize) -> Option<Bitmap> {
    let code = ch as u32;
    let mut mask = Mask::new(cell);
    let (w, h) = (cell.width as i32, cell.height as i32);
    match code {
        0x2500..=0x257f => {
            let arms = ARMS[(code - 0x2500) as usize];
            if arms == 0 {
                return None;
            }
            let light = (w / 8).max(1);
            // Total width of a weight's strokes; a missing arm still meets at the center.
            let span = |weight: u8| match weight {
                2 => light * 2,
                3 => light * 3,
                _ => light,
            };
            // Strokes of one arm across the axis at `center`: one, or two for double.
            let strokes = |weight: u8, center: i32| {
                let start = center - span(weight) / 2;
                match weight {
                    3 => [
                        (start, start + light),
                        (start + 2 * light, start + 3 * light),
                    ],
                    _ => [(start, start + span(weight)); 2],
                }
            };
            let (cx, cy) = (w / 2, h / 2);
            let [left, right, up, down] = [0, 2, 4, 6].map(|shift| (arms >> shift) & 3);
            // Arms reach across the perpendicular strokes so corners and joins close.
            let v = (
                cx - span(up.max(down)) / 2,
                cx - span(up.max(down)) / 2 + span(up.max(down)),
            );
            let hz = (
                cy - span(left.max(right)) / 2,
                cy - span(left.max(right)) / 2 + span(left.max(right)),
            );
            for (weight, from, to) in [(left, 0, v.1), (right, v.0, w)] {
                if weight != 0 {
                    for (y0, y1) in strokes(weight, cy) {
                        mask.fill(from, y0, to, y1, 255);
                    }
                }
            }
            for (weight, from, to) in [(up, 0, hz.1), (down, hz.0, h)] {
                if weight != 0 {
                    for (x0, x1) in strokes(weight, cx) {
                        mask.fill(x0, from, x1, to, 255);
                    }
                }
            }
        }
        0x2580 => mask.fill(0, 0, w, h / 2, 255),
        0x2581..=0x2588 => {
            let eighths = (code - 0x2580) as i32;
            mask.fill(0, h - h * eighths / 8, w, h, 255);
        }
        0x2589..=0x258f => {
            let eighths = (0x2590 - code) as i32;
            mask.fill(0, 0, w * eighths / 8, h, 255);
        }
        0x2590 => mask.fill(w / 2, 0, w, h, 255),
        0x2591..=0x2593 => mask.fill(0, 0, w, h, [64, 128, 192][(code - 0x2591) as usize]),
        0x2594 => mask.fill(0, 0, w, (h / 8).max(1), 255),
        0x2595 => mask.fill(w - (w / 8).max(1), 0, w, h, 255),
        0x2596..=0x259f => {
            // Quadrants: upper left 1, upper right 2, lower left 4, lower right 8.
            let bits = [
                4,
                8,
                1,
                1 | 4 | 8,
                1 | 8,
                1 | 2 | 4,
                1 | 2 | 8,
                2,
                2 | 4,
                2 | 4 | 8,
            ][(code - 0x2596) as usize];
            let (mx, my) = (w / 2, h / 2);
            for (bit, x0, y0, x1, y1) in [
                (1, 0, 0, mx, my),
                (2, mx, 0, w, my),
                (4, 0, my, mx, h),
                (8, mx, my, w, h),
            ] {
                if bits & bit != 0 {
                    mask.fill(x0, y0, x1, y1, 255);
                }
            }
        }
        _ => return None,
    }
    Some(mask.bitmap(cell))
}
