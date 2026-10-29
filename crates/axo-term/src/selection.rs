//! Text selection over screen and history lines.

use crate::{
    cell::{Flags, Row},
    grid::Grid,
};

/// A cell position; `line` follows [`Grid::line`] indexing (negative is history).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: isize,
    pub col: usize,
}

/// What a drag selects between its anchor and head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell by cell.
    Simple,
    /// Whole words, from a double click.
    Word,
    /// Whole lines, from a triple click.
    Line,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub kind: SelectionKind,
}

/// An inclusive, ordered, expanded range: `start` to `end` cell inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Point,
    pub end: Point,
}

impl Range {
    #[inline]
    pub fn contains(&self, line: isize, col: usize) -> bool {
        let p = Point { line, col };
        self.start <= p && p <= self.end
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || "_-./~:@+%#?&=".contains(c)
}

fn word_bounds(row: &Row, col: usize) -> (usize, usize) {
    let cells = &row.cells;
    if col >= cells.len() || !is_word(cells[col].ch) {
        return (col, col);
    }
    let mut start = col;
    while start > 0 && (is_word(cells[start - 1].ch) || cells[start - 1].flags.has(Flags::SPACER)) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cells.len()
        && (is_word(cells[end + 1].ch) || cells[end + 1].flags.has(Flags::SPACER))
    {
        end += 1;
    }
    (start, end)
}

impl Selection {
    pub fn new(at: Point, kind: SelectionKind) -> Self {
        Self {
            anchor: at,
            head: at,
            kind,
        }
    }

    /// The ordered range, expanded to words or lines.
    pub fn range(&self, grid: &Grid) -> Range {
        let (mut start, mut end) = match self.anchor <= self.head {
            true => (self.anchor, self.head),
            false => (self.head, self.anchor),
        };
        match self.kind {
            SelectionKind::Simple => {}
            SelectionKind::Word => {
                if let Some(row) = grid.line(start.line) {
                    start.col = word_bounds(row, start.col).0;
                }
                if let Some(row) = grid.line(end.line) {
                    end.col = word_bounds(row, end.col).1;
                }
            }
            SelectionKind::Line => {
                start.col = 0;
                end.col = usize::MAX;
            }
        }
        Range { start, end }
    }

    /// Moves the selection when lines scroll into history.
    pub fn shift(&mut self, lines: isize) {
        self.anchor.line -= lines;
        self.head.line -= lines;
    }

    /// The selected text; soft-wrapped lines join without a newline.
    pub fn text(&self, grid: &Grid) -> String {
        let range = self.range(grid);
        let mut out = String::new();
        let mut line = range.start.line;
        while line <= range.end.line {
            let Some(row) = grid.line(line) else { break };
            let from = if line == range.start.line {
                range.start.col
            } else {
                0
            };
            let to = if line == range.end.line {
                range.end.col.saturating_add(1)
            } else {
                usize::MAX
            };
            row.text(from, to, &mut out);
            if line != range.end.line && !row.wrapped {
                out.push('\n');
            }
            line += 1;
        }
        out
    }
}
