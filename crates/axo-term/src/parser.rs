//! A VT parser: the ANSI state machine with a bulk printable run
//! fast path, inline UTF-8 and allocation-free parameters.
//!
//! The parser is a byte-at-a-time state machine (Paul Williams' VT500 diagram,
//! the one `vte` implements), with three departures that matter for speed:
//!
//! * In the ground state the common case is a run of printable ASCII, which the
//!   parser hands to [`Term::print_run`] as one slice. That loop writes cells
//!   without leaving the chunk of memory it owns, instead of going through one
//!   call, one width lookup and two grid indexes per character.
//! * Parameters live in fixed arrays owned by the parser, so a CSI sequence
//!   allocates nothing and dispatch reads them in place.
//! * Image strings are long, so sixel and kitty graphics data are taken in bulk
//!   as well. Every other DCS and SOS/PM/APC string is skipped without
//!   interpreting it: the bytes are consumed until the string terminator.

use crate::{sixel::Decoder, term::Term};

/// Parameter groups one CSI sequence may carry. `vte` stops at the same point,
/// ignoring the sequence once it overflows.
pub(crate) const MAX_GROUPS: usize = 32;
/// Size of the flat value pool shared by `;` groups and `:` sub-parameters.
const MAX_VALUES: usize = 96;
/// Intermediate bytes kept (private markers such as `?` count as intermediates).
const MAX_INTERMEDIATES: usize = 2;
/// Longest OSC payload kept: titles and directories are far shorter.
const OSC_CAP: usize = 8 * 1024;
/// Longest graphics command kept: clients chunk at 4 KiB, but a few send one
/// image unchunked, and 64 MiB of base64 is a 4K frame and then some.
const APC_CAP: usize = 64 << 20;
/// Capacity kept between graphics commands, enough for a run of chunks.
const APC_KEEP: usize = 64 * 1024;

const REPLACEMENT: char = '\u{fffd}';

/// Parsed CSI parameters: a list of groups, where each group holds the values
/// of one `;`-separated parameter, split further on `:`.
#[derive(Clone, Copy)]
pub(crate) struct Params {
    /// `off[i]..off[i + 1]` indexes the values of group `i`.
    off: [u16; MAX_GROUPS + 1],
    flat: [u16; MAX_VALUES],
    groups: u8,
    len: u8,
    /// The group being collected.
    cur: u16,
    /// A parameter did not fit; the sequence is to be ignored.
    full: bool,
}

impl Params {
    const fn new() -> Self {
        Self {
            off: [0; MAX_GROUPS + 1],
            flat: [0; MAX_VALUES],
            groups: 0,
            len: 0,
            cur: 0,
            full: false,
        }
    }

    #[inline]
    fn clear(&mut self) {
        self.groups = 0;
        self.len = 0;
        self.cur = 0;
        self.full = false;
        self.off[0] = 0;
    }

    /// Appends a digit to the group being collected.
    #[inline]
    fn digit(&mut self, byte: u8) {
        if self.full {
            return;
        }
        self.cur = self.cur.saturating_mul(10);
        self.cur = self.cur.saturating_add((byte - b'0') as u16);
    }

    /// `:` — the next value belongs to the same group.
    #[inline]
    fn sub_iterator(&mut self) {
        self.push(self.cur);
        self.cur = 0;
    }

    /// `;` — the next value starts a new group.
    #[inline]
    fn next_group(&mut self) {
        self.end_group();
    }

    /// Closes the group under collection; called at dispatch, so a sequence
    /// without parameters yields one zero-valued group, as in xterm.
    #[inline]
    fn finish(&mut self) {
        self.end_group();
    }

    #[inline]
    fn end_group(&mut self) {
        if self.full {
            self.cur = 0;
            return;
        }
        if self.groups as usize >= MAX_GROUPS {
            self.full = true;
            self.cur = 0;
            return;
        }
        self.push(self.cur);
        self.cur = 0;
        if self.full {
            return;
        }
        self.groups += 1;
        self.off[self.groups as usize] = self.len as u16;
    }

    #[inline]
    fn push(&mut self, value: u16) {
        if self.full {
            return;
        }
        if self.len as usize >= MAX_VALUES {
            self.full = true;
            return;
        }
        self.flat[self.len as usize] = value;
        self.len += 1;
    }

    /// The full value list of group `i`; empty past the end.
    #[inline]
    pub(crate) fn group(&self, i: usize) -> &[u16] {
        if i < self.groups as usize {
            let from = self.off[i] as usize;
            let to = self.off[i + 1] as usize;
            &self.flat[from..to]
        } else {
            &[]
        }
    }

    /// The first value of group `i`, or `0` when absent.
    #[inline]
    pub(crate) fn raw(&self, i: usize) -> u16 {
        if i < self.groups as usize {
            self.flat[self.off[i] as usize]
        } else {
            0
        }
    }
}

/// The parameter list a terminal needs from a CSI sequence: the interface
/// `vte`'s list and this parser's own are both read through.
pub(crate) trait ParamSource {
    /// Number of `;`-separated groups.
    fn len(&self) -> usize;
    /// The values of group `i`, empty past the end.
    fn group(&self, i: usize) -> &[u16];
    /// The first value of group `i`, or `0` when absent.
    fn raw(&self, i: usize) -> u16;
    /// Parameter `i`, where zero and absence both mean `default`.
    #[inline]
    fn get(&self, i: usize, default: usize) -> usize {
        match self.raw(i) {
            0 => default,
            value => value as usize,
        }
    }
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ParamSource for Params {
    #[inline]
    fn len(&self) -> usize {
        self.groups as usize
    }
    #[inline]
    fn group(&self, i: usize) -> &[u16] {
        Params::group(self, i)
    }
    #[inline]
    fn raw(&self, i: usize) -> u16 {
        Params::raw(self, i)
    }
}

#[cfg(test)]
impl ParamSource for vte::Params {
    fn len(&self) -> usize {
        self.iter().count()
    }
    fn group(&self, i: usize) -> &[u16] {
        self.iter().nth(i).unwrap_or(&[])
    }
    fn raw(&self, i: usize) -> u16 {
        self.group(i).first().copied().unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    /// The parameters of a `DCS`, up to the byte that names the string.
    DcsEntry,
    /// Sixel data, decoded in bulk.
    Sixel,
    /// An `APC` string, kept in bulk if it turns out to be a graphics command.
    Apc,
    /// `DCS` and the `SOS`/`PM`/`APC` strings: consumed without interpretation.
    String,
}

pub(crate) struct Parser {
    state: State,
    params: Params,
    inter: [u8; MAX_INTERMEDIATES],
    inter_len: u8,
    /// A sequence collects parameters or intermediates it cannot hold.
    ignoring: bool,
    osc: Vec<u8>,
    /// The OSC payload outgrew its buffer; the sequence is dropped.
    osc_overflow: bool,
    /// The sixel image in progress.
    sixel: Option<Box<Decoder>>,
    /// The graphics command in progress.
    apc: Vec<u8>,
    /// Leading bytes of a UTF-8 character split across two calls.
    partial: [u8; 4],
    partial_len: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: State::Ground,
            params: Params::new(),
            inter: [0; MAX_INTERMEDIATES],
            inter_len: 0,
            ignoring: false,
            osc: Vec::new(),
            osc_overflow: false,
            sixel: None,
            apc: Vec::new(),
            partial: [0; 4],
            partial_len: 0,
        }
    }
}

impl Parser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Applies `bytes` to `term`, carrying state across calls.
    pub(crate) fn advance(&mut self, term: &mut Term, bytes: &[u8]) {
        let mut i = 0;
        if self.partial_len != 0 {
            i = self.resume_utf8(term, bytes);
        }
        while i < bytes.len() {
            if self.state == State::Ground {
                i += self.ground(term, &bytes[i..]);
            } else {
                i += self.step(term, &bytes[i..]);
            }
        }
    }

    /// The ground state: print a printable run, then act on the byte that
    /// interrupted it. Returns how many bytes were consumed.
    #[inline]
    fn ground(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let mut i = 0;
        if term.fast_print_ready() {
            while i < bytes.len() && matches!(bytes[i], 0x20..=0x7e) {
                i += 1;
            }
            if i != 0 {
                term.print_run(&bytes[..i]);
            }
            if i == bytes.len() {
                return i;
            }
        }
        match bytes[i] {
            0x1b => {
                self.begin_escape();
                i + 1
            }
            // C0 controls, and single bytes that are not valid UTF-8 but name a
            // C1 control: executed, exactly as `vte` does.
            0x00..=0x1f | 0x80..=0x9f => {
                term.execute(bytes[i]);
                i + 1
            }
            0x7f => i + 1,
            // Printable but not taken by the fast path: the graphics charset or
            // insert mode is active.
            0x20..=0x7e => {
                term.print(bytes[i] as char);
                i + 1
            }
            _ => i + self.utf8(term, &bytes[i..]),
        }
    }

    /// Decodes one UTF-8 character, stashing it when the buffer ends inside it.
    fn utf8(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let need = match bytes[0] {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => {
                term.print(REPLACEMENT);
                return 1;
            }
        };
        let total = need + 1;
        if bytes.len() < total {
            let len = bytes.len();
            self.partial[..len].copy_from_slice(&bytes[..len]);
            self.partial_len = len as u8;
            return len;
        }
        match decode(&bytes[..total]) {
            Some(c) => {
                emit(term, c);
                total
            }
            None => {
                // One byte is dropped, as `str::from_utf8` reports for a lone
                // lead byte; the rest is re-examined by the next iteration.
                term.print(REPLACEMENT);
                1
            }
        }
    }

    fn resume_utf8(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let have = self.partial_len as usize;
        let total = match self.partial[0] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        let take = (total - have).min(bytes.len());
        self.partial[have..have + take].copy_from_slice(&bytes[..take]);
        if have + take < total {
            self.partial_len = (have + take) as u8;
            return take;
        }
        self.partial_len = 0;
        match decode(&self.partial[..total]) {
            Some(c) => emit(term, c),
            None => term.print(REPLACEMENT),
        }
        take
    }

    /// One byte of a sequence, or as much of an image string as there is.
    /// Returns how many bytes were consumed.
    fn step(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let byte = bytes[0];
        match self.state {
            State::Escape => self.escape(term, byte),
            State::EscapeIntermediate => self.escape_intermediate(term, byte),
            State::CsiEntry => self.csi_entry(term, byte),
            State::CsiParam => self.csi_param(term, byte),
            State::CsiIntermediate => self.csi_intermediate(term, byte),
            State::CsiIgnore => self.csi_ignore(term, byte),
            State::OscString => self.osc_string(term, byte),
            State::DcsEntry => self.dcs_entry(term, byte),
            State::String => self.ignore_string(term, byte),
            State::Sixel => return self.sixel(term, bytes),
            State::Apc => return self.apc(term, bytes),
            State::Ground => unreachable!(),
        }
        1
    }

    #[inline]
    fn begin_escape(&mut self) {
        self.reset();
        self.state = State::Escape;
    }

    #[inline]
    fn reset(&mut self) {
        self.params.clear();
        self.inter_len = 0;
        self.ignoring = false;
    }

    fn escape(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            // An intermediate byte (charset designation, `#`, ...).
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::EscapeIntermediate;
            }
            0x50 => {
                self.reset();
                self.state = State::DcsEntry;
            }
            0x58 | 0x5e => self.state = State::String, // SOS, PM
            0x5f => self.state = State::Apc,
            0x5b => {
                self.reset();
                self.state = State::CsiEntry;
            }
            0x5d => {
                self.osc.clear();
                self.osc_overflow = false;
                self.state = State::OscString;
            }
            0x18 | 0x1a => {
                term.execute(byte);
                self.state = State::Ground;
            }
            // ESC twice in a row restarts the sequence.
            0x1b => self.reset(),
            0x30..=0x4f | 0x51..=0x57 | 0x59..=0x5a | 0x5c | 0x60..=0x7e => {
                term.esc(&self.inter[..self.inter_len as usize], self.ignoring, byte);
                self.state = State::Ground;
            }
            _ => self.anywhere(term, byte),
        }
    }

    fn escape_intermediate(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            0x20..=0x2f => self.collect(byte),
            0x30..=0x7e => {
                term.esc(&self.inter[..self.inter_len as usize], self.ignoring, byte);
                self.state = State::Ground;
            }
            0x7f => {}
            _ => self.anywhere(term, byte),
        }
    }

    fn csi_entry(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => {
                self.params.digit(byte);
                self.state = State::CsiParam;
            }
            0x3a => {
                self.params.sub_iterator();
                self.state = State::CsiParam;
            }
            0x3b => {
                self.params.next_group();
                self.state = State::CsiParam;
            }
            // Private markers `<`, `=`, `>`, `?` are collected as intermediates.
            0x3c..=0x3f => {
                self.collect(byte);
                self.state = State::CsiParam;
            }
            0x40..=0x7e => self.dispatch(term, byte),
            _ => self.anywhere(term, byte),
        }
    }

    fn csi_param(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            }
            0x30..=0x39 => self.params.digit(byte),
            0x3a => self.params.sub_iterator(),
            0x3b => self.params.next_group(),
            0x3c..=0x3f => self.state = State::CsiIgnore,
            0x40..=0x7e => self.dispatch(term, byte),
            0x7f => {}
            _ => self.anywhere(term, byte),
        }
    }

    fn csi_intermediate(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            0x20..=0x2f => self.collect(byte),
            0x30..=0x3f => self.state = State::CsiIgnore,
            0x40..=0x7e => self.dispatch(term, byte),
            0x7f => {}
            _ => self.anywhere(term, byte),
        }
    }

    fn csi_ignore(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => term.execute(byte),
            0x20..=0x3f | 0x7f => {}
            0x40..=0x7e => self.state = State::Ground,
            _ => self.anywhere(term, byte),
        }
    }

    #[inline]
    fn dispatch(&mut self, term: &mut Term, action: u8) {
        self.params.finish();
        term.csi(
            &self.params,
            &self.inter[..self.inter_len as usize],
            self.ignoring || self.params.full,
            action,
        );
        self.state = State::Ground;
    }

    fn osc_string(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
            0x07 => self.osc_end(term, true),
            0x18 | 0x1a => {
                self.osc_end(term, false);
                term.execute(byte);
            }
            // ESC ends the string; the next byte continues as an escape, so
            // `ESC \` is the string terminator and anything else is a new one.
            0x1b => {
                self.osc_end(term, false);
                self.reset();
                self.state = State::Escape;
            }
            _ if self.osc_overflow => {}
            _ => {
                if self.osc.len() >= OSC_CAP {
                    self.osc_overflow = true;
                } else {
                    self.osc.push(byte);
                }
            }
        }
    }

    fn osc_end(&mut self, term: &mut Term, bell: bool) {
        if !self.osc_overflow {
            term.osc(&self.osc, bell);
        }
        self.osc.clear();
        self.osc_overflow = false;
        self.state = State::Ground;
    }

    /// The header of a DCS: `q` makes it sixel data, any other final byte a
    /// string the terminal has no action for.
    #[inline(never)]
    fn dcs_entry(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x18 | 0x1a | 0x1b | 0x9c => {
                self.end_string(term, byte);
            }
            0x30..=0x39 => self.params.digit(byte),
            0x3b => self.params.next_group(),
            b'q' if self.inter_len == 0 => {
                self.params.finish();
                // `P2` asks for unpainted pixels to stay transparent.
                self.sixel = Some(term.sixel_decoder(self.params.raw(1) == 1));
                self.state = State::Sixel;
            }
            0x40..=0x7e => self.state = State::String,
            _ => self.collect(byte),
        }
    }

    /// Sixel data up to the terminator. Returns how many bytes were consumed.
    #[inline(never)]
    fn sixel(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let decoder = self.sixel.as_mut().expect("the state implies a decoder");
        let end = decoder.feed(bytes);
        if end == bytes.len() {
            return end;
        }
        let decoder = self.sixel.take().expect("the state implies a decoder");
        if self.end_string(term, bytes[end])
            && let Some(pixels) = decoder.finish()
        {
            term.sixel(pixels);
        }
        end + 1
    }

    /// An APC string: its first byte tells a graphics command, gathered until
    /// the terminator, from anything else. Returns how many bytes were consumed.
    #[inline(never)]
    fn apc(&mut self, term: &mut Term, bytes: &[u8]) -> usize {
        let end = bytes
            .iter()
            .position(|byte| is_string_end(*byte))
            .unwrap_or(bytes.len());
        let foreign = self.apc.is_empty() && end != 0 && bytes[0] != b'G';
        if foreign || self.apc.len() + end > APC_CAP {
            self.apc = Vec::new();
            self.state = State::String;
            return 0;
        }
        self.apc.extend_from_slice(&bytes[..end]);
        if end == bytes.len() {
            return end;
        }
        if self.end_string(term, bytes[end]) && !self.apc.is_empty() {
            term.kitty(&self.apc[1..]);
        }
        self.apc.clear();
        self.apc.shrink_to(APC_KEEP);
        end + 1
    }

    /// DCS and the other string sequences: nothing inside is interpreted.
    fn ignore_string(&mut self, term: &mut Term, byte: u8) {
        if is_string_end(byte) {
            self.end_string(term, byte);
        }
    }

    /// Leaves a string on a byte [`is_string_end`] accepts. True when the
    /// string is complete, false when `CAN` or `SUB` cancelled it.
    fn end_string(&mut self, term: &mut Term, byte: u8) -> bool {
        match byte {
            0x18 | 0x1a => {
                term.execute(byte);
                self.state = State::Ground;
                false
            }
            0x1b => {
                self.reset();
                self.state = State::Escape;
                true
            }
            _ => {
                self.state = State::Ground;
                true
            }
        }
    }

    /// `CAN`, `SUB` and `ESC` act in every state.
    #[inline]
    fn anywhere(&mut self, term: &mut Term, byte: u8) {
        match byte {
            0x18 | 0x1a => {
                term.execute(byte);
                self.state = State::Ground;
            }
            0x1b => {
                self.reset();
                self.state = State::Escape;
            }
            _ => {}
        }
    }

    #[inline]
    fn collect(&mut self, byte: u8) {
        if (self.inter_len as usize) < MAX_INTERMEDIATES {
            self.inter[self.inter_len as usize] = byte;
            self.inter_len += 1;
        } else {
            self.ignoring = true;
        }
    }
}

/// Whether `byte` ends a string: `CAN`, `SUB`, `ESC` or a C1 `ST`.
#[inline]
fn is_string_end(byte: u8) -> bool {
    matches!(byte, 0x18 | 0x1a | 0x1b | 0x9c)
}

/// A decoded character is a C1 control when it falls in `0x80..=0x9f`.
#[inline]
fn emit(term: &mut Term, c: char) {
    match c as u32 {
        code @ 0x80..=0x9f => term.execute(code as u8),
        _ => term.print(c),
    }
}

/// Decodes one complete UTF-8 sequence, rejecting overlong forms, surrogates
/// and values above `U+10FFFF`.
#[inline]
fn decode(bytes: &[u8]) -> Option<char> {
    let (len, min) = match bytes[0] {
        0xc2..=0xdf => (2, 0x80),
        0xe0..=0xef => (3, 0x800),
        _ => (4, 0x1_0000),
    };
    if bytes.len() < len {
        return None;
    }
    let mut code = (bytes[0] as u32) & (0x7f >> len);
    for &byte in &bytes[1..len] {
        if byte & 0xc0 != 0x80 {
            return None;
        }
        code = (code << 6) | (byte & 0x3f) as u32;
    }
    if code < min || code > 0x10_ffff || (0xd800..=0xdfff).contains(&code) {
        return None;
    }
    char::from_u32(code)
}
