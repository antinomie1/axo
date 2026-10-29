//! Screen text and the attributes that run across it.
//!
//! A row keeps one 32-bit word per column and its attributes as runs, so the
//! cells of a uniform line cost four bytes each instead of eight, and a
//! coloured prompt costs a handful of runs rather than an attribute per cell.
//! The two flags that describe geometry rather than looks — a wide character
//! and its trailing half — live in the text itself, where a zero word marks the
//! half that draws nothing.

/// A cell color before palette resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The palette's default foreground or background.
    #[default]
    Default,
    /// One of the 256 indexed colors.
    Indexed(u8),
    /// A direct 24-bit color.
    Rgb(u8, u8, u8),
}

/// Cell attribute bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(pub u16);

impl Flags {
    pub const BOLD: u16 = 1;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKE: u16 = 1 << 6;
    /// The first half of a double-width character.
    pub const WIDE: u16 = 1 << 7;
    /// The second half of a double-width character; draws nothing.
    pub const SPACER: u16 = 1 << 8;

    /// Whether every bit of `bits` is set.
    #[inline]
    pub fn has(self, bits: u16) -> bool {
        self.0 & bits == bits
    }
    #[inline]
    pub fn set(&mut self, bits: u16, on: bool) {
        if on {
            self.0 |= bits;
        } else {
            self.0 &= !bits;
        }
    }
}

/// The attributes of a run of columns.
///
/// `Flags::WIDE` and `Flags::SPACER` never appear here: they belong to the text,
/// which encodes the trailing half of a wide character as a zero word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
}

impl Style {
    /// A blank cell's style: background color erase keeps only the background.
    #[inline]
    pub fn blank(template: &Style) -> Self {
        Self {
            fg: Color::Default,
            bg: template.bg,
            flags: Flags::default(),
        }
    }
}

/// The word that draws a space.
const SPACE: u32 = b' ' as u32;

/// One line: the characters, and the attributes as runs.
#[derive(Clone, Debug)]
pub struct Row {
    /// One word per column; `0` is the trailing half of a wide character.
    text: Vec<u32>,
    /// `(first column, style)`, sorted by column; the first run starts at 0.
    styles: Vec<(u16, Style)>,
    /// The run found last, so sequential writes and the per-column render walk
    /// do not search again. A stale value simply fails its bounds check.
    hint: std::cell::Cell<u16>,
    /// The line continues on the next one (soft wrap), so copying joins them.
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, template: &Style) -> Self {
        Self {
            text: vec![SPACE; cols],
            styles: vec![(0, Style::blank(template))],
            hint: std::cell::Cell::new(0),
            wrapped: false,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The character at `x`; a space where a wide character's trailing half is.
    #[inline]
    pub fn ch(&self, x: usize) -> char {
        match self.text.get(x) {
            Some(&word) if word != 0 => char::from_u32(word).unwrap_or('\u{fffd}'),
            _ => ' ',
        }
    }

    /// Whether `x` holds the trailing half of a wide character.
    #[inline]
    pub fn is_spacer(&self, x: usize) -> bool {
        self.text.get(x) == Some(&0)
    }

    /// Whether `x` holds the leading half of a wide character.
    #[inline]
    pub fn is_wide(&self, x: usize) -> bool {
        self.is_spacer(x + 1)
    }

    /// The attributes at `x`.
    #[inline]
    pub fn style_at(&self, x: usize) -> Style {
        self.styles[self.run_at(x)].1
    }

    /// Blanks every cell, reusing the allocations.
    pub fn reset(&mut self, cols: usize, template: &Style) {
        if self.text.len() == cols {
            self.text.fill(SPACE);
        } else {
            self.text.clear();
            self.text.resize(cols, SPACE);
        }
        self.styles.clear();
        self.styles.push((0, Style::blank(template)));
        self.wrapped = false;
    }

    /// Resizes the line, blanking the columns that appear and cutting a wide
    /// character in half at the new edge.
    pub fn resize(&mut self, cols: usize, template: &Style) {
        let blank = Style::blank(template);
        if cols < self.text.len() {
            if self.text[cols] == 0 {
                self.text[cols - 1] = SPACE;
            }
            self.text.truncate(cols);
            while matches!(self.styles.last(), Some(run) if run.0 as usize >= cols) {
                self.styles.pop();
            }
            if self.styles.is_empty() {
                self.styles.push((0, blank));
            }
        } else if cols > self.text.len() {
            let from = self.text.len();
            self.text.resize(cols, SPACE);
            self.set_style(from, cols, blank);
        }
    }

    /// Writes printable ASCII starting at `start`, one cell per byte.
    ///
    /// A run overwrites both halves of any wide character it touches: only the
    /// edges of the range can be a half, since the fill replaces everything in
    /// between.
    pub(crate) fn write_ascii(&mut self, start: usize, bytes: &[u8], style: Style) {
        let end = start + bytes.len();
        let blank = Style::blank(&style);
        if start > 0 && self.is_spacer(start) {
            self.text[start - 1] = SPACE;
            self.set_style(start - 1, start, blank);
        }
        if self.is_spacer(end) {
            self.text[end] = SPACE;
            self.set_style(end, end + 1, blank);
        }
        for (cell, byte) in self.text[start..end].iter_mut().zip(bytes) {
            *cell = *byte as u32;
        }
        self.set_style(start, end, style);
    }

    /// Writes one character with its attributes, blanking a wide character it
    /// overlaps.
    pub(crate) fn write_char(&mut self, x: usize, ch: char, width: usize, style: Style) {
        let blank = Style::blank(&style);
        if self.is_spacer(x) && x > 0 {
            self.text[x - 1] = SPACE;
            self.set_style(x - 1, x, blank);
        }
        if self.text.get(x + width) == Some(&0) {
            self.text[x + width] = SPACE;
            self.set_style(x + width, x + width + 1, blank);
        }
        self.text[x] = ch as u32;
        if width == 2 {
            self.text[x + 1] = 0;
        }
        self.set_style(x, x + width, style);
    }

    /// Blanks `from..to`, also clearing wide characters cut in half.
    pub(crate) fn clear(&mut self, from: usize, to: usize, template: &Style) {
        let blank = Style::blank(template);
        let to = to.min(self.text.len());
        if from >= to {
            return;
        }
        if from > 0 && self.is_spacer(from) {
            self.text[from - 1] = SPACE;
            self.set_style(from - 1, from, blank);
        }
        if self.is_spacer(to) {
            self.text[to] = SPACE;
            self.set_style(to, to + 1, blank);
        }
        self.text[from..to].fill(SPACE);
        self.set_style(from, to, blank);
    }

    /// Inserts `n` blank cells at `x`, moving the rest right (`ICH`).
    pub(crate) fn insert_cells(&mut self, x: usize, n: usize, template: &Style) {
        self.shift_cells(x, n as isize, template, Right);
    }

    /// Removes `n` cells at `x`, moving the rest left (`DCH`).
    pub(crate) fn remove_cells(&mut self, x: usize, n: usize, template: &Style) {
        self.shift_cells(x, -(n as isize), template, Left);
    }

    fn shift_cells(&mut self, x: usize, delta: isize, template: &Style, direction: Direction) {
        let cols = self.text.len();
        let n = delta.unsigned_abs();
        if n == 0 || x >= cols {
            return;
        }
        // Rotating the text and rotating the per-column styles must agree, so
        // the styles are materialized and rotated alongside.
        let mut columns: Vec<Style> = (0..cols).map(|column| self.style_at(column)).collect();
        let blank = Style::blank(template);
        match direction {
            Right => {
                self.text[x..].rotate_right(n);
                self.text[x..x + n].fill(SPACE);
                columns[x..].rotate_right(n);
                columns[x..x + n].fill(blank);
            }
            Left => {
                self.text[x..].rotate_left(n);
                self.text[cols - n..].fill(SPACE);
                columns[x..].rotate_left(n);
                columns[cols - n..].fill(blank);
            }
        }
        self.set_style_columns(&columns);
    }

    /// Whether the line is visually empty in `template`'s blank.
    pub(crate) fn is_blank(&self, template: &Style) -> bool {
        let blank = Style::blank(template);
        self.text.iter().all(|word| *word == SPACE) && self.styles.iter().all(|run| run.1 == blank)
    }

    /// Replaces every cell with `ch` in `style` (`DECALN`).
    pub(crate) fn fill_with(&mut self, ch: char, style: Style) {
        self.text.fill(ch as u32);
        self.styles.clear();
        self.styles.push((0, style));
    }

    /// Replaces every run with the per-column styles given.
    fn set_style_columns(&mut self, columns: &[Style]) {
        self.styles.clear();
        let mut current = columns.first().copied().unwrap_or_default();
        self.styles.push((0, current));
        for (x, style) in columns.iter().enumerate().skip(1) {
            if *style != current {
                current = *style;
                self.styles.push((x as u16, current));
            }
        }
    }

    /// Gives `from..to` one style, splitting and merging runs as needed.
    pub(crate) fn set_style(&mut self, from: usize, to: usize, style: Style) {
        let to = to.min(self.text.len());
        if from >= to {
            return;
        }
        let first = self.run_at(from);
        let last = self.run_at(to - 1);
        // The common cases: the range already has this style, or it covers whole
        // runs and only needs the style stamped on them.
        if first == last && self.styles[first].1 == style {
            return;
        }
        if self.styles[first].0 as usize == from && self.run_end(last) == to {
            if first == last {
                self.styles[first].1 = style;
            } else {
                for run in &mut self.styles[first..=last] {
                    run.1 = style;
                }
                self.styles.drain(first + 1..=last);
            }
            self.merge_runs(first);
            return;
        }
        // Split the runs at both ends so the range is covered by whole runs.
        if self.styles[first].0 as usize != from {
            let inherited = self.styles[first].1;
            self.styles.insert(first + 1, (from as u16, inherited));
        }
        let last = self.run_at(to - 1);
        if self.run_end(last) > to {
            let inherited = self.styles[last].1;
            self.styles.insert(last + 1, (to as u16, inherited));
        }
        let first = self.run_at(from);
        let last = self.run_at(to - 1);
        for run in &mut self.styles[first..=last] {
            run.1 = style;
        }
        self.merge_runs(first);
    }

    /// The index of the run that covers column `x`.
    #[inline]
    fn run_at(&self, x: usize) -> usize {
        let x = x as u16;
        let hint = self.hint.get() as usize;
        if hint < self.styles.len() && self.styles[hint].0 <= x && x < self.run_end(hint) as u16 {
            return hint;
        }
        let at = match self.styles.binary_search_by_key(&x, |run| run.0) {
            Ok(at) => at,
            Err(0) => 0,
            Err(after) => after - 1,
        };
        self.hint.set(at as u16);
        at
    }

    /// The column where run `i` ends, exclusive.
    #[inline]
    fn run_end(&self, i: usize) -> usize {
        self.styles
            .get(i + 1)
            .map_or(self.text.len(), |run| run.0 as usize)
    }

    /// Drops runs that repeat their neighbour's style, so runs stay minimal.
    fn merge_runs(&mut self, at: usize) {
        while at + 1 < self.styles.len() && self.styles[at].1 == self.styles[at + 1].1 {
            self.styles.remove(at + 1);
        }
        if at > 0 && self.styles[at - 1].1 == self.styles[at].1 {
            self.styles.remove(at);
            let at = at - 1;
            while at + 1 < self.styles.len() && self.styles[at].1 == self.styles[at + 1].1 {
                self.styles.remove(at + 1);
            }
        }
    }

    /// The row's text without trailing blanks, skipping wide-character spacers.
    pub fn text(&self, from: usize, to: usize, out: &mut String) {
        let to = to.min(self.text.len());
        let from = from.min(to);
        let start = out.len();
        for x in from..to {
            if !self.is_spacer(x) {
                out.push(self.ch(x));
            }
        }
        let trimmed = out[start..].trim_end_matches(' ').len();
        out.truncate(start + trimmed);
    }
}

/// Which way `shift_cells` moves the cells after the edited range.
#[derive(Clone, Copy)]
enum Direction {
    Left,
    Right,
}
use Direction::{Left, Right};

#[cfg(test)]
mod tests {
    use super::*;

    fn style(fg: u8, flags: u16) -> Style {
        Style {
            fg: Color::Indexed(fg),
            bg: Color::Default,
            flags: Flags(flags),
        }
    }

    #[test]
    fn a_new_row_is_blank_and_one_run() {
        let template = Style {
            fg: Color::Indexed(1),
            bg: Color::Indexed(2),
            flags: Flags(Flags::BOLD),
        };
        let row = Row::new(4, &template);
        assert_eq!(row.styles.len(), 1);
        // Blank means background erase: the background survives, nothing else.
        let blank = row.style_at(0);
        assert_eq!(blank.bg, Color::Indexed(2));
        assert_eq!(blank.fg, Color::Default);
        assert_eq!(blank.flags, Flags::default());
    }

    #[test]
    fn writing_merges_equal_neighbours() {
        let mut row = Row::new(8, &Style::default());
        row.write_ascii(0, b"abcd", style(1, 0));
        row.write_ascii(4, b"efgh", style(1, 0));
        assert_eq!(row.styles.len(), 1);
        let mut text = String::new();
        row.text(0, 8, &mut text);
        assert_eq!(text, "abcdefgh");
    }

    #[test]
    fn writing_splits_runs() {
        let mut row = Row::new(8, &Style::default());
        row.write_ascii(0, b"abcdefgh", style(1, 0));
        row.write_ascii(2, b"XY", style(2, Flags::BOLD));
        assert_eq!(row.style_at(1), style(1, 0));
        assert_eq!(row.style_at(2), style(2, Flags::BOLD));
        assert_eq!(row.style_at(3), style(2, Flags::BOLD));
        assert_eq!(row.style_at(4), style(1, 0));
        assert_eq!(row.styles.len(), 3);
        let mut text = String::new();
        row.text(0, 8, &mut text);
        assert_eq!(text, "abXYefgh");
    }

    #[test]
    fn a_wide_character_owns_two_words() {
        let mut row = Row::new(4, &Style::default());
        row.write_char(0, '中', 2, style(3, 0));
        assert!(row.is_wide(0));
        assert!(row.is_spacer(1));
        assert_eq!(row.ch(0), '中');
        assert_eq!(row.style_at(1), style(3, 0));
        // Overwriting the spacer clears the wide character it belongs to.
        row.write_ascii(1, b"x", style(4, 0));
        assert!(!row.is_wide(0));
        assert_eq!(row.ch(0), ' ');
        assert_eq!(row.ch(1), 'x');
        let mut text = String::new();
        row.text(0, 4, &mut text);
        assert_eq!(text, " x");
    }

    #[test]
    fn inserting_and_removing_cells_keeps_styles_aligned() {
        let mut row = Row::new(6, &Style::default());
        row.write_ascii(0, b"abcdef", style(1, 0));
        row.write_ascii(2, b"XY", style(2, 0));
        // abcdef with XY in red at 2..4: "abXYef"
        row.insert_cells(0, 2, &Style::default());
        let mut text = String::new();
        row.text(0, 6, &mut text);
        assert_eq!(text, "  abXY");
        assert_eq!(row.style_at(0), Style::blank(&Style::default()));
        assert_eq!(row.style_at(2), style(1, 0));
        assert_eq!(row.style_at(4), style(2, 0));
        row.remove_cells(0, 2, &Style::default());
        let mut text = String::new();
        row.text(0, 6, &mut text);
        assert_eq!(text, "abXY");
    }

    #[test]
    fn clearing_keeps_only_the_background() {
        let template = Style {
            fg: Color::Indexed(1),
            bg: Color::Indexed(5),
            flags: Flags(Flags::BOLD),
        };
        let mut row = Row::new(4, &Style::default());
        row.write_ascii(0, b"abcd", style(1, 0));
        row.clear(1, 3, &template);
        let mut text = String::new();
        row.text(0, 4, &mut text);
        assert_eq!(text, "a  d");
        let blank = row.style_at(1);
        assert_eq!(blank.bg, Color::Indexed(5));
        assert_eq!(blank.fg, Color::Default);
        assert_eq!(blank.flags, Flags::default());
    }

    #[test]
    fn resizing_cuts_a_wide_character_in_half() {
        let mut row = Row::new(4, &Style::default());
        row.write_char(2, '中', 2, style(1, 0));
        assert!(row.is_wide(2));
        row.resize(3, &Style::default());
        assert_eq!(row.len(), 3);
        assert!(!row.is_wide(2));
        assert_eq!(row.ch(2), ' ');
    }
}
