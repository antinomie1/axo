//! The kitty graphics protocol: commands in `ESC _ G <control> ; <payload> ESC \`.
//!
//! Implemented: direct, file and temporary file transmission, 24 and 32 bit
//! pixels, PNG, zlib compression, placements with cropping, scaling and z-index,
//! deletion, queries and the storage quota. Not implemented, and answered with
//! an error so that clients can fall back: shared memory transmission, animation
//! and virtual placements.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    image::{MAX_BYTES, Pixels, Placement},
    term::Term,
};

/// One command's control data, with the protocol's defaults as zeros.
#[derive(Clone, Copy, Default)]
struct Command {
    action: u8,
    delete: u8,
    medium: u8,
    compression: u8,
    format: u32,
    more: bool,
    quiet: u32,
    id: u32,
    number: u32,
    placement: u32,
    /// Source image width and height.
    size: [u32; 2],
    /// Bytes to read from a file, and where from.
    read: [u64; 2],
    /// Crop rectangle: x, y, width, height.
    crop: [u32; 4],
    offset: [u32; 2],
    cells: [u32; 2],
    z: i32,
    /// Leave the cursor where it is.
    hold_cursor: bool,
    /// A Unicode placeholder placement.
    virtual_placement: bool,
}

impl Command {
    fn parse(control: &[u8]) -> Self {
        let mut command = Self::default();
        for pair in control.split(|byte| *byte == b',') {
            let [key, b'=', value @ ..] = pair else {
                continue;
            };
            let letter = value.first().copied().unwrap_or(0);
            let number = integer(value);
            let unsigned = number.clamp(0, u32::MAX.into()) as u32;
            // Screen sizes, kept small enough that pixels computed from them
            // cannot overflow.
            let small = unsigned.min(u16::MAX.into());
            match key {
                b'a' => command.action = letter,
                b'd' => command.delete = letter,
                b't' => command.medium = letter,
                b'o' => command.compression = letter,
                b'f' => command.format = unsigned,
                b'm' => command.more = unsigned != 0,
                b'q' => command.quiet = unsigned,
                b'i' => command.id = unsigned,
                b'I' => command.number = unsigned,
                b'p' => command.placement = unsigned,
                b's' => command.size[0] = unsigned,
                b'v' => command.size[1] = unsigned,
                b'S' => command.read[0] = unsigned.into(),
                b'O' => command.read[1] = unsigned.into(),
                b'x' => command.crop[0] = unsigned,
                b'y' => command.crop[1] = unsigned,
                b'w' => command.crop[2] = unsigned,
                b'h' => command.crop[3] = unsigned,
                b'X' => command.offset[0] = small,
                b'Y' => command.offset[1] = small,
                b'c' => command.cells[0] = small,
                b'r' => command.cells[1] = small,
                b'z' => command.z = number.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
                b'C' => command.hold_cursor = unsigned != 0,
                b'U' => command.virtual_placement = unsigned != 0,
                _ => {}
            }
        }
        command
    }

    /// The transmission a chunked transfer is still gathering.
    fn continues(&self, pending: &Pending) -> bool {
        self.action == 0 || self.action == pending.command.action
    }
}

/// A decimal integer; anything else reads as zero.
fn integer(value: &[u8]) -> i64 {
    let (sign, digits) = match value {
        [b'-', rest @ ..] => (-1, rest),
        _ => (1, value),
    };
    let mut number = 0i64;
    for byte in digits {
        if !byte.is_ascii_digit() {
            return 0;
        }
        number = number
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'));
    }
    sign * number
}

/// A chunked transmission in progress.
pub(crate) struct Pending {
    command: Command,
    data: Vec<u8>,
    decoder: Base64,
}

const INVALID: u8 = 0x80;

/// Sextet of each byte, or [`INVALID`].
const SEXTETS: [u8; 256] = {
    let mut table = [INVALID; 256];
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut i = 0;
    while i < alphabet.len() {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// Base64 decoding that carries a partial group across chunks.
#[derive(Default)]
struct Base64 {
    bits: u32,
    count: u32,
}

impl Base64 {
    fn decode(&mut self, input: &[u8], out: &mut Vec<u8>) {
        out.reserve(input.len() / 4 * 3 + 3);
        let mut rest = input;
        // Whole groups of valid bytes make up all but the very end of a chunk.
        if self.count == 0 {
            let (groups, _) = input.as_chunks::<4>();
            let mut done = 0;
            for group in groups {
                let [a, b, c, d] = group.map(|byte| SEXTETS[byte as usize]);
                if (a | b | c | d) & INVALID != 0 {
                    break;
                }
                let word =
                    u32::from(a) << 18 | u32::from(b) << 12 | u32::from(c) << 6 | u32::from(d);
                out.extend_from_slice(&word.to_be_bytes()[1..]);
                done += 4;
            }
            rest = &input[done..];
        }
        for &byte in rest {
            // Padding ends a group, whatever came before it: clients may pad
            // each chunk on its own.
            if byte == b'=' {
                self.count = 0;
            }
            let sextet = SEXTETS[byte as usize];
            if sextet & INVALID != 0 {
                continue;
            }
            self.bits = self.bits << 6 | u32::from(sextet);
            self.count += 6;
            if self.count >= 8 {
                self.count -= 8;
                out.push((self.bits >> self.count) as u8);
            }
        }
    }
}

type Outcome<T = ()> = Result<T, &'static str>;

/// Reads the file named by a transmission, refusing anything that is not an
/// ordinary file on disk: a pipe would hang the terminal, and escape sequences
/// in untrusted output must not reach devices or `/proc`.
fn read_file(path: &[u8], command: &Command) -> Outcome<Vec<u8>> {
    let path = PathBuf::from(String::from_utf8(path.to_vec()).map_err(|_| "EINVAL:bad path")?);
    let temporary = command.medium == b't';
    if temporary && !is_temporary(&path) {
        return Err("EPERM:not a temporary graphics file");
    }
    let real = std::fs::canonicalize(&path).map_err(|_| "EBADF:cannot read the file")?;
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|dir| real.starts_with(dir))
    {
        return Err("EPERM:not an image file");
    }
    let metadata = std::fs::metadata(&real).map_err(|_| "EBADF:cannot read the file")?;
    if !metadata.is_file() {
        return Err("EBADF:not a regular file");
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err("EFBIG:file too large");
    }
    let data = std::fs::read(&path).map_err(|_| "EBADF:cannot read the file");
    if temporary {
        let _ = std::fs::remove_file(&path);
    }
    let data = data?;
    let [size, offset] = command.read;
    let start = (offset as usize).min(data.len());
    let end = match size {
        0 => data.len(),
        _ => (start + size as usize).min(data.len()),
    };
    Ok(match (start, end) {
        (0, end) if end == data.len() => data,
        _ => data[start..end].to_vec(),
    })
}

/// The protocol's rule for files the terminal may delete: in a temporary
/// directory, with the marker in the name.
fn is_temporary(path: &Path) -> bool {
    let marked = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().contains("tty-graphics-protocol"));
    marked && (path.starts_with(std::env::temp_dir()) || path.starts_with("/tmp"))
}

fn inflate(data: &[u8]) -> Outcome<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, MAX_BYTES)
        .map_err(|_| "EINVAL:bad zlib data")
}

/// Grows `samples`-per-pixel data to RGBA.
fn expand(data: &[u8], width: u32, height: u32, samples: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in data.chunks_exact(samples) {
        rgba.extend_from_slice(&match *pixel {
            [r, g, b] => [r, g, b, 255],
            [gray, alpha] => [gray, gray, gray, alpha],
            [gray] => [gray, gray, gray, 255],
            _ => unreachable!("rgba needs no expansion"),
        });
    }
    rgba
}

fn png(data: &[u8]) -> Outcome<Pixels> {
    const BAD: &str = "EBADPNG:cannot decode the image";
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| BAD)?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if !Pixels::fits(width as usize, height as usize) {
        return Err("EFBIG:image too large");
    }
    let mut buffer = vec![0; reader.output_buffer_size().ok_or(BAD)?];
    let frame = reader.next_frame(&mut buffer).map_err(|_| BAD)?;
    buffer.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => expand(&buffer, width, height, 3),
        png::ColorType::GrayscaleAlpha => expand(&buffer, width, height, 2),
        png::ColorType::Grayscale => expand(&buffer, width, height, 1),
        png::ColorType::Indexed => return Err(BAD),
    };
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

/// Turns a transmission's bytes into pixels.
fn decode(command: &Command, mut data: Vec<u8>) -> Outcome<Pixels> {
    match command.compression {
        0 => {}
        b'z' => data = inflate(&data)?,
        _ => return Err("EINVAL:unknown compression"),
    }
    let [width, height] = command.size;
    let samples = match command.format {
        100 => return png(&data),
        0 | 32 => 4,
        24 => 3,
        _ => return Err("EINVAL:unknown format"),
    };
    if !Pixels::fits(width as usize, height as usize) {
        return Err("EINVAL:bad image size");
    }
    let needed = width as usize * height as usize * samples;
    if data.len() < needed {
        return Err("ENODATA:not enough pixels");
    }
    data.truncate(needed);
    let rgba = match samples {
        4 => data,
        _ => expand(&data, width, height, 3),
    };
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

impl Term {
    /// Runs one graphics command; `raw` follows the `G` of the APC string.
    pub(crate) fn kitty(&mut self, raw: &[u8]) {
        let split = raw
            .iter()
            .position(|byte| *byte == b';')
            .unwrap_or(raw.len());
        let command = Command::parse(&raw[..split]);
        let payload = raw.get(split + 1..).unwrap_or(&[]);
        // A command that does not continue the open transfer abandons it.
        let mut pending = match self.kitty.take() {
            Some(pending) if command.continues(&pending) => pending,
            _ => Box::new(Pending {
                command,
                data: Vec::new(),
                decoder: Base64::default(),
            }),
        };
        pending.decoder.decode(payload, &mut pending.data);
        if pending.data.len() > MAX_BYTES {
            return self.answer(&pending.command, Err("EFBIG:too much data"));
        }
        if command.more {
            self.kitty = Some(pending);
            return;
        }
        let Pending {
            command: mut first,
            data,
            ..
        } = *pending;
        let outcome = match first.action {
            0 | b't' | b'T' | b'q' => self.transmit(&mut first, data),
            b'p' => self.put(&mut first),
            b'd' => return self.delete(&first),
            _ => Err("EINVAL:unsupported action"),
        };
        self.answer(&first, outcome);
    }

    /// Reports a result, unless the client asked for silence or named nothing.
    fn answer(&mut self, command: &Command, outcome: Outcome) {
        let quiet = match outcome {
            Ok(()) => command.quiet >= 1,
            Err(_) => command.quiet >= 2,
        };
        let Command {
            id,
            number,
            placement,
            ..
        } = *command;
        if quiet || (id == 0 && number == 0) {
            return;
        }
        let mut reply = format!("\x1b_Gi={id}");
        if number != 0 {
            reply.push_str(&format!(",I={number}"));
        }
        if placement != 0 {
            reply.push_str(&format!(",p={placement}"));
        }
        let message = outcome.err().unwrap_or("OK");
        self.respond(format!("{reply};{message}\x1b\\"));
    }

    fn transmit(&mut self, command: &mut Command, data: Vec<u8>) -> Outcome {
        if command.virtual_placement {
            return Err("ENOTSUP:virtual placements are not supported");
        }
        let data = match command.medium {
            0 | b'd' => data,
            b'f' | b't' => read_file(&data, command)?,
            _ => return Err("ENOTSUP:unsupported transmission medium"),
        };
        let pixels = Arc::new(decode(command, data)?);
        if command.action == b'q' {
            return Ok(());
        }
        let images = self.images_mut();
        if command.id == 0 && command.number != 0 {
            command.id = images.next_id();
        }
        if command.id != 0 {
            images.store(command.id, command.number, pixels.clone());
        }
        if command.action == b'T' {
            self.show(command, pixels);
        }
        Ok(())
    }

    /// `a=p`: a new placement of a stored image.
    fn put(&mut self, command: &mut Command) -> Outcome {
        if command.virtual_placement {
            return Err("ENOTSUP:virtual placements are not supported");
        }
        let images = self.images_mut();
        if command.id == 0 {
            command.id = images
                .numbered(command.number)
                .ok_or("ENOENT:no such image")?;
        }
        let pixels = images.stored(command.id).ok_or("ENOENT:no such image")?;
        let pixels = pixels.clone();
        self.show(command, pixels);
        Ok(())
    }

    /// Places `pixels` at the cursor and moves the cursor past them.
    fn show(&mut self, command: &Command, pixels: Arc<Pixels>) {
        let [x, y, width, height] = command.crop;
        let (x, y) = (x.min(pixels.width), y.min(pixels.height));
        let side = |asked: u32, room: u32| if asked == 0 { room } else { asked.min(room) };
        let crop = [
            x,
            y,
            side(width, pixels.width - x),
            side(height, pixels.height - y),
        ];
        if crop[2] == 0 || crop[3] == 0 {
            return;
        }
        let line = self.grid().base() + self.cursor.y as i64;
        let placement = Placement {
            crop,
            offset: command.offset,
            cells: command.cells,
            z: command.z,
            image: command.id,
            id: command.placement,
            ..Placement::whole(pixels, line, self.cursor.x)
        };
        let [cols, rows] = placement.span(self.images().cell());
        self.images_mut().place(placement);
        if !command.hold_cursor {
            self.pass_image(cols, rows);
        }
    }

    fn delete(&mut self, command: &Command) {
        let selector = match command.delete {
            0 => b'a',
            selector => selector,
        };
        let free = selector.is_ascii_uppercase();
        let (cell, base) = (self.images().cell(), self.grid().base());
        // Cell coordinates in the protocol count from one.
        let at_x = command.crop[0].saturating_sub(1) as usize;
        let at_y = base + i64::from(command.crop[1].saturating_sub(1));
        let cursor = (self.cursor.x, base + self.cursor.y as i64);
        let z = command.z;
        let everything = |_: u32| true;
        let images = self.images_mut();
        match selector.to_ascii_lowercase() {
            b'a' => images.delete(|p| p.reaches(base, cell), free, everything),
            b'i' | b'n' => {
                let id = match selector.to_ascii_lowercase() {
                    b'i' => Some(command.id),
                    _ => images.numbered(command.number),
                };
                let Some(id) = id.filter(|id| *id != 0) else {
                    return;
                };
                let placement = command.placement;
                let doomed = |p: &Placement| p.image == id && (placement == 0 || p.id == placement);
                images.delete(doomed, free, |stored| stored == id);
            }
            b'r' => {
                let (first, last) = (command.crop[0], command.crop[1]);
                let range = |id: u32| (first..=last).contains(&id);
                images.delete(|p| p.image != 0 && range(p.image), free, range);
            }
            selector => {
                let at = |p: &Placement, x: usize, y: i64| {
                    p.touches_column(x, cell) && p.touches_line(y, cell)
                };
                match selector {
                    b'c' => images.delete(|p| at(p, cursor.0, cursor.1), free, everything),
                    b'p' => images.delete(|p| at(p, at_x, at_y), free, everything),
                    b'q' => images.delete(|p| at(p, at_x, at_y) && p.z == z, free, everything),
                    b'x' => images.delete(|p| p.touches_column(at_x, cell), free, everything),
                    b'y' => images.delete(|p| p.touches_line(at_y, cell), free, everything),
                    b'z' => images.delete(|p| p.z == z, free, everything),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes standard base64, for building commands in tests.
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut group = [0u8; 3];
            group[..chunk.len()].copy_from_slice(chunk);
            let word = u32::from(group[0]) << 16 | u32::from(group[1]) << 8 | u32::from(group[2]);
            for i in 0..4 {
                match i <= chunk.len() {
                    true => out.push(ALPHABET[(word >> (18 - 6 * i) & 63) as usize] as char),
                    false => out.push('='),
                }
            }
        }
        out
    }

    fn command(term: &mut Term, control: &str, payload: &[u8]) {
        let encoded = base64(payload);
        term.advance(format!("\x1b_G{control};{encoded}\x1b\\").as_bytes());
    }

    fn reply(term: &mut Term) -> String {
        String::from_utf8(term.take_responses().unwrap_or_default()).unwrap()
    }

    /// A 2×2 RGBA image.
    const PIXELS: [u8; 16] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 4];

    fn term() -> Term {
        let mut term = Term::new(20, 5, 100);
        term.set_cell_size(10, 20);
        term
    }

    fn placed(term: &Term) -> Vec<&Placement> {
        term.grid().images().iter().collect()
    }

    #[test]
    fn base64_decodes_across_chunks_and_padding() {
        for input in [&b"M"[..], b"Ma", b"Man", b"Many bytes of data"] {
            let encoded = base64(input);
            let mut out = Vec::new();
            Base64::default().decode(encoded.as_bytes(), &mut out);
            assert_eq!(out, input, "whole");
            for split in 0..encoded.len() {
                let (mut out, mut decoder) = (Vec::new(), Base64::default());
                decoder.decode(&encoded.as_bytes()[..split], &mut out);
                decoder.decode(&encoded.as_bytes()[split..], &mut out);
                assert_eq!(out, input, "split at {split}");
            }
        }
    }

    #[test]
    fn padded_chunks_decode_on_their_own() {
        let (mut out, mut decoder) = (Vec::new(), Base64::default());
        for chunk in ["TQ==", "TWE=", "TWFu"] {
            decoder.decode(chunk.as_bytes(), &mut out);
        }
        assert_eq!(out, b"MMaMan");
    }

    #[test]
    fn absurd_sizes_cannot_overflow_or_scroll_forever() {
        let mut term = term();
        command(
            &mut term,
            "a=T,f=32,s=1,v=2,c=4294967295,r=4294967295,X=4294967295,Y=4294967295",
            &PIXELS[..8],
        );
        // A tall sliver scaled by width alone is the worst case for the ratio.
        command(&mut term, "a=T,f=32,s=1,v=2,c=4294967295", &PIXELS[..8]);
        // Each scrolled its way past the screen and out of history, in bounded
        // time, leaving the cursor on it.
        assert!(term.cursor.y < term.rows());
    }

    #[test]
    fn transmit_and_display_answers_and_moves_the_cursor() {
        let mut term = term();
        command(&mut term, "a=T,f=32,s=2,v=2,i=7,c=3,r=2", &PIXELS);
        assert_eq!(reply(&mut term), "\x1b_Gi=7;OK\x1b\\");
        let placement = placed(&term)[0];
        assert_eq!(placement.pixels.rgba, PIXELS);
        assert_eq!(placement.size([10, 20]), [30, 40]);
        assert_eq!((term.cursor.x, term.cursor.y), (3, 1));
    }

    #[test]
    fn rgb_is_expanded_and_short_data_rejected() {
        let mut term = term();
        command(&mut term, "a=T,f=24,s=1,v=2,i=1", &[1, 2, 3, 4, 5, 6]);
        assert_eq!(placed(&term)[0].pixels.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
        reply(&mut term);
        command(&mut term, "a=T,f=24,s=2,v=2,i=2", &[1, 2, 3]);
        assert_eq!(
            reply(&mut term),
            "\x1b_Gi=2;ENODATA:not enough pixels\x1b\\"
        );
    }

    #[test]
    fn chunks_are_reassembled_and_interleaved_output_survives() {
        let mut term = term();
        let encoded = base64(&PIXELS);
        let (first, second) = encoded.split_at(8);
        let stream = format!(
            "\x1b_Ga=T,f=32,s=2,v=2,i=3,m=1,C=1;{first}\x1b\\ok\
             \x1b_Gm=0;{second}\x1b\\"
        );
        for byte in stream.as_bytes() {
            term.advance(std::slice::from_ref(byte));
        }
        assert_eq!(placed(&term)[0].pixels.rgba, PIXELS);
        assert_eq!(term.grid().row(0).ch(0), 'o');
    }

    #[test]
    fn compressed_and_png_payloads_decode() {
        let mut term = term();
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&PIXELS, 6);
        command(&mut term, "a=T,f=32,o=z,s=2,v=2", &zlib);
        assert_eq!(placed(&term)[0].pixels.rgba, PIXELS);

        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&PIXELS).unwrap();
        writer.finish().unwrap();
        command(&mut term, "a=T,f=100,i=4", &png);
        assert_eq!(reply(&mut term), "\x1b_Gi=4;OK\x1b\\");
        assert_eq!(term.grid().images().iter().count(), 2);
        assert_eq!(placed(&term)[1].pixels.rgba, PIXELS);
        command(&mut term, "a=T,f=100,i=5", b"not a png");
        assert!(reply(&mut term).contains("EBADPNG"));
    }

    #[test]
    fn stored_images_are_placed_and_replaced_by_id() {
        let mut term = term();
        command(&mut term, "a=t,f=32,s=2,v=2,i=1,q=1", &PIXELS);
        assert!(placed(&term).is_empty());
        assert_eq!(reply(&mut term), "", "q=1 silences success");
        term.advance(b"\x1b_Ga=p,i=1,p=9,C=1\x1b\\");
        assert_eq!(reply(&mut term), "\x1b_Gi=1,p=9;OK\x1b\\");
        // The same placement id moves it instead of adding another.
        term.advance(b"\x1b[3;4H\x1b_Ga=p,i=1,p=9,C=1,x=1,w=1,z=-5\x1b\\");
        let placements = placed(&term);
        assert_eq!(placements.len(), 1);
        assert_eq!((placements[0].col, placements[0].line), (3, 2));
        assert_eq!(placements[0].crop, [1, 0, 1, 2]);
        assert_eq!(placements[0].z, -5);
        reply(&mut term);
        // Retransmitting the id replaces the image and removes its placements.
        command(&mut term, "a=t,f=32,s=2,v=2,i=1,q=1", &PIXELS);
        assert!(placed(&term).is_empty());
        term.advance(b"\x1b_Ga=p,i=2\x1b\\");
        assert_eq!(reply(&mut term), "\x1b_Gi=2;ENOENT:no such image\x1b\\");
    }

    #[test]
    fn numbers_get_an_id_from_the_terminal() {
        let mut term = term();
        command(&mut term, "a=t,f=32,s=2,v=2,I=13", &PIXELS);
        let answer = reply(&mut term);
        assert!(answer.contains(",I=13;OK"), "{answer}");
        term.advance(b"\x1b_Ga=p,I=13,q=1\x1b\\");
        assert_eq!(placed(&term).len(), 1);
    }

    #[test]
    fn queries_answer_without_storing() {
        let mut term = term();
        command(&mut term, "a=q,f=24,s=1,v=1,i=31,t=d", &[0, 0, 0]);
        assert_eq!(reply(&mut term), "\x1b_Gi=31;OK\x1b\\");
        term.advance(b"\x1b_Ga=p,i=31\x1b\\");
        assert!(reply(&mut term).contains("ENOENT"));
        command(&mut term, "a=q,i=32,t=s,f=24,s=1,v=1", b"/name");
        assert!(reply(&mut term).contains("ENOTSUP"));
    }

    #[test]
    fn unsupported_features_refuse_rather_than_misbehave() {
        let mut term = term();
        command(&mut term, "a=T,U=1,f=32,s=2,v=2,i=1", &PIXELS);
        assert!(reply(&mut term).contains("ENOTSUP"));
        assert!(placed(&term).is_empty());
        term.advance(b"\x1b_Ga=f,i=1\x1b\\");
        assert!(reply(&mut term).contains("EINVAL"));
    }

    #[test]
    fn files_are_read_and_only_temporary_ones_removed() {
        let dir = std::env::temp_dir();
        let name = format!("tty-graphics-protocol-axo-{}", std::process::id());
        let plain = format!("axo-image-{}", std::process::id());
        let (file, kept) = (dir.join(name), dir.join(plain));
        for path in [&file, &kept] {
            std::fs::write(path, PIXELS).unwrap();
        }
        let mut term = term();
        let path = |path: &Path| path.to_str().unwrap().as_bytes().to_vec();
        command(&mut term, "a=T,t=f,f=32,s=2,v=2,i=1", &path(&kept));
        assert_eq!(reply(&mut term), "\x1b_Gi=1;OK\x1b\\");
        assert!(kept.exists());
        command(&mut term, "a=T,t=t,f=32,s=2,v=2,i=2", &path(&file));
        assert_eq!(reply(&mut term), "\x1b_Gi=2;OK\x1b\\");
        assert!(!file.exists());
        // A file without the marker is not the terminal's to delete, and what
        // is not a regular file is not read.
        command(&mut term, "a=T,t=t,f=32,s=2,v=2,i=3", &path(&kept));
        assert!(reply(&mut term).contains("EPERM"));
        assert!(kept.exists());
        std::fs::remove_file(kept).unwrap();
        command(&mut term, "a=T,t=f,f=32,s=2,v=2,i=4", &path(&dir));
        assert!(reply(&mut term).contains("EBADF"));
        // `/proc` exists only on Linux; every Unix has devices.
        #[cfg(target_os = "linux")]
        let secret = "/proc/self/environ";
        #[cfg(all(unix, not(target_os = "linux")))]
        let secret = "/dev/zero";
        #[cfg(unix)]
        {
            command(&mut term, "a=T,t=f,f=32,s=2,v=2,i=5", secret.as_bytes());
            assert!(reply(&mut term).contains("EPERM"));
        }
        // A link does not change what the path really names.
        #[cfg(unix)]
        {
            let link = dir.join(format!("axo-image-link-{}", std::process::id()));
            std::os::unix::fs::symlink(secret, &link).unwrap();
            command(&mut term, "a=T,t=f,f=32,s=2,v=2,i=6", &path(&link));
            assert!(reply(&mut term).contains("EPERM"));
            std::fs::remove_file(link).unwrap();
        }
    }

    #[test]
    fn deletion_selects_and_frees() {
        let mut term = term();
        let two = |term: &mut Term| {
            term.advance(b"\x1b[H\x1b_Ga=p,i=1,q=1,C=1,z=1\x1b\\");
            term.advance(b"\x1b[3;5H\x1b_Ga=p,i=2,q=1,C=1,z=2\x1b\\");
        };
        for id in [1, 2] {
            command(&mut term, &format!("a=t,f=32,s=2,v=2,i={id},q=1"), &PIXELS);
        }
        // Every selector here hits exactly one of the two.
        for selector in [
            "d=i,i=1",
            "d=z,z=2",
            "d=c",
            "d=p,x=5,y=3",
            "d=q,x=5,y=3,z=2",
            "d=x,x=5",
            "d=y,y=3",
            "d=r,x=2,y=2",
        ] {
            term.advance(b"\x1b_Ga=d\x1b\\");
            two(&mut term);
            assert_eq!(placed(&term).len(), 2, "{selector}");
            term.advance(format!("\x1b_Ga=d,{selector}\x1b\\").as_bytes());
            assert_eq!(placed(&term).len(), 1, "{selector}");
        }
        // Lower case keeps the data, upper case frees it.
        term.advance(b"\x1b_Ga=d\x1b\\\x1b_Ga=d,d=i,i=1\x1b\\");
        term.advance(b"\x1b_Ga=p,i=1,q=1\x1b\\");
        assert_eq!(placed(&term).len(), 1);
        term.advance(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        term.advance(b"\x1b_Ga=p,i=1\x1b\\");
        assert!(reply(&mut term).contains("ENOENT"));
    }
}
