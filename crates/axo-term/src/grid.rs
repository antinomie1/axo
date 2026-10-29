//! Screen lines and bounded scrollback in one unified ring buffer.

use std::collections::VecDeque;

use crate::cell::{Row, Style};

/// The visible lines of one screen and, for the primary screen, its history.
///
/// Both live in one [`VecDeque`]: `0..history_len` is history (oldest first)
/// and `history_len..ring.len()` is the active screen (`0..rows`). A full-screen
/// scroll into history only pushes or recycles one [`Row`] without moving the
/// other screen lines.
pub struct Grid {
    cols: usize,
    rows: usize,
    ring: VecDeque<Row>,
    max_history: usize,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, max_history: usize) -> Self {
        let blank = Style::default();
        let mut ring = VecDeque::with_capacity(rows + max_history.min(1024));
        for _ in 0..rows {
            ring.push_back(Row::new(cols, &blank));
        }
        Self {
            cols,
            rows,
            ring,
            max_history,
        }
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }
    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }
    #[inline]
    pub fn history_len(&self) -> usize {
        self.ring.len() - self.rows
    }

    #[inline]
    pub fn row(&self, y: usize) -> &Row {
        let base = self.ring.len() - self.rows;
        &self.ring[base + y]
    }
    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut Row {
        let base = self.ring.len() - self.rows;
        &mut self.ring[base + y]
    }

    /// A line by signed index: `0..rows` is the screen, negative indexes count
    /// back into history (`-1` is the newest history line).
    #[inline]
    pub fn line(&self, y: isize) -> Option<&Row> {
        let idx = self.history_len() as isize + y;
        (idx >= 0).then(|| self.ring.get(idx as usize)).flatten()
    }

    /// Scrolls `top..=bottom` up by `n`, blanking the freed bottom lines.
    /// Lines leaving the top go to history when `keep` is set; returns how many did.
    pub fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        n: usize,
        template: &Style,
        keep: bool,
    ) -> usize {
        let n = n.min(bottom + 1 - top);
        if n == 0 {
            return 0;
        }
        let keep = keep && self.max_history > 0;
        if keep && top == 0 {
            for _ in 0..n {
                let fresh = if self.history_len() >= self.max_history {
                    let mut row = self.ring.pop_front().expect("history is full");
                    row.reset(self.cols, template);
                    row
                } else {
                    Row::new(self.cols, template)
                };
                if bottom + 1 == self.rows {
                    self.ring.push_back(fresh);
                } else {
                    let at = (self.ring.len() - self.rows) + bottom + 1;
                    self.ring.insert(at, fresh);
                }
            }
            return n;
        }
        let base = self.ring.len() - self.rows;
        for _ in 0..n {
            let mut row = self.ring.remove(base + top).expect("valid top");
            row.reset(self.cols, template);
            self.ring.insert(base + bottom, row);
        }
        0
    }

    /// Scrolls `top..=bottom` down by `n`, blanking the freed top lines.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, template: &Style) {
        let n = n.min(bottom + 1 - top);
        let base = self.ring.len() - self.rows;
        for _ in 0..n {
            let mut row = self.ring.remove(base + bottom).expect("valid bottom");
            row.reset(self.cols, template);
            self.ring.insert(base + top, row);
        }
    }

    pub fn clear_history(&mut self) {
        let hist = self.history_len();
        self.ring.drain(..hist);
        self.ring.shrink_to_fit();
    }

    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
        let extra = self.history_len().saturating_sub(max);
        if extra > 0 {
            self.ring.drain(..extra);
        }
    }

    /// Resizes the screen, keeping the cursor's line visible: shrinking drops
    /// blank lines below the cursor first and then moves top lines to history;
    /// growing pulls lines back from history. Returns the cursor's new line.
    pub fn resize(&mut self, cols: usize, rows: usize, cursor: usize) -> usize {
        let blank = Style::default();
        let mut cursor = cursor;
        if cols != self.cols {
            let base = self.history_len();
            for row in self.ring.range_mut(base..) {
                row.resize(cols, &blank);
            }
            self.cols = cols;
        }
        while self.rows > rows && self.rows - 1 > cursor {
            if !self.ring.back().unwrap().is_blank(&blank) {
                break;
            }
            self.ring.pop_back();
            self.rows -= 1;
        }
        if self.rows > rows {
            let extra = self.rows - rows;
            self.rows = rows;
            let drop_hist = self.history_len().saturating_sub(self.max_history);
            if drop_hist > 0 {
                self.ring.drain(..drop_hist);
            }
            cursor = cursor.saturating_sub(extra);
        }
        while self.rows < rows {
            if self.history_len() > 0 {
                self.rows += 1;
                self.row_mut(0).resize(cols, &blank);
                cursor += 1;
            } else {
                self.ring.push_back(Row::new(cols, &blank));
                self.rows += 1;
            }
        }
        cursor.min(rows - 1)
    }
}
