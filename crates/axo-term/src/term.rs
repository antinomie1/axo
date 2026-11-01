//! The terminal state machine: VT sequences applied to two screens.

use std::sync::Arc;

use unicode_width::UnicodeWidthChar;

use crate::{
    cell::{Color, Flags, Style},
    grid::Grid,
    image::{Images, MAX_SIDE, Pixels, Placement},
    kitty::Pending,
    parser::{Params, Parser},
    selection::{Point, Range, Selection},
    sixel::{self, Decoder},
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
    /// A sixel draws from the top left and scrolls nothing (DECSDM, 80).
    pub sixel_display: bool,
    /// The cursor ends beside a sixel instead of below it (8452).
    pub sixel_cursor_right: bool,
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
            sixel_display: false,
            sixel_cursor_right: false,
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
    /// The kitty graphics transfer still receiving chunks.
    pub(crate) kitty: Option<Box<Pending>>,
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
            kitty: None,
        }
    }

    /// Applies output bytes from the child process.
    pub fn advance(&mut self, bytes: &[u8]) {
        // The parser needs `&mut self` for the terminal it drives, so it is
        // moved out for the call; the default left behind allocates nothing.
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
            let grid = self.grid_mut();
            grid.punch(y, start, start + len);
            grid.row_mut(y).write_ascii(start, &run[i..i + len], pen);
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

    /// Tells the terminal the cell size in pixels, which the graphics
    /// protocols and pixel size reports are measured in.
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        if width != 0 && height != 0 {
            self.primary.images.set_cell([width, height]);
            self.alt.images.set_cell([width, height]);
        }
    }

    #[inline]
    pub(crate) fn images(&self) -> &Images {
        self.grid().images()
    }
    #[inline]
    pub(crate) fn images_mut(&mut self) -> &mut Images {
        &mut self.grid_mut().images
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
        let grid = self.grid_mut();
        grid.punch(y, from, to);
        grid.row_mut(y).clear(from, to, &pen);
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
                self.alt.images.clear();
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
        let [width, height] = self.images().cell();
        *self = Term {
            report_colors,
            ..fresh
        };
        self.set_cell_size(width, height);
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
            let grid = self.grid_mut();
            grid.punch(y, x, x + 1);
            grid.row_mut(y).write_ascii(x, &[c as u8], pen);
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
        let (pen, insert) = (self.cursor.pen, self.modes.insert);
        let grid = self.grid_mut();
        grid.punch(y, x, if insert { cols } else { x + width });
        if insert {
            grid.row_mut(y).insert_cells(x, width, &pen);
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

    fn sgr(&mut self, params: &Params) {
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
            80 => self.modes.sixel_display = on,
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
            8452 => self.modes.sixel_cursor_right = on,
            _ => {}
        }
    }

    pub(crate) fn respond(&mut self, bytes: impl AsRef<[u8]>) {
        self.responses.extend_from_slice(bytes.as_ref());
    }

    /// Moves the cursor past an image that starts at it: to the right of its
    /// last row, scrolling when that row is below the screen.
    pub(crate) fn pass_image(&mut self, cols: usize, rows: usize) {
        let x = self.cursor.x + cols;
        for _ in 1..rows {
            self.linefeed();
        }
        if x < self.cols() {
            self.cursor.x = x;
        } else {
            self.cursor.x = 0;
            self.linefeed();
        }
    }

    /// A decoder for a sixel image starting now. Unpainted pixels show the
    /// default background, which the palette reports here, unless transparent.
    pub(crate) fn sixel_decoder(&self, transparent: bool) -> Box<Decoder> {
        let [r, g, b] = self.report_colors[1];
        let alpha = if transparent { 0 } else { 255 };
        Decoder::new(u32::from_ne_bytes([r, g, b, alpha]))
    }

    /// Shows a finished sixel image. It is cell content: at the cursor, moving
    /// the cursor below itself, unless display mode pins it to the top left.
    pub(crate) fn sixel(&mut self, pixels: Pixels) {
        let display = self.modes.sixel_display;
        let (col, y) = match display {
            true => (0, 0),
            false => (self.cursor.x, self.cursor.y),
        };
        let line = self.grid().base() + y as i64;
        let placement = Placement::whole(Arc::new(pixels), line, col);
        let [cols, rows] = placement.span(self.images().cell());
        self.images_mut().add_sixel(placement);
        if display {
            return;
        }
        if self.modes.sixel_cursor_right {
            self.pass_image(cols, rows);
        } else {
            for _ in 0..rows {
                self.linefeed();
            }
            self.cursor.x = col;
        }
    }

    /// `ED 2`: what was on the screen goes; what scrolled off stays. Images
    /// stay out of line here and below: `csi` is hot, and this is not.
    #[cold]
    fn clear_visible_images(&mut self) {
        let (base, cell) = (self.grid().base(), self.images().cell());
        let on_screen = |placement: &Placement| placement.reaches(base, cell);
        self.images_mut().delete(on_screen, false, |_| false);
    }

    /// Answers `XTSMGRAPHICS`: every attribute is read-only here, so a request
    /// to set one reports the value it stays at.
    #[cold]
    fn graphics_attribute(&mut self, item: u16) {
        let [cell_width, cell_height] = self.images().cell();
        let geometry = |cells: usize, cell: u32| (cells * cell as usize).min(MAX_SIDE);
        let reply = match item {
            1 => format!("\x1b[?1;0;{}S", sixel::REGISTERS),
            2 => format!(
                "\x1b[?2;0;{};{}S",
                geometry(self.cols(), cell_width),
                geometry(self.rows(), cell_height)
            ),
            // ReGIS, and anything else: not there.
            _ => format!("\x1b[?{item};1;0S"),
        };
        self.respond(reply);
    }

    /// Answers the `XTWINOPS` size reports: text area in pixels, cell in
    /// pixels, text area in cells.
    #[cold]
    fn report_size(&mut self, kind: u16) {
        let (cols, rows) = (self.cols(), self.rows());
        let [width, height] = self.images().cell().map(|v| v as usize);
        match kind {
            14 => self.respond(format!("\x1b[4;{};{}t", rows * height, cols * width)),
            16 => self.respond(format!("\x1b[6;{height};{width}t")),
            18 => self.respond(format!("\x1b[8;{rows};{cols}t")),
            _ => {}
        }
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
    pub(crate) fn csi(
        &mut self,
        params: &Params,
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
                let grid = self.grid_mut();
                grid.punch(y, x, cols);
                grid.row_mut(y).insert_cells(x, n, &pen);
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
                        self.clear_visible_images();
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
                let grid = self.grid_mut();
                grid.punch(y, x, cols);
                grid.row_mut(y).remove_cells(x, n, &pen);
            }
            (b'S', []) => self.scroll_up_region(self.top, params.get(0, 1)),
            (b'S', [b'?']) => self.graphics_attribute(params.raw(0)),
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
            // A VT220 with sixel graphics and ANSI color.
            (b'c', []) => self.respond(b"\x1b[?62;4;22c"),
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
            (b't', []) => self.report_size(params.raw(0)),
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
