//! A sixel decoder.
//!
//! The picture is a band of six rows at a time, written column by column with
//! a current color. Data arrives far more often than commands, so [`Decoder::feed`]
//! stays in one loop over the bytes and fills whole spans at once for a repeat,
//! the form most encoders compress to. Pixels stay `u32` while decoding, in a
//! buffer that doubles as the image grows, and are packed once at the end.
//!
//! The aspect ratio in raster attributes is ignored: every encoder in use writes
//! square pixels.

use crate::image::{MAX_BYTES, MAX_SIDE, Pixels};

/// Color registers, the number xterm and foot give an image; also reported to
/// applications through `XTSMGRAPHICS`.
pub(crate) const REGISTERS: usize = 1024;

/// The first registers of a VT340, in percent.
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

#[inline]
fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_ne_bytes([r, g, b, 255])
}

/// A color from percentages.
fn percent(r: u32, g: u32, b: u32) -> u32 {
    let scale = |value: u32| (value.min(100) * 255 / 100) as u8;
    rgba(scale(r), scale(g), scale(b))
}

/// A color from DEC's hue, lightness and saturation, where hue 0 is blue.
fn hls(hue: u32, lightness: u32, saturation: u32) -> u32 {
    let sector = ((hue + 240) % 360) as f32 / 60.0;
    let (l, s) = (
        lightness.min(100) as f32 / 100.0,
        saturation.min(100) as f32 / 100.0,
    );
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let channel = |value: f32| ((value + l - chroma / 2.0) * 255.0).round() as u8;
    rgba(channel(r), channel(g), channel(b))
}

/// The byte class a run of digits belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Data,
    Repeat,
    Color,
    Raster,
}

pub(crate) struct Decoder {
    pixels: Vec<u32>,
    /// Allocated columns and rows of `pixels`.
    stride: usize,
    rows: usize,
    /// The extent in use: painted, or declared by raster attributes.
    width: usize,
    height: usize,
    /// Where the next column goes; `y` is the top of the band.
    x: usize,
    y: usize,
    color: u32,
    /// What unpainted pixels hold.
    background: u32,
    palette: Box<[u32; REGISTERS]>,
    command: Command,
    args: [u32; 5],
    arg: usize,
    repeat: usize,
}

impl Decoder {
    pub(crate) fn new(background: u32) -> Box<Self> {
        let mut palette = Box::new([rgba(0, 0, 0); REGISTERS]);
        for (register, [r, g, b]) in palette.iter_mut().zip(VT340) {
            *register = percent(r.into(), g.into(), b.into());
        }
        Box::new(Self {
            pixels: Vec::new(),
            stride: 0,
            rows: 0,
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            color: palette[0],
            background,
            palette,
            command: Command::Data,
            args: [0; 5],
            arg: 0,
            repeat: 1,
        })
    }

    /// Decodes `bytes` up to the first byte that can end the string, and returns
    /// its index, or the length when there is none.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> usize {
        for (i, &byte) in bytes.iter().enumerate() {
            match byte {
                b'?'..=b'~' => {
                    if self.command != Command::Data {
                        self.finish_command();
                    }
                    self.band(byte - b'?');
                }
                b'0'..=b'9' => {
                    let arg = &mut self.args[self.arg];
                    *arg = arg
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                }
                b';' => self.arg = (self.arg + 1).min(self.args.len() - 1),
                0x18 | 0x1a | 0x1b | 0x9c => {
                    self.finish_command();
                    return i;
                }
                _ => {
                    self.finish_command();
                    match byte {
                        b'!' => self.command = Command::Repeat,
                        b'#' => self.command = Command::Color,
                        b'"' => self.command = Command::Raster,
                        b'$' => self.x = 0,
                        b'-' => {
                            self.x = 0;
                            self.y = (self.y + 6).min(MAX_SIDE);
                        }
                        _ => {}
                    }
                }
            }
        }
        bytes.len()
    }

    fn finish_command(&mut self) {
        let [first, second, third, fourth, fifth] = std::mem::take(&mut self.args);
        let given = std::mem::take(&mut self.arg) + 1;
        match std::mem::replace(&mut self.command, Command::Data) {
            Command::Data => {}
            Command::Repeat => self.repeat = (first as usize).clamp(1, MAX_SIDE),
            Command::Color => {
                let register = first as usize % REGISTERS;
                if given == 5 {
                    self.palette[register] = match second {
                        1 => hls(third, fourth, fifth),
                        _ => percent(third, fourth, fifth),
                    };
                }
                self.color = self.palette[register];
            }
            Command::Raster => {
                let (width, height) = (third as usize, fourth as usize);
                if Pixels::fits(width, height) && self.reserve(width, height) {
                    self.width = self.width.max(width);
                    self.height = self.height.max(height);
                }
            }
        }
    }

    /// Writes one sixel, `repeat` columns wide.
    #[inline]
    fn band(&mut self, bits: u8) {
        let count = std::mem::replace(&mut self.repeat, 1);
        let end = self.x + count;
        if (end > self.stride || self.y + 6 > self.rows) && !self.reserve(end, self.y + 6) {
            return;
        }
        if bits != 0 {
            let mut at = self.y * self.stride + self.x;
            for row in 0..6 {
                if bits >> row & 1 != 0 {
                    self.pixels[at..at + count].fill(self.color);
                }
                at += self.stride;
            }
            self.height = self
                .height
                .max(self.y + (u8::BITS - bits.leading_zeros()) as usize);
        }
        self.x = end;
        self.width = self.width.max(end);
    }

    /// Makes room for `cols` × `rows`, doubling so that an image of unknown size
    /// copies itself a logarithmic number of times. False when it cannot grow.
    #[cold]
    fn reserve(&mut self, cols: usize, rows: usize) -> bool {
        if cols <= self.stride && rows <= self.rows {
            return true;
        }
        if cols > MAX_SIDE || rows > MAX_SIDE {
            return false;
        }
        let exact = (cols.max(self.stride), rows.max(self.rows));
        let doubled = (
            exact.0.max(self.stride * 2).min(MAX_SIDE),
            exact.1.max(self.rows * 2).min(MAX_SIDE),
        );
        let bytes = |(cols, rows): (usize, usize)| cols * rows * size_of::<u32>();
        let (stride, height) = match bytes(doubled) <= MAX_BYTES {
            true => doubled,
            false if bytes(exact) <= MAX_BYTES => exact,
            false => return false,
        };
        let mut pixels = vec![self.background; stride * height];
        if self.stride != 0 {
            for (row, from) in self.pixels.chunks_exact(self.stride).enumerate() {
                pixels[row * stride..][..self.stride].copy_from_slice(from);
            }
        }
        self.pixels = pixels;
        self.stride = stride;
        self.rows = height;
        true
    }

    /// The finished image; `None` when nothing was painted or declared.
    pub(crate) fn finish(&self) -> Option<Pixels> {
        let (width, height) = (self.width, self.height.min(self.rows));
        if !Pixels::fits(width, height) {
            return None;
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in self.pixels.chunks_exact(self.stride).take(height) {
            for pixel in &row[..width] {
                rgba.extend_from_slice(&pixel.to_ne_bytes());
            }
        }
        Some(Pixels {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(data: &[u8], background: u32) -> Option<Pixels> {
        let mut decoder = Decoder::new(background);
        assert_eq!(decoder.feed(data), data.len());
        decoder.finish()
    }

    fn pixel(image: &Pixels, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.width + x) * 4) as usize;
        image.rgba[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn paints_bands_columns_and_repeats() {
        // `~` sets all six rows, `?` none, `@` only the top one.
        let image = decode(b"#1;2;100;0;0~~@$#2;2;0;100;0??~-#1!3~", 0).unwrap();
        assert_eq!((image.width, image.height), (3, 12));
        assert_eq!(pixel(&image, 0, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 2, 0), [0, 255, 0, 255], "`$` overpaints");
        assert_eq!(pixel(&image, 2, 5), [0, 255, 0, 255]);
        assert_eq!(pixel(&image, 2, 6), [255, 0, 0, 255], "`-` starts a band");
        assert_eq!(pixel(&image, 1, 11), [255, 0, 0, 255], "`!3` repeats");
    }

    #[test]
    fn height_ends_at_the_last_painted_row() {
        // `F` is 0b000111: the band is only three rows tall.
        let image = decode(b"#15F", 0).unwrap();
        assert_eq!((image.width, image.height), (1, 3));
    }

    #[test]
    fn raster_attributes_declare_the_size() {
        let opaque = rgba(1, 2, 3);
        let image = decode(b"\"1;1;4;8#15~", opaque).unwrap();
        assert_eq!((image.width, image.height), (4, 8));
        assert_eq!(pixel(&image, 3, 7), [1, 2, 3, 255], "background fill");
        // More data than declared still shows.
        let image = decode(b"\"1;1;1;1#15~~", 0).unwrap();
        assert_eq!((image.width, image.height), (2, 6));
    }

    #[test]
    fn colors_use_the_registers_and_hls() {
        // Register 2 keeps the VT340 red; 3 redefined in HLS: hue 120 is red.
        let image = decode(b"#2~#3;1;120;50;100~", 0).unwrap();
        assert_eq!(pixel(&image, 0, 0), [204, 33, 33, 255]);
        assert_eq!(pixel(&image, 1, 0), [255, 0, 0, 255]);
        // Hue 0 is blue.
        assert_eq!(hls(0, 50, 100), rgba(0, 0, 255));
        assert_eq!(hls(240, 50, 100), rgba(0, 255, 0));
    }

    #[test]
    fn nothing_painted_is_no_image() {
        assert!(decode(b"#0;2;0;0;0", 0).is_none());
        assert!(decode(b"", 0).is_none());
    }

    #[test]
    fn stops_at_a_terminator_and_resumes_across_reads() {
        let mut decoder = Decoder::new(0);
        assert_eq!(decoder.feed(b"#1;2;1"), 6);
        assert_eq!(decoder.feed(b"00;0;0!1"), 8);
        assert_eq!(decoder.feed(b"0~\x1b\\"), 2);
        let image = decoder.finish().unwrap();
        assert_eq!((image.width, image.height), (10, 6));
        assert_eq!(pixel(&image, 9, 5), [255, 0, 0, 255]);
    }

    #[test]
    fn hostile_sizes_stay_bounded() {
        assert!(decode(b"\"1;1;4000000000;4000000000", 0).is_none());
        // Repeats past the limit are dropped, and an absurd one alone is not
        // allowed to allocate its way there.
        let image = decode(b"#15!99999999999~!99999999999~", 0).unwrap();
        assert!(image.width as usize <= MAX_SIDE);
        // Rows past the limit are dropped too.
        let tall = "#15~-".repeat(MAX_SIDE);
        let image = decode(tall.as_bytes(), 0).unwrap();
        assert!(image.height as usize <= MAX_SIDE);
    }
}
