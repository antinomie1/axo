use axo_term::{Color, Flags, Point, Selection, SelectionKind, Term};

fn line(term: &Term, y: isize) -> String {
    let mut out = String::new();
    let row = term.grid().line(y).unwrap();
    row.text(0, usize::MAX, &mut out);
    out
}

#[test]
fn prints_wraps_and_scrolls_into_history() {
    let mut term = Term::new(5, 2, 100);
    term.advance(b"abcdefg\r\nxy\r\nz");
    assert_eq!(line(&term, -2), "abcde");
    assert_eq!(line(&term, -1), "fg");
    assert_eq!(line(&term, 0), "xy");
    assert_eq!(line(&term, 1), "z");
    // The soft-wrapped row joins its continuation without a newline.
    term.start_selection(Selection::new(
        Point { line: -2, col: 0 },
        SelectionKind::Simple,
    ));
    term.extend_selection(Point { line: -1, col: 1 });
    assert_eq!(term.selection_text().as_deref(), Some("abcdefg"));
}

#[test]
fn sgr_wide_chars_and_erase() {
    let mut term = Term::new(10, 3, 0);
    term.advance("\x1b[1;38;2;1;2;3m中\x1b[0mx".as_bytes());
    let row = term.grid().row(0);
    assert!(row.cells[0].flags.has(Flags::WIDE | Flags::BOLD));
    assert_eq!(row.cells[0].fg, Color::Rgb(1, 2, 3));
    assert!(row.cells[1].flags.has(Flags::SPACER));
    assert_eq!(row.cells[2].ch, 'x');
    assert_eq!(row.cells[2].fg, Color::Default);
    // Overwriting the spacer clears the wide character.
    term.advance(b"\x1b[1;2Hy");
    assert_eq!(line(&term, 0), " yx");
    term.advance(b"\x1b[2J");
    assert_eq!(line(&term, 0), "");
}

#[test]
fn alternate_screen_restores_primary() {
    let mut term = Term::new(8, 2, 10);
    term.advance(b"shell");
    term.advance(b"\x1b[?1049h\x1b[Hvim");
    assert!(term.alt_screen());
    assert_eq!(line(&term, 0), "vim");
    term.advance(b"\x1b[?1049l");
    assert_eq!(line(&term, 0), "shell");
    assert_eq!(term.cursor.x, 5);
}

#[test]
fn reports_cursor_position() {
    let mut term = Term::new(8, 4, 0);
    term.advance(b"\x1b[3;4H\x1b[6n");
    assert_eq!(term.take_responses().unwrap(), b"\x1b[3;4R");
}

#[test]
fn resize_keeps_cursor_line() {
    let mut term = Term::new(6, 4, 10);
    term.advance(b"1\r\n2\r\n3\r\n4");
    term.resize(6, 2);
    assert_eq!(line(&term, 1), "4");
    assert_eq!(term.cursor.y, 1);
    term.resize(6, 4);
    assert_eq!(line(&term, 0), "1");
}

#[test]
fn encodes_keys_and_mouse() {
    use axo_term::{Key, Modes, Mods, MouseAction, MouseButton, encode_key, encode_mouse};
    let mut modes = Modes::default();
    let enc = |key, text: &str, mods: Mods, modes: &Modes| {
        let mut out = Vec::new();
        encode_key(key, text, mods, modes, &mut out);
        out
    };
    let ctrl = Mods {
        ctrl: true,
        ..Mods::default()
    };
    assert_eq!(enc(Key::Char('c'), "", ctrl, &modes), b"\x03");
    assert_eq!(
        enc(
            Key::Char('x'),
            "x",
            Mods {
                alt: true,
                ..Mods::default()
            },
            &modes
        ),
        b"\x1bx"
    );
    assert_eq!(enc(Key::Up, "", Mods::default(), &modes), b"\x1b[A");
    assert_eq!(enc(Key::Right, "", ctrl, &modes), b"\x1b[1;5C");
    assert_eq!(enc(Key::F(5), "", Mods::default(), &modes), b"\x1b[15~");
    assert_eq!(
        enc(
            Key::Tab,
            "",
            Mods {
                shift: true,
                ..Mods::default()
            },
            &modes
        ),
        b"\x1b[Z"
    );
    modes.app_cursor = true;
    assert_eq!(enc(Key::Up, "", Mods::default(), &modes), b"\x1bOA");
    modes.mouse_sgr = true;
    let mut out = Vec::new();
    encode_mouse(
        MouseButton::Left,
        MouseAction::Release,
        4,
        2,
        Mods::default(),
        &modes,
        &mut out,
    );
    assert_eq!(out, b"\x1b[<0;5;3m");
}

#[test]
fn scroll_regions_and_line_edits_keep_history_intact() {
    let mut term = Term::new(6, 4, 10);
    term.advance(b"a\r\nb\r\nc\r\nd");
    // Region 2..=3: scrolling inside it must not touch history or row 0.
    term.advance(b"\x1b[2;3r\x1b[3;1H\nX");
    assert_eq!(term.grid().history_len(), 0);
    assert_eq!(line(&term, 0), "a");
    assert_eq!(line(&term, 1), "c");
    assert_eq!(line(&term, 2), "X");
    assert_eq!(line(&term, 3), "d");
    // Reverse index at the region top scrolls the region down.
    term.advance(b"\x1b[2;1H\x1bM");
    assert_eq!(line(&term, 1), "");
    assert_eq!(line(&term, 2), "c");
    // Insert and delete lines at row 1 within the region.
    term.advance(b"\x1b[r\x1b[2;1HY\x1b[L");
    assert_eq!(line(&term, 1), "");
    assert_eq!(line(&term, 2), "Y");
    term.advance(b"\x1b[M");
    assert_eq!(line(&term, 1), "Y");
    // A full-screen scroll still feeds history.
    term.advance(b"\x1b[4;1H\n\n");
    assert_eq!(term.grid().history_len(), 2);
    assert_eq!(line(&term, -2), "a");
}

#[test]
fn ascii_fast_path_matches_general_path() {
    // Wrapping at the last column defers the wrap until the next printable.
    let mut term = Term::new(4, 2, 10);
    term.advance(b"abcdef");
    assert_eq!(line(&term, 0), "abcd");
    assert_eq!(line(&term, 1), "ef");
    // Overwriting half of a wide character with ASCII clears its partner.
    let mut term = Term::new(6, 2, 10);
    term.advance("中x".as_bytes());
    assert!(term.grid().row(0).cells[0].flags.has(Flags::WIDE));
    term.advance(b"\x1b[1;2HY");
    assert_eq!(line(&term, 0), " Yx");
    assert!(!term.grid().row(0).cells[0].flags.has(Flags::WIDE));
    assert!(!term.grid().row(0).cells[1].flags.has(Flags::SPACER));
    // Insert mode still shifts the line for ASCII.
    let mut term = Term::new(6, 1, 0);
    term.advance(b"\x1b[4habc\r\x1b[1;2HZ");
    assert_eq!(line(&term, 0), "aZbc");
    // The active SGR still applies to ASCII written through the fast path.
    let mut term = Term::new(6, 1, 0);
    term.advance(b"\x1b[1;38;5;196mA\x1b[0mB");
    let row = term.grid().row(0);
    assert!(row.cells[0].flags.has(Flags::BOLD));
    assert_eq!(row.cells[0].fg, Color::Indexed(196));
    assert_eq!(row.cells[1].fg, Color::Default);
}

#[test]
fn osc_7_reports_the_shell_directory() {
    let mut term = Term::new(20, 2, 10);
    assert_eq!(term.cwd(), None);
    // xterm and most shells escape spaces as `%20`; the host is optional.
    term.advance(b"\x1b]7;file://host/home/me/my%20dir\x07");
    assert_eq!(term.cwd(), Some("/home/me/my dir"));
    // A later report replaces it, so tabs follow the shell.
    term.advance(b"\x1b]7;file://localhost/tmp\x1b\\");
    assert_eq!(term.cwd(), Some("/tmp"));
    // Windows shells send a drive through the same URI, with a leading slash.
    term.advance(b"\x1b]7;file:///C:/Users/me\x07");
    assert_eq!(term.cwd(), Some("C:/Users/me"));
    // The host names the machine the shell runs on, which is this one, so only
    // the path matters; another scheme is not a directory report at all.
    term.advance(b"\x1b]7;file://other/tmp\x07");
    assert_eq!(term.cwd(), Some("/tmp"));
    term.advance(b"\x1b]7;https://example.com/tmp\x07");
    assert_eq!(term.cwd(), Some("/tmp"));
}
