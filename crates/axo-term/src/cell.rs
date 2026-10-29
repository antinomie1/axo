//! One grid cell and its attributes.

/// A cell color before palette resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// A character with its colors and attributes; 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: Flags::default(),
        }
    }
}

impl Cell {
    /// A blank cell keeping only the background of `template` (background color erase).
    #[inline]
    pub fn blank(template: &Cell) -> Self {
        Self {
            bg: template.bg,
            ..Self::default()
        }
    }
}

/// One line of cells.
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next one (soft wrap), so copying joins them.
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, template: &Cell) -> Self {
        Self {
            cells: vec![Cell::blank(template); cols],
            wrapped: false,
        }
    }

    /// Blanks every cell, reusing the allocation.
    #[inline]
    pub fn reset(&mut self, cols: usize, template: &Cell) {
        let blank = Cell::blank(template);
        if self.cells.len() == cols {
            self.cells.fill(blank);
        } else {
            self.cells.clear();
            self.cells.resize(cols, blank);
        }
        self.wrapped = false;
    }

    /// The row's text without trailing blanks, skipping wide-character spacers.
    pub fn text(&self, from: usize, to: usize, out: &mut String) {
        let to = to.min(self.cells.len());
        let start = out.len();
        for cell in &self.cells[from.min(to)..to] {
            if !cell.flags.has(Flags::SPACER) {
                out.push(cell.ch);
            }
        }
        let trimmed = out[start..].trim_end_matches(' ').len();
        out.truncate(start + trimmed);
    }
}
