//! The terminal state machine: VT sequences applied to two screens.

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::{
    cell::{Cell, Color, Flags},
    grid::Grid,
    selection::{Point, Range, Selection},
};

/// Which mouse events the application asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// Presses and releases (1000).
    Click,
    /// Plus motion while a button is held (1002).
    Drag,
    /// Plus all motion (1003).
    Motion,
}

/// Cursor shape requested with DECSCUSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

/// Terminal modes the host reads to encode input.
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub show_cursor: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub insert: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    /// Wheel sends arrow keys on the alternate screen (1007).
    pub alternate_scroll: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            show_cursor: true,
            autowrap: true,
            origin: false,
            insert: false,
            bracketed_paste: false,
            focus_events: false,
            mouse: MouseMode::Off,
            mouse_sgr: false,
            alternate_scroll: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    /// DEC special graphics: line drawing.
    Graphics,
}

/// The write position and the attributes new characters get.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
    pub template: Cell,
    /// The last column was written; the next printable wraps first.
    wrap_next: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Saved {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    shift: usize,
}

/// A terminal: primary and alternate screens, cursor, modes and selection.
pub struct Term {
    primary: Grid,
    alt: Grid,
    alt_active: bool,
    pub cursor: Cursor,
    saved: [Saved; 2],
    top: usize,
    bottom: usize,
    pub modes: Modes,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    shift: usize,
    /// Shape the application requested; `None` uses the configured one.
    pub cursor_shape: Option<CursorShape>,
    title: String,
    title_changed: bool,
    /// The directory the shell reported with OSC 7, in UTF-8.
    cwd: Option<String>,
    responses: Vec<u8>,
    display_offset: usize,
    selection: Option<Selection>,
    /// Default foreground and background reported to OSC 10/11 queries.
    pub report_colors: [[u8; 3]; 2],
    scrollback: usize,
    parser: vte::Parser,
}

/// The local path of an OSC 7 `file://host/path` URI, percent-decoded, or
/// `None` for another scheme. The host names the machine running the shell,
/// which is this one, so only the path matters.
fn parse_directory(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        let hex = |byte: Option<u8>| {
            let byte = byte?;
            char::from(byte).to_digit(16).map(|digit| digit as u8)
        };
        let (high, low) = (hex(bytes.next()), hex(bytes.next()));
        match (high, low) {
            (Some(high), Some(low)) => out.push(high << 4 | low),
            _ => return None,
        }
    }
    let path = String::from_utf8(out).ok()?;
    // Windows URIs keep a leading slash before the drive: `/C:/Users`.
    let path = match path.strip_prefix('/') {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => path,
    };
    (!path.is_empty()).then_some(path)
}

fn graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

impl Term {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let (cols, rows) = (cols.max(2), rows.max(1));
        Self {
            primary: Grid::new(cols, rows, scrollback),
            alt: Grid::new(cols, rows, 0),
            alt_active: false,
            cursor: Cursor::default(),
            saved: [Saved::default(); 2],
            top: 0,
            bottom: rows - 1,
            modes: Modes::default(),
            tabs: (0..cols).map(|x| x % 8 == 0).collect(),
            charsets: [Charset::Ascii; 2],
            shift: 0,
            cursor_shape: None,
            title: String::new(),
            title_changed: false,
            cwd: None,
            responses: Vec::new(),
            display_offset: 0,
            selection: None,
            report_colors: [[0xdd; 3], [0x00; 3]],
            scrollback,
            parser: vte::Parser::new(),
        }
    }

    /// Applies output bytes from the child process.
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    #[inline]
    pub fn grid(&self) -> &Grid {
        if self.alt_active {
            &self.alt
        } else {
            &self.primary
        }
    }
    #[inline]
    fn grid_mut(&mut self) -> &mut Grid {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.primary
        }
    }
    pub fn cols(&self) -> usize {
        self.grid().cols()
    }
    pub fn rows(&self) -> usize {
        self.grid().rows()
    }
    pub fn alt_screen(&self) -> bool {
        self.alt_active
    }

    /// Bytes the terminal answers with (status reports); written back to the child.
    pub fn take_responses(&mut self) -> Option<Vec<u8>> {
        (!self.responses.is_empty()).then(|| std::mem::take(&mut self.responses))
    }

    /// The title, once after each change.
    pub fn take_title(&mut self) -> Option<&str> {
        std::mem::take(&mut self.title_changed).then_some(self.title.as_str())
    }
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The working directory the shell last reported with OSC 7, if any.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    // ---- viewport ----

    /// How many history lines the view is scrolled back.
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }
    /// Scrolls the view by `lines` (positive goes back into history).
    pub fn scroll_display(&mut self, lines: isize) {
        let max = self.grid().history_len() as isize;
        self.display_offset = (self.display_offset as isize + lines).clamp(0, max) as usize;
    }
    pub fn scroll_to_bottom(&mut self) {
        self.display_offset = 0;
    }
    /// The [`Grid::line`] index shown at view row `y`.
    #[inline]
    pub fn view_line(&self, y: usize) -> isize {
        y as isize - self.display_offset as isize
    }

    // ---- selection ----

    pub fn start_selection(&mut self, selection: Selection) {
        self.selection = Some(selection);
    }
    pub fn extend_selection(&mut self, head: Point) {
        if let Some(selection) = &mut self.selection {
            selection.head = head;
        }
    }
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }
    pub fn selection_range(&self) -> Option<Range> {
        let selection = self.selection?;
        let range = selection.range(self.grid());
        (range.start != range.end || selection.kind != crate::SelectionKind::Simple)
            .then_some(range)
    }
    pub fn selection_text(&self) -> Option<String> {
        self.selection_range()?;
        let text = self.selection?.text(self.grid());
        (!text.is_empty()).then_some(text)
    }

    pub fn set_scrollback(&mut self, lines: usize) {
        self.scrollback = lines;
        self.primary.set_max_history(lines);
        self.display_offset = self.display_offset.min(self.primary.history_len());
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if cols == self.cols() && rows == self.rows() {
            return;
        }
        let (active, inactive) = match self.alt_active {
            true => (1, 0),
            false => (0, 1),
        };
        let y = self.primary.resize(
            cols,
            rows,
            if active == 0 {
                self.cursor.y
            } else {
                self.saved[0].cursor.y
            },
        );
        let alt_y = self
            .alt
            .resize(cols, rows, if active == 1 { self.cursor.y } else { 0 });
        self.cursor.y = if self.alt_active { alt_y } else { y };
        self.saved[inactive].cursor.y = self.saved[inactive].cursor.y.min(rows - 1);
        self.saved[inactive].cursor.x = self.saved[inactive].cursor.x.min(cols - 1);
        self.cursor.x = self.cursor.x.min(cols - 1);
        self.cursor.wrap_next = false;
        self.tabs = (0..cols).map(|x| x % 8 == 0).collect();
        self.top = 0;
        self.bottom = rows - 1;
        self.display_offset = self.display_offset.min(self.grid().history_len());
        self.selection = None;
    }

    // ---- primitives ----

    /// Scrolls the region up; lines leaving a top-anchored primary region enter history.
    fn scroll_up_region(&mut self, top: usize, n: usize) {
        let keep = !self.alt_active && top == 0;
        let (bottom, template) = (self.bottom, self.cursor.template);
        let added = self.grid_mut().scroll_up(top, bottom, n, &template, keep);
        if added > 0 {
            if self.display_offset > 0 {
                self.display_offset = (self.display_offset + added).min(self.grid().history_len());
            }
            if let Some(selection) = &mut self.selection {
                selection.shift(added as isize);
            }
        }
    }

    fn linefeed(&mut self) {
        self.cursor.wrap_next = false;
        if self.cursor.y == self.bottom {
            self.scroll_up_region(self.top, 1);
        } else if self.cursor.y + 1 < self.rows() {
            self.cursor.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.wrap_next = false;
        if self.cursor.y == self.top {
            let (top, bottom, template) = (self.top, self.bottom, self.cursor.template);
            self.grid_mut().scroll_down(top, bottom, 1, &template);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Blanks `from..to` on line `y`, also clearing wide characters cut in half.
    fn clear_cells(&mut self, y: usize, from: usize, to: usize) {
        let template = self.cursor.template;
        let row = &mut self.grid_mut().row_mut(y).cells;
        let to = to.min(row.len());
        if from >= to {
            return;
        }
        if from > 0 && row[from].flags.has(Flags::SPACER) {
            row[from - 1] = Cell::blank(&template);
        }
        if to < row.len() && row[to].flags.has(Flags::SPACER) {
            row[to] = Cell::blank(&template);
        }
        row[from..to].fill(Cell::blank(&template));
    }

    fn goto(&mut self, x: usize, y: usize) {
        let (top, bottom) = match self.modes.origin {
            true => (self.top, self.bottom),
            false => (0, self.rows() - 1),
        };
        self.cursor.x = x.min(self.cols() - 1);
        self.cursor.y = (top + y).min(bottom);
        self.cursor.wrap_next = false;
    }

    fn goto_row(&mut self, y: usize) {
        let x = self.cursor.x;
        self.goto(x, y);
    }

    fn save_cursor(&mut self) {
        let index = self.alt_active as usize;
        self.saved[index] = Saved {
            cursor: self.cursor,
            origin: self.modes.origin,
            charsets: self.charsets,
            shift: self.shift,
        };
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved[self.alt_active as usize];
        self.cursor = saved.cursor;
        self.cursor.x = self.cursor.x.min(self.cols() - 1);
        self.cursor.y = self.cursor.y.min(self.rows() - 1);
        self.modes.origin = saved.origin;
        self.charsets = saved.charsets;
        self.shift = saved.shift;
    }

    fn set_alt_screen(&mut self, on: bool, clear: bool) {
        if on == self.alt_active {
            return;
        }
        if on {
            self.save_cursor();
            self.alt_active = true;
            self.saved[1] = self.saved[0];
            if clear {
                let rows = self.rows();
                for y in 0..rows {
                    self.clear_cells(y, 0, usize::MAX);
                }
            }
        } else {
            self.alt_active = false;
            self.restore_cursor();
        }
        self.display_offset = 0;
        self.selection = None;
        self.top = 0;
        self.bottom = self.rows() - 1;
    }

    /// RIS: everything back to power-on state; history is dropped too.
    fn reset(&mut self) {
        let fresh = Term::new(self.cols(), self.rows(), self.scrollback);
        let report_colors = self.report_colors;
        *self = Term {
            report_colors,
            ..fresh
        };
    }

    fn print_char(&mut self, c: char) {
        // Fast path: printable ASCII in the default charset, the bulk of output.
        if (' '..='~').contains(&c)
            && self.charsets[self.shift] == Charset::Ascii
            && !self.modes.insert
        {
            if self.cursor.wrap_next && self.modes.autowrap {
                let y = self.cursor.y;
                self.grid_mut().row_mut(y).wrapped = true;
                self.cursor.x = 0;
                self.linefeed();
            }
            let (x, y, cols) = (self.cursor.x, self.cursor.y, self.cols());
            if self.grid().row(y).cells[x].flags.0 & (Flags::WIDE | Flags::SPACER) != 0 {
                self.clear_cells(y, x, x + 1);
            }
            let mut cell = self.cursor.template;
            cell.ch = c;
            self.grid_mut().row_mut(y).cells[x] = cell;
            if x + 1 >= cols {
                self.cursor.wrap_next = true;
            } else {
                self.cursor.x = x + 1;
            }
            return;
        }
        let c = match self.charsets[self.shift] {
            Charset::Graphics => graphics(c),
            Charset::Ascii => c,
        };
        let width = match c.width() {
            Some(w @ 1..=2) => w,
            _ => return,
        };
        let cols = self.cols();
        if self.cursor.wrap_next && self.modes.autowrap {
            let y = self.cursor.y;
            self.grid_mut().row_mut(y).wrapped = true;
            self.cursor.x = 0;
            self.linefeed();
        }
        if width == 2 && self.cursor.x + 1 >= cols {
            if !self.modes.autowrap {
                return;
            }
            let (x, y) = (self.cursor.x, self.cursor.y);
            self.clear_cells(y, x, cols);
            self.grid_mut().row_mut(y).wrapped = true;
            self.cursor.x = 0;
            self.linefeed();
        }
        let (x, y) = (self.cursor.x, self.cursor.y);
        let template = self.cursor.template;
        if self.modes.insert {
            let row = &mut self.grid_mut().row_mut(y).cells;
            row[x..].rotate_right(width);
        }
        // Overwriting half of a wide character blanks its other half.
        self.clear_cells(y, x, x + width);
        let row = &mut self.grid_mut().row_mut(y).cells;
        let mut cell = template;
        cell.ch = c;
        cell.flags.set(Flags::WIDE | Flags::SPACER, false);
        if width == 2 {
            cell.flags.set(Flags::WIDE, true);
            row[x] = cell;
            let mut spacer = template;
            spacer.ch = ' ';
            spacer.flags.set(Flags::WIDE, false);
            spacer.flags.set(Flags::SPACER, true);
            row[x + 1] = spacer;
        } else {
            row[x] = cell;
        }
        if x + width >= cols {
            self.cursor.x = cols - 1;
            self.cursor.wrap_next = true;
        } else {
            self.cursor.x = x + width;
        }
    }

    fn sgr(&mut self, params: &Params) {
        let t = &mut self.cursor.template;
        let mut iter = params.iter();
        if params.is_empty() {
            *t = Cell::default();
            return;
        }
        while let Some(param) = iter.next() {
            match param[0] {
                0 => *t = Cell::default(),
                1 => t.flags.set(Flags::BOLD, true),
                2 => t.flags.set(Flags::DIM, true),
                3 => t.flags.set(Flags::ITALIC, true),
                4 => t.flags.set(Flags::UNDERLINE, param.get(1) != Some(&0)),
                7 => t.flags.set(Flags::INVERSE, true),
                8 => t.flags.set(Flags::HIDDEN, true),
                9 => t.flags.set(Flags::STRIKE, true),
                21 => t.flags.set(Flags::UNDERLINE, true),
                22 => t.flags.set(Flags::BOLD | Flags::DIM, false),
                23 => t.flags.set(Flags::ITALIC, false),
                24 => t.flags.set(Flags::UNDERLINE, false),
                27 => t.flags.set(Flags::INVERSE, false),
                28 => t.flags.set(Flags::HIDDEN, false),
                29 => t.flags.set(Flags::STRIKE, false),
                n @ 30..=37 => t.fg = Color::Indexed(n as u8 - 30),
                39 => t.fg = Color::Default,
                n @ 40..=47 => t.bg = Color::Indexed(n as u8 - 40),
                49 => t.bg = Color::Default,
                n @ 90..=97 => t.fg = Color::Indexed(n as u8 - 90 + 8),
                n @ 100..=107 => t.bg = Color::Indexed(n as u8 - 100 + 8),
                n @ (38 | 48 | 58) => {
                    // Colon form carries the color in sub-parameters, semicolon form in
                    // the following parameters.
                    let color = if param.len() > 1 {
                        extended_color(&param[1..])
                    } else {
                        let mut rest = [0u16; 4];
                        let kind = iter.next().map(|p| p[0]);
                        let count = match kind {
                            Some(2) => 3,
                            Some(5) => 1,
                            _ => 0,
                        };
                        for slot in rest.iter_mut().take(count) {
                            *slot = iter.next().map(|p| p[0]).unwrap_or(0);
                        }
                        match kind {
                            Some(2) => {
                                Some(Color::Rgb(rest[0] as u8, rest[1] as u8, rest[2] as u8))
                            }
                            Some(5) => Some(Color::Indexed(rest[0] as u8)),
                            _ => None,
                        }
                    };
                    match (n, color) {
                        (38, Some(c)) => t.fg = c,
                        (48, Some(c)) => t.bg = c,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn set_mode(&mut self, private: bool, mode: u16, on: bool) {
        if !private {
            if mode == 4 {
                self.modes.insert = on;
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = on,
            6 => {
                self.modes.origin = on;
                self.goto(0, 0);
            }
            7 => self.modes.autowrap = on,
            25 => self.modes.show_cursor = on,
            47 => self.set_alt_screen(on, false),
            1047 => self.set_alt_screen(on, true),
            1048 => match on {
                true => self.save_cursor(),
                false => self.restore_cursor(),
            },
            1049 => self.set_alt_screen(on, true),
            1000 => self.modes.mouse = if on { MouseMode::Click } else { MouseMode::Off },
            1002 => self.modes.mouse = if on { MouseMode::Drag } else { MouseMode::Off },
            1003 => {
                self.modes.mouse = if on {
                    MouseMode::Motion
                } else {
                    MouseMode::Off
                }
            }
            1004 => self.modes.focus_events = on,
            1006 => self.modes.mouse_sgr = on,
            1007 => self.modes.alternate_scroll = on,
            2004 => self.modes.bracketed_paste = on,
            _ => {}
        }
    }

    fn respond(&mut self, bytes: impl AsRef<[u8]>) {
        self.responses.extend_from_slice(bytes.as_ref());
    }
}

fn extended_color(sub: &[u16]) -> Option<Color> {
    match sub {
        [5, index, ..] => Some(Color::Indexed(*index as u8)),
        // `38:2:colorspace:r:g:b` and the common `38:2:r:g:b`.
        [2, _, r, g, b, ..] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
        [2, r, g, b] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
        _ => None,
    }
}

/// First values of each parameter, with zero meaning "default".
struct Args([u16; 16], usize);

impl Args {
    fn new(params: &Params) -> Self {
        let mut args = [0u16; 16];
        let mut count = 0;
        for param in params.iter().take(16) {
            args[count] = param[0];
            count += 1;
        }
        Self(args, count)
    }
    /// Parameter `i`, or `default` when absent or zero.
    #[inline]
    fn get(&self, i: usize, default: usize) -> usize {
        match self.0[i] {
            0 => default,
            v if i < self.1 => v as usize,
            _ => default,
        }
    }
    /// Parameter `i` where zero is meaningful.
    #[inline]
    fn raw(&self, i: usize) -> u16 {
        if i < self.1 { self.0[i] } else { 0 }
    }
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.cursor.wrap_next = false;
                self.cursor.x = self.cursor.x.saturating_sub(1);
            }
            0x09 => {
                let cols = self.cols();
                let mut x = self.cursor.x + 1;
                while x < cols && !self.tabs[x] {
                    x += 1;
                }
                self.cursor.x = x.min(cols - 1);
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            0x0e => self.shift = 1,
            0x0f => self.shift = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let args = Args::new(params);
        let private = intermediates.first() == Some(&b'?');
        let (cols, rows) = (self.cols(), self.rows());
        let (x, y) = (self.cursor.x, self.cursor.y);
        match (action, intermediates) {
            ('@', []) => {
                let n = args.get(0, 1).min(cols - x);
                let template = self.cursor.template;
                let row = &mut self.grid_mut().row_mut(y).cells;
                row[x..].rotate_right(n);
                row[x..x + n].fill(Cell::blank(&template));
            }
            ('A', []) => {
                let top = if y >= self.top { self.top } else { 0 };
                self.cursor.y = y.saturating_sub(args.get(0, 1)).max(top);
                self.cursor.wrap_next = false;
            }
            ('B' | 'e', []) => {
                let bottom = if y <= self.bottom {
                    self.bottom
                } else {
                    rows - 1
                };
                self.cursor.y = (y + args.get(0, 1)).min(bottom);
                self.cursor.wrap_next = false;
            }
            ('C' | 'a', []) => {
                self.cursor.x = (x + args.get(0, 1)).min(cols - 1);
                self.cursor.wrap_next = false;
            }
            ('D', []) => {
                self.cursor.x = x.saturating_sub(args.get(0, 1));
                self.cursor.wrap_next = false;
            }
            ('E', []) => {
                self.cursor.y = (y + args.get(0, 1)).min(rows - 1);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            ('F', []) => {
                self.cursor.y = y.saturating_sub(args.get(0, 1));
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            ('G' | '`', []) => {
                self.cursor.x = (args.get(0, 1) - 1).min(cols - 1);
                self.cursor.wrap_next = false;
            }
            ('H' | 'f', []) => self.goto(args.get(1, 1) - 1, args.get(0, 1) - 1),
            ('I', []) => {
                for _ in 0..args.get(0, 1) {
                    self.execute(0x09);
                }
            }
            ('J', []) => {
                match args.raw(0) {
                    0 => {
                        self.clear_cells(y, x, cols);
                        for line in y + 1..rows {
                            self.clear_cells(line, 0, cols);
                        }
                    }
                    1 => {
                        for line in 0..y {
                            self.clear_cells(line, 0, cols);
                        }
                        self.clear_cells(y, 0, x + 1);
                    }
                    2 => {
                        for line in 0..rows {
                            self.clear_cells(line, 0, cols);
                        }
                    }
                    3 => {
                        self.grid_mut().clear_history();
                        self.display_offset = 0;
                    }
                    _ => {}
                }
                self.selection = None;
            }
            ('K', []) => match args.raw(0) {
                0 => self.clear_cells(y, x, cols),
                1 => self.clear_cells(y, 0, x + 1),
                2 => self.clear_cells(y, 0, cols),
                _ => {}
            },
            ('L', []) if (self.top..=self.bottom).contains(&y) => {
                let (bottom, template) = (self.bottom, self.cursor.template);
                self.grid_mut()
                    .scroll_down(y, bottom, args.get(0, 1), &template);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            ('M', []) if (self.top..=self.bottom).contains(&y) => {
                let (bottom, template) = (self.bottom, self.cursor.template);
                self.grid_mut()
                    .scroll_up(y, bottom, args.get(0, 1), &template, false);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            ('P', []) => {
                let n = args.get(0, 1).min(cols - x);
                let template = self.cursor.template;
                let row = &mut self.grid_mut().row_mut(y).cells;
                row[x..].rotate_left(n);
                row[cols - n..].fill(Cell::blank(&template));
            }
            ('S', []) => self.scroll_up_region(self.top, args.get(0, 1)),
            ('T', []) => {
                let (top, bottom, template) = (self.top, self.bottom, self.cursor.template);
                self.grid_mut()
                    .scroll_down(top, bottom, args.get(0, 1), &template);
            }
            ('X', []) => self.clear_cells(y, x, x + args.get(0, 1)),
            ('Z', []) => {
                for _ in 0..args.get(0, 1) {
                    let mut x = self.cursor.x;
                    while x > 0 {
                        x -= 1;
                        if self.tabs[x] {
                            break;
                        }
                    }
                    self.cursor.x = x;
                }
            }
            ('b', []) => {
                let previous = (x > 0).then(|| self.grid().row(y).cells[x - 1].ch);
                if let Some(c) = previous {
                    for _ in 0..args.get(0, 1).min(cols * rows) {
                        self.print_char(c);
                    }
                }
            }
            ('c', []) => self.respond(b"\x1b[?6c"),
            ('c', [b'>']) => self.respond(b"\x1b[>1;10;0c"),
            ('d', []) => self.goto_row(args.get(0, 1) - 1),
            ('g', []) => match args.raw(0) {
                0 => self.tabs[x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            ('h', _) | ('l', _) => {
                let on = action == 'h';
                for param in params.iter() {
                    self.set_mode(private, param[0], on);
                }
            }
            ('m', []) => self.sgr(params),
            ('n', []) => match args.raw(0) {
                5 => self.respond(b"\x1b[0n"),
                6 => {
                    let row = match self.modes.origin {
                        true => y - self.top,
                        false => y,
                    };
                    self.respond(format!("\x1b[{};{}R", row + 1, x + 1));
                }
                _ => {}
            },
            ('q', [b' ']) => {
                self.cursor_shape = match args.raw(0) {
                    1 | 2 => Some(CursorShape::Block),
                    3 | 4 => Some(CursorShape::Underline),
                    5 | 6 => Some(CursorShape::Bar),
                    _ => None,
                }
            }
            ('p', [b'!']) => {
                self.modes = Modes::default();
                self.cursor.template = Cell::default();
                self.top = 0;
                self.bottom = rows - 1;
                self.charsets = [Charset::Ascii; 2];
                self.shift = 0;
            }
            ('r', []) => {
                let top = args.get(0, 1) - 1;
                let bottom = args.get(1, rows).min(rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ('s', []) => self.save_cursor(),
            ('u', []) => self.restore_cursor(),
            ('t', []) if args.raw(0) == 18 => self.respond(format!("\x1b[8;{rows};{cols}t")),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cursor.x = 0;
                self.linefeed();
            }
            ([], b'H') => {
                let x = self.cursor.x;
                self.tabs[x] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([b'('], b'0') => self.charsets[0] = Charset::Graphics,
            ([b'('], _) => self.charsets[0] = Charset::Ascii,
            ([b')'], b'0') => self.charsets[1] = Charset::Graphics,
            ([b')'], _) => self.charsets[1] = Charset::Ascii,
            ([b'#'], b'8') => {
                let rows = self.rows();
                for y in 0..rows {
                    for cell in &mut self.grid_mut().row_mut(y).cells {
                        *cell = Cell {
                            ch: 'E',
                            ..Cell::default()
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        match params {
            [b"0" | b"2", rest @ ..] if !rest.is_empty() => {
                let title = rest
                    .iter()
                    .map(|part| String::from_utf8_lossy(part))
                    .collect::<Vec<_>>()
                    .join(";");
                if title != self.title {
                    self.title = title;
                    self.title_changed = true;
                }
            }
            // OSC 7: the shell reports its directory as a `file://host/path`
            // URI, which no other source can answer for a child that changes
            // directory (and the only source on Windows).
            [b"7", rest @ ..] => {
                let uri = rest
                    .iter()
                    .map(|part| String::from_utf8_lossy(part))
                    .collect::<Vec<_>>()
                    .join(";");
                if let Some(cwd) = parse_directory(&uri)
                    && self.cwd.as_deref() != Some(&cwd)
                {
                    self.cwd = Some(cwd);
                }
            }
            [which @ (b"10" | b"11"), b"?"] => {
                let [r, g, b] = self.report_colors[(*which == b"11") as usize];
                let end = if bell_terminated { "\x07" } else { "\x1b\\" };
                let which = std::str::from_utf8(which).unwrap_or("10");
                self.respond(format!(
                    "\x1b]{which};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{end}"
                ));
            }
            _ => {}
        }
    }
}
