//! Screen lines plus a bounded scrollback ring.

use std::collections::VecDeque;

use crate::cell::{Cell, Flags, Row};

/// The visible lines of one screen and, for the primary screen, its history.
///
/// History rows keep the width they had when they scrolled off; readers treat
/// cells past a row's end as blank, so a resize never rewrites the history.
pub struct Grid {
    cols: usize,
    lines: Vec<Row>,
    history: VecDeque<Row>,
    max_history: usize,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, max_history: usize) -> Self {
        let blank = Cell::default();
        Self {
            cols,
            lines: (0..rows).map(|_| Row::new(cols, &blank)).collect(),
            history: VecDeque::new(),
            max_history,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.lines.len()
    }
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    #[inline]
    pub fn row(&self, y: usize) -> &Row {
        &self.lines[y]
    }
    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut Row {
        &mut self.lines[y]
    }

    /// A line by signed index: `0..rows` is the screen, negative indexes count
    /// back into history (`-1` is the newest history line).
    pub fn line(&self, y: isize) -> Option<&Row> {
        if y >= 0 {
            self.lines.get(y as usize)
        } else {
            let back = (-y) as usize;
            self.history
                .len()
                .checked_sub(back)
                .map(|index| &self.history[index])
        }
    }

    /// Scrolls `top..=bottom` up by `n`, blanking the freed bottom lines.
    /// Lines leaving the top go to history when `keep` is set; returns how many did.
    pub fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        n: usize,
        template: &Cell,
        keep: bool,
    ) -> usize {
        let n = n.min(bottom + 1 - top);
        if n == 0 {
            return 0;
        }
        self.lines[top..=bottom].rotate_left(n);
        let keep = keep && self.max_history > 0;
        for y in bottom + 1 - n..=bottom {
            if keep {
                let fresh = match self.history.len() >= self.max_history {
                    true => {
                        let mut row = self.history.pop_front().expect("history is full");
                        row.reset(self.cols, template);
                        row
                    }
                    false => Row::new(self.cols, template),
                };
                let old = std::mem::replace(&mut self.lines[y], fresh);
                self.history.push_back(old);
            } else {
                self.lines[y].reset(self.cols, template);
            }
        }
        if keep { n } else { 0 }
    }

    /// Scrolls `top..=bottom` down by `n`, blanking the freed top lines.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, template: &Cell) {
        let n = n.min(bottom + 1 - top);
        self.lines[top..=bottom].rotate_right(n);
        for row in &mut self.lines[top..top + n] {
            row.reset(self.cols, template);
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history.shrink_to_fit();
    }

    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
        while self.history.len() > max {
            self.history.pop_front();
        }
    }

    /// Resizes the screen, keeping the cursor's line visible: shrinking drops
    /// blank lines below the cursor first and then moves top lines to history;
    /// growing pulls lines back from history. Returns the cursor's new line.
    pub fn resize(&mut self, cols: usize, rows: usize, cursor: usize) -> usize {
        let blank = Cell::default();
        let mut cursor = cursor;
        if cols != self.cols {
            for row in &mut self.lines {
                if cols < row.cells.len() && row.cells[cols - 1].flags.has(Flags::WIDE) {
                    row.cells[cols - 1] = Cell::blank(&row.cells[cols - 1]);
                }
                row.cells.resize(cols, blank);
            }
            self.cols = cols;
        }
        while self.lines.len() > rows && self.lines.len() - 1 > cursor {
            if !self.lines.last().unwrap().cells.iter().all(|c| *c == blank) {
                break;
            }
            self.lines.pop();
        }
        if self.lines.len() > rows {
            let extra = self.lines.len() - rows;
            for row in self.lines.drain(..extra) {
                if self.max_history > 0 {
                    if self.history.len() >= self.max_history {
                        self.history.pop_front();
                    }
                    self.history.push_back(row);
                }
            }
            cursor = cursor.saturating_sub(extra);
        }
        while self.lines.len() < rows {
            match self.history.pop_back() {
                Some(mut row) => {
                    row.cells.resize(cols, blank);
                    self.lines.insert(0, row);
                    cursor += 1;
                }
                None => self.lines.push(Row::new(cols, &blank)),
            }
        }
        cursor.min(rows - 1)
    }
}
