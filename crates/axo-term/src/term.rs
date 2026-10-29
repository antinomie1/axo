//! The terminal state machine: VT sequences applied to two screens.

use unicode_width::UnicodeWidthChar;

use crate::{
    cell::{Color, Flags, Style},
    grid::Grid,
    parser::{ParamSource, Parser},
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
    /// The attributes a printed character inherits.
    pub pen: Style,
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
    parser: Parser,
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
            parser: Parser::new(),
        }
    }

    /// Applies output bytes from the child process.
    pub fn advance(&mut self, bytes: &[u8]) {
        // The parser needs `&mut self` for the terminal it drives, so it is
        // moved out for the call; it owns no allocation and is small.
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// Whether printable ASCII can skip the per-character path: the graphics
    /// charset and insert mode both need it.
    #[inline(always)]
    pub(crate) fn fast_print_ready(&self) -> bool {
        !self.modes.insert && matches!(self.charsets[self.shift], Charset::Ascii)
    }

    /// Writes a run of printable ASCII (0x20..=0x7e) with the default charset.
    ///
    /// This is the path essentially all output takes: the row takes the whole
    /// slice and writes its text plane in one pass, then the attributes as a
    /// single run.
    pub(crate) fn print_run(&mut self, run: &[u8]) {
        let cols = self.cols();
        let pen = self.cursor.pen;
        let mut i = 0;
        while i < run.len() {
            if self.cursor.wrap_next && self.modes.autowrap {
                let y = self.cursor.y;
                self.grid_mut().row_mut(y).wrapped = true;
                self.cursor.x = 0;
                self.linefeed();
            }
            let (start, y) = (self.cursor.x, self.cursor.y);
            let len = (cols - start).min(run.len() - i);
            self.grid_mut()
                .row_mut(y)
                .write_ascii(start, &run[i..i + len], pen);
            let end = start + len;
            i += len;
            if end >= cols {
                self.cursor.x = cols - 1;
                self.cursor.wrap_next = true;
            } else {
                self.cursor.x = end;
            }
        }
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
        let (bottom, template) = (self.bottom, self.cursor.pen);
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
            let (top, bottom, template) = (self.top, self.bottom, self.cursor.pen);
            self.grid_mut().scroll_down(top, bottom, 1, &template);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
        }
    }

    /// Blanks `from..to` on line `y`, also clearing wide characters cut in half.
    fn clear_cells(&mut self, y: usize, from: usize, to: usize) {
        let pen = self.cursor.pen;
        self.grid_mut().row_mut(y).clear(from, to, &pen);
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
            let pen = self.cursor.pen;
            self.grid_mut().row_mut(y).write_ascii(x, &[c as u8], pen);
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
        let pen = self.cursor.pen;
        if self.modes.insert {
            self.grid_mut().row_mut(y).insert_cells(x, width, &pen);
        }
        // The row blanks a wide character the character overlaps.
        self.grid_mut().row_mut(y).write_char(x, c, width, pen);
        if x + width >= cols {
            self.cursor.x = cols - 1;
            self.cursor.wrap_next = true;
        } else {
            self.cursor.x = x + width;
        }
    }

    fn sgr<P: ParamSource>(&mut self, params: &P) {
        let mut pen = self.cursor.pen;
        if params.is_empty() {
            self.cursor.pen = Style::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let group = params.group(i);
            i += 1;
            match group[0] {
                0 => pen = Style::default(),
                1 => pen.flags.set(Flags::BOLD, true),
                2 => pen.flags.set(Flags::DIM, true),
                3 => pen.flags.set(Flags::ITALIC, true),
                4 => pen.flags.set(Flags::UNDERLINE, group.get(1) != Some(&0)),
                7 => pen.flags.set(Flags::INVERSE, true),
                8 => pen.flags.set(Flags::HIDDEN, true),
                9 => pen.flags.set(Flags::STRIKE, true),
                21 => pen.flags.set(Flags::UNDERLINE, true),
                22 => pen.flags.set(Flags::BOLD | Flags::DIM, false),
                23 => pen.flags.set(Flags::ITALIC, false),
                24 => pen.flags.set(Flags::UNDERLINE, false),
                27 => pen.flags.set(Flags::INVERSE, false),
                28 => pen.flags.set(Flags::HIDDEN, false),
                29 => pen.flags.set(Flags::STRIKE, false),
                n @ 30..=37 => pen.fg = Color::Indexed(n as u8 - 30),
                39 => pen.fg = Color::Default,
                n @ 40..=47 => pen.bg = Color::Indexed(n as u8 - 40),
                49 => pen.bg = Color::Default,
                n @ 90..=97 => pen.fg = Color::Indexed(n as u8 - 90 + 8),
                n @ 100..=107 => pen.bg = Color::Indexed(n as u8 - 100 + 8),
                n @ (38 | 48 | 58) => {
                    // Colon form carries the color in the same group's
                    // sub-parameters, semicolon form in the groups that follow.
                    let color = if group.len() > 1 {
                        extended_color(&group[1..])
                    } else {
                        // `38;2;r;g;b` and `38;5;index` carry their arguments in
                        // the groups that follow this one.
                        let kind = params.group(i).first().copied();
                        let value = |k: usize| params.group(i + k).first().copied().unwrap_or(0);
                        let (r, g, b) = (value(1), value(2), value(3));
                        let color = match kind {
                            Some(2) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                            Some(5) => Some(Color::Indexed(r as u8)),
                            _ => None,
                        };
                        i += match kind {
                            Some(2) => 4,
                            Some(5) => 2,
                            Some(_) => 1,
                            None => 0,
                        };
                        color
                    };
                    match (n, color) {
                        (38, Some(c)) => pen.fg = c,
                        (48, Some(c)) => pen.bg = c,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        self.cursor.pen = pen;
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

/// The actions the parser performs on the terminal.
impl Term {
    /// Prints one character at the cursor.
    pub(crate) fn print(&mut self, c: char) {
        self.print_char(c);
    }

    /// Runs a C0 control.
    pub(crate) fn execute(&mut self, byte: u8) {
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

    /// Runs a CSI sequence (`ESC [ ... final`).
    pub(crate) fn csi<P: ParamSource>(
        &mut self,
        params: &P,
        inter: &[u8],
        ignore: bool,
        action: u8,
    ) {
        if ignore {
            return;
        }
        let private = inter.first() == Some(&b'?');
        let (cols, rows) = (self.cols(), self.rows());
        let (x, y) = (self.cursor.x, self.cursor.y);
        match (action, inter) {
            (b'@', []) => {
                let n = params.get(0, 1).min(cols - x);
                let pen = self.cursor.pen;
                self.grid_mut().row_mut(y).insert_cells(x, n, &pen);
            }
            (b'A', []) => {
                let top = if y >= self.top { self.top } else { 0 };
                self.cursor.y = y.saturating_sub(params.get(0, 1)).max(top);
                self.cursor.wrap_next = false;
            }
            (b'B' | b'e', []) => {
                let bottom = if y <= self.bottom {
                    self.bottom
                } else {
                    rows - 1
                };
                self.cursor.y = (y + params.get(0, 1)).min(bottom);
                self.cursor.wrap_next = false;
            }
            (b'C' | b'a', []) => {
                self.cursor.x = (x + params.get(0, 1)).min(cols - 1);
                self.cursor.wrap_next = false;
            }
            (b'D', []) => {
                self.cursor.x = x.saturating_sub(params.get(0, 1));
                self.cursor.wrap_next = false;
            }
            (b'E', []) => {
                self.cursor.y = (y + params.get(0, 1)).min(rows - 1);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            (b'F', []) => {
                self.cursor.y = y.saturating_sub(params.get(0, 1));
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            (b'G' | b'`', []) => {
                self.cursor.x = (params.get(0, 1) - 1).min(cols - 1);
                self.cursor.wrap_next = false;
            }
            (b'H' | b'f', []) => self.goto(params.get(1, 1) - 1, params.get(0, 1) - 1),
            (b'I', []) => {
                for _ in 0..params.get(0, 1) {
                    self.execute(0x09);
                }
            }
            (b'J', []) => {
                match params.raw(0) {
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
            (b'K', []) => match params.raw(0) {
                0 => self.clear_cells(y, x, cols),
                1 => self.clear_cells(y, 0, x + 1),
                2 => self.clear_cells(y, 0, cols),
                _ => {}
            },
            (b'L', []) if (self.top..=self.bottom).contains(&y) => {
                let (bottom, template) = (self.bottom, self.cursor.pen);
                self.grid_mut()
                    .scroll_down(y, bottom, params.get(0, 1), &template);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            (b'M', []) if (self.top..=self.bottom).contains(&y) => {
                let (bottom, template) = (self.bottom, self.cursor.pen);
                self.grid_mut()
                    .scroll_up(y, bottom, params.get(0, 1), &template, false);
                self.cursor.x = 0;
                self.cursor.wrap_next = false;
            }
            (b'P', []) => {
                let n = params.get(0, 1).min(cols - x);
                let pen = self.cursor.pen;
                self.grid_mut().row_mut(y).remove_cells(x, n, &pen);
            }
            (b'S', []) => self.scroll_up_region(self.top, params.get(0, 1)),
            (b'T', []) => {
                let (top, bottom, template) = (self.top, self.bottom, self.cursor.pen);
                self.grid_mut()
                    .scroll_down(top, bottom, params.get(0, 1), &template);
            }
            (b'X', []) => self.clear_cells(y, x, x + params.get(0, 1)),
            (b'Z', []) => {
                for _ in 0..params.get(0, 1) {
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
            (b'b', []) => {
                let previous = (x > 0).then(|| self.grid().row(y).ch(x - 1));
                if let Some(c) = previous {
                    for _ in 0..params.get(0, 1).min(cols * rows) {
                        self.print_char(c);
                    }
                }
            }
            (b'c', []) => self.respond(b"\x1b[?6c"),
            (b'c', [b'>']) => self.respond(b"\x1b[>1;10;0c"),
            (b'd', []) => self.goto_row(params.get(0, 1) - 1),
            (b'g', []) => match params.raw(0) {
                0 => self.tabs[x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            (b'h', _) | (b'l', _) => {
                let on = action == b'h';
                for i in 0..params.len() {
                    self.set_mode(private, params.raw(i), on);
                }
            }
            (b'm', []) => self.sgr(params),
            (b'n', []) => match params.raw(0) {
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
            (b'q', [b' ']) => {
                self.cursor_shape = match params.raw(0) {
                    1 | 2 => Some(CursorShape::Block),
                    3 | 4 => Some(CursorShape::Underline),
                    5 | 6 => Some(CursorShape::Bar),
                    _ => None,
                }
            }
            (b'p', [b'!']) => {
                self.modes = Modes::default();
                self.cursor.pen = Style::default();
                self.top = 0;
                self.bottom = rows - 1;
                self.charsets = [Charset::Ascii; 2];
                self.shift = 0;
            }
            (b'r', []) => {
                let top = params.get(0, 1) - 1;
                let bottom = params.get(1, rows).min(rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            (b's', []) => self.save_cursor(),
            (b'u', []) => self.restore_cursor(),
            (b't', []) if params.raw(0) == 18 => self.respond(format!("\x1b[8;{rows};{cols}t")),
            _ => {}
        }
    }

    /// Runs an escape sequence (`ESC`, intermediates, final).
    pub(crate) fn esc(&mut self, inter: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (inter, byte) {
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
                    self.grid_mut().row_mut(y).fill_with('E', Style::default());
                }
            }
            _ => {}
        }
    }

    /// Runs an operating system command (`ESC ] ... BEL` or `ESC \`), which the
    /// parser hands over as its raw payload.
    pub(crate) fn osc(&mut self, raw: &[u8], bell_terminated: bool) {
        let (command, payload) = match raw.iter().position(|byte| *byte == b';') {
            Some(at) => (&raw[..at], &raw[at + 1..]),
            None => (raw, &raw[raw.len()..]),
        };
        match (command, payload) {
            (b"0" | b"2", payload) if !payload.is_empty() => {
                let title = String::from_utf8_lossy(payload);
                if title != self.title {
                    self.title = title.into_owned();
                    self.title_changed = true;
                }
            }
            // OSC 7: the shell reports its directory as a `file://host/path`
            // URI, which no other source can answer for a child that changes
            // directory (and the only source on Windows).
            (b"7", payload) => {
                let uri = String::from_utf8_lossy(payload);
                if let Some(cwd) = parse_directory(&uri)
                    && self.cwd.as_deref() != Some(&cwd)
                {
                    self.cwd = Some(cwd);
                }
            }
            (command @ (b"10" | b"11"), b"?") => {
                let [r, g, b] = self.report_colors[(command == b"11") as usize];
                let end = if bell_terminated { "\x07" } else { "\x1b\\" };
                let which = std::str::from_utf8(command).unwrap_or("10");
                self.respond(format!(
                    "\x1b]{which};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{end}"
                ));
            }
            _ => {}
        }
    }
}

/// The hand-written parser is checked against the reference implementation
/// (`vte`) it replaced: both drive the same terminal methods, over random
/// output split at random points, and the resulting state must be identical.
#[cfg(test)]
mod differential {
    use super::*;

    /// Feeds `vte`'s callbacks into the same actions the new parser calls.
    struct Reference<'a>(&'a mut Term);

    impl vte::Perform for Reference<'_> {
        fn print(&mut self, c: char) {
            self.0.print(c);
        }
        fn execute(&mut self, byte: u8) {
            self.0.execute(byte);
        }
        fn csi_dispatch(
            &mut self,
            params: &vte::Params,
            intermediates: &[u8],
            ignore: bool,
            action: char,
        ) {
            self.0.csi(params, intermediates, ignore, action as u8);
        }
        fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
            self.0.esc(intermediates, ignore, byte);
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
            let mut raw = Vec::new();
            for (i, part) in params.iter().enumerate() {
                if i > 0 {
                    raw.push(b';');
                }
                raw.extend_from_slice(part);
            }
            self.0.osc(&raw, bell_terminated);
        }
    }

    /// Everything an application can observe, as text, for a diffable failure.
    fn dump(term: &Term) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "alt={} cursor=({},{},wrap={}) tpl={:?}/{:?}/{} top={} bottom={} shift={} \
             display={} shape={:?} title={:?} cwd={:?} responses={:?}\n",
            term.alt_active,
            term.cursor.x,
            term.cursor.y,
            term.cursor.wrap_next,
            term.cursor.pen.fg,
            term.cursor.pen.bg,
            term.cursor.pen.flags.0,
            term.top,
            term.bottom,
            term.shift,
            term.display_offset,
            term.cursor_shape,
            term.title,
            term.cwd,
            String::from_utf8_lossy(&term.responses),
        ));
        out.push_str(&format!(
            "modes={:?} charsets={:?} tabs={:?}\n",
            (
                term.modes.app_cursor,
                term.modes.app_keypad,
                term.modes.show_cursor,
                term.modes.autowrap,
                term.modes.origin,
                term.modes.insert,
                term.modes.bracketed_paste,
                term.modes.focus_events,
                term.modes.mouse,
                term.modes.mouse_sgr,
                term.modes.alternate_scroll
            ),
            term.charsets,
            term.tabs,
        ));
        for (name, grid) in [("primary", &term.primary), ("alt", &term.alt)] {
            out.push_str(&format!(
                "{name} {}x{} history={}\n",
                grid.cols(),
                grid.rows(),
                grid.history_len()
            ));
            for line in -(grid.history_len() as isize)..grid.rows() as isize {
                let Some(row) = grid.line(line) else { continue };
                out.push_str(if row.wrapped { "W|" } else { "-|" });
                for x in 0..grid.cols() {
                    let style = row.style_at(x);
                    out.push_str(&format!(
                        "{}/{}/{}/{:?}/{:?}/{}/",
                        row.ch(x),
                        row.is_spacer(x) as u8,
                        row.is_wide(x) as u8,
                        style.fg,
                        style.bg,
                        style.flags.0
                    ));
                }
                out.push('\n');
            }
        }
        out
    }

    /// A small deterministic generator: xorshift64*, so failures reproduce.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: u64) -> usize {
            (self.next() % n) as usize
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len() as u64)]
        }
    }

    /// One random piece of terminal output.
    fn fragment(rng: &mut Rng, out: &mut Vec<u8>, utf8: bool) {
        const CSI_FINALS: &[u8] = b"ABCDEFGHIJKLMNOPSTXZ@`abcdeghlmnpqrstu";
        const COLORS: &[&str] = &[
            "38;5;196",
            "48;5;21",
            "38;2;255;128;0",
            "48;2;1;2;3",
            "38:2::9:8:7",
            "38:5:33",
            "58;5;9",
            "38;2;1;2",
            "48:2::1:2:3",
        ];
        match rng.below(100) {
            0..=34 => {
                // Printable ASCII, sometimes long enough to wrap several lines.
                let len = 1 + rng.below(40);
                for _ in 0..len {
                    out.push(b' ' + rng.below(95) as u8);
                }
            }
            35..=44 if utf8 => {
                // Non-ASCII text: narrow, wide, and C1 controls written as UTF-8.
                out.extend_from_slice(rng.pick(&[
                    "héllo".as_bytes(),
                    "日本語".as_bytes(),
                    "│─└┐".as_bytes(),
                    "🙂".as_bytes(),
                    "✓".as_bytes(),
                    "æøå".as_bytes(),
                    "\u{80}\u{9b}".as_bytes(),
                ]));
            }
            35..=44 => {
                // Without UTF-8 in play, box drawing comes from the graphics
                // charset instead.
                out.extend_from_slice(b"\x1b(0lqk\x1b(B");
            }
            45..=57 => {
                // Control characters that reach the grid.
                out.push(*rng.pick(b"\n\r\t\x08\x0e\x0f\x07\x00\x18\x1a\x7f"));
            }
            58..=80 => {
                // SGR, sometimes with extended colors.
                out.extend_from_slice(b"\x1b[");
                let parts = rng.below(4) + 1;
                for i in 0..parts {
                    if i > 0 {
                        out.push(b';');
                    }
                    match rng.below(10) {
                        0..=3 => out.extend_from_slice(rng.pick(COLORS).as_bytes()),
                        4 => out.extend_from_slice(b"4:3"),
                        5 => out.extend_from_slice(b"38;5"),
                        _ => {
                            let codes: [&[u8]; 10] = [
                                b"0", b"1", b"2", b"4", b"7", b"22", b"39", b"49", b"90", b"100",
                            ];
                            out.extend_from_slice(codes[rng.below(10)]);
                        }
                    }
                }
                out.push(b'm');
            }
            81..=92 => {
                // Movement, erasing, editing and modes.
                out.extend_from_slice(b"\x1b[");
                if rng.below(4) == 0 {
                    out.push(*rng.pick(b"?<>"));
                }
                let parts = rng.below(3);
                for i in 0..parts {
                    if i > 0 {
                        out.push(b';');
                    }
                    let values: [&[u8]; 10] = [
                        b"1", b"2", b"3", b"5", b"10", b"24", b"80", b"1049", b"1000", b"2004",
                    ];
                    out.extend_from_slice(values[rng.below(10)]);
                }
                out.push(*rng.pick(CSI_FINALS));
            }
            93..=97 => {
                // OSC with a short payload, BEL- or ST-terminated.
                let payload = *rng.pick(&[
                    "0;title",
                    "2;a b;c",
                    "7;file://host/tmp/x%20y",
                    "10;?",
                    "11;?",
                    "0;",
                    "52;c;YWJj",
                    "9;notify;hi",
                    "133;A",
                ]);
                out.extend_from_slice(b"\x1b]");
                out.extend_from_slice(payload.as_bytes());
                out.extend_from_slice(if rng.below(2) == 0 {
                    b"\x07"
                } else {
                    b"\x1b\\"
                });
            }
            98 => {
                // ESC sequences with and without intermediates.
                out.push(0x1b);
                if rng.below(2) == 0 {
                    out.push(*rng.pick(b"()#"));
                }
                out.push(*rng.pick(b"78DEHMc=>\\*"));
            }
            _ => {
                // DCS / SOS / PM / APC payloads.
                out.extend_from_slice(rng.pick(&[
                    b"\x1bP1$r0m\x1b\\".as_slice(),
                    b"\x1bP>|axo\x1b\\".as_slice(),
                    b"\x1bX junk \x1b\\".as_slice(),
                    b"\x1b^pm\x07".as_slice(),
                    b"\x1b_apc\x1b\\".as_slice(),
                    b"\x1bPq#0;2;0;0;0\x1b\\".as_slice(),
                ]));
            }
        }
    }

    /// Generates `count` fragments into one byte stream.
    fn corpus(seed: u64, count: usize, utf8: bool) -> Vec<u8> {
        let mut rng = Rng(seed);
        let mut out = Vec::new();
        for _ in 0..count {
            fragment(&mut rng, &mut out, utf8);
        }
        out
    }

    /// Feeds both parsers the same pieces and compares after every one.
    fn compare(pieces: &[&[u8]], label: &str) {
        let (cols, rows) = (24, 8);
        let mut mine = Term::new(cols, rows, 32);
        let mut theirs = Term::new(cols, rows, 32);
        let mut parser = vte::Parser::new();
        for piece in pieces {
            mine.advance(piece);
            parser.advance(&mut Reference(&mut theirs), piece);
            let (ours, reference) = (dump(&mine), dump(&theirs));
            assert!(
                ours == reference,
                "{label}: diverged after {:?}; first difference:\n{}",
                String::from_utf8_lossy(piece),
                first_difference(&ours, &reference)
            );
        }
    }

    fn every_split(bytes: &[u8], chunk: usize) -> Vec<&[u8]> {
        bytes.chunks(chunk.max(1)).collect()
    }

    /// Splits at character boundaries: a multi-byte character is never cut.
    fn char_splits(bytes: &[u8], chunk: usize) -> Vec<&[u8]> {
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < bytes.len() {
            let mut end = (start + chunk.max(1)).min(bytes.len());
            while end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
                end += 1;
            }
            pieces.push(&bytes[start..end]);
            start = end;
        }
        pieces
    }

    /// The first differing lines of two dumps, for a readable failure.
    fn first_difference(ours: &str, reference: &str) -> String {
        let mut out = String::new();
        for (n, (a, b)) in ours.lines().zip(reference.lines()).enumerate() {
            if a != b {
                out.push_str(&format!("line {n}\n  ours: {a:.300}\n  ref : {b:.300}\n"));
                if out.len() > 1200 {
                    break;
                }
            }
        }
        out
    }

    /// Splits at arbitrary byte offsets: escape sequences, control strings and
    /// the parser's state after each call have to match the reference.
    #[test]
    fn matches_reference_on_random_ascii_output() {
        for seed in 1..=24u64 {
            let bytes = corpus(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 120, false);
            compare(
                &every_split(&bytes, bytes.len()),
                &format!("seed {seed} whole"),
            );
            for chunk in [1, 2, 3, 7, 16, 64] {
                compare(
                    &every_split(&bytes, chunk),
                    &format!("seed {seed} chunk {chunk}"),
                );
            }
        }
    }

    /// Multi-byte text, fed in pieces that always end on a character boundary.
    ///
    /// Splitting *inside* a character is excluded on purpose: `vte` drops the
    /// characters that follow such a character within the same call (it prints
    /// only the first one and still consumes the whole valid prefix), so the
    /// two parsers disagree there by design. `keeps_text_after_a_split_character`
    /// pins our behaviour for that case.
    #[test]
    fn matches_reference_on_random_utf8_output() {
        for seed in 1..=16u64 {
            let bytes = corpus(seed.wrapping_mul(0x2545_f491_4f6c_dd1d), 120, true);
            compare(
                &char_splits(&bytes, bytes.len()),
                &format!("seed {seed} whole"),
            );
            for chunk in [2, 5, 9, 40] {
                compare(
                    &char_splits(&bytes, chunk),
                    &format!("seed {seed} chunk {chunk}"),
                );
            }
        }
    }

    /// Parse throughput only: no PTY, no render, no locking. Run with
    /// `cargo test -p axo-term --release --lib throughput -- --ignored --nocapture`.
    #[test]
    #[ignore = "reports numbers, asserts nothing"]
    fn throughput() {
        use std::time::Instant;
        let pure_print: Vec<u8> = (0..4 << 20)
            .map(|i| {
                if i % 141 == 140 {
                    b'\n'
                } else {
                    b'a' + (i % 26) as u8
                }
            })
            .collect();
        let pure_csi: Vec<u8> = (0..256 << 10)
            .flat_map(|i: usize| {
                format!("\x1b[{};{}H\x1b[1;3{}m", i % 28 + 1, i % 141 + 1, i % 8).into_bytes()
            })
            .collect();
        // The way real styled output looks: segments written left to right,
        // a few colours per line, like `ls --color` or a compiler.
        let mut styled = Vec::new();
        for line in 0..40_000 {
            styled.extend_from_slice(b"\x1b[K");
            for segment in 0..6 {
                styled.extend_from_slice(
                    format!(
                        "\x1b[{}m",
                        [0, 1, 32, 33, 38, 34, 36, 90][(line + segment) % 8]
                    )
                    .as_bytes(),
                );
                styled.extend_from_slice(b"segment-text-here  ");
            }
            styled.extend_from_slice(b"\x1b[0m\r\n");
        }
        for (name, bytes) in [
            ("printable+lf", pure_print),
            ("csi-only", pure_csi),
            ("styled-lines", styled),
            ("mixed-ascii", corpus(0x1234_5678_9abc_def0, 4000, false)),
            ("mixed-utf8", corpus(0x1234_5678_9abc_def0, 4000, true)),
        ] {
            let reps = (64 << 20) / bytes.len().max(1);
            let mib = (bytes.len() * reps) as f64 / (1024.0 * 1024.0);

            let mut term = Term::new(141, 28, 1000);
            let start = Instant::now();
            for _ in 0..reps {
                term.advance(&bytes);
            }
            let ours = start.elapsed().as_secs_f64();

            let mut term = Term::new(141, 28, 1000);
            let mut parser = vte::Parser::new();
            let start = Instant::now();
            for _ in 0..reps {
                parser.advance(&mut Reference(&mut term), &bytes);
            }
            let theirs = start.elapsed().as_secs_f64();

            println!(
                "{name}: hand-written {:.1} MiB/s, vte {:.1} MiB/s, {:.2}x",
                mib / ours,
                mib / theirs,
                theirs / ours
            );
        }
    }

    /// A character cut in half by the read boundary is not lost, and neither is
    /// the text that follows it in the completing call.
    #[test]
    fn keeps_text_after_a_split_character() {
        let (cols, rows) = (24, 8);
        let mut term = Term::new(cols, rows, 32);
        term.advance(b"\xc3"); // the first byte of `å` alone
        term.advance(b"\xa5hi");
        let mut text = String::new();
        term.grid().row(0).text(0, cols, &mut text);
        assert_eq!(text, "åhi");

        // The same for a four-byte character split in three places.
        let mut term = Term::new(cols, rows, 32);
        term.advance(b"\xf0\x9f");
        term.advance(b"\x99");
        term.advance(b"\x82!");
        let mut text = String::new();
        term.grid().row(0).text(0, cols, &mut text);
        assert_eq!(text, "🙂!");
    }
}
