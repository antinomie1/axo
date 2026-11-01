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
    assert!(row.is_wide(0));
    assert!(row.style_at(0).flags.has(Flags::BOLD));
    assert_eq!(row.style_at(0).fg, Color::Rgb(1, 2, 3));
    assert!(row.is_spacer(1));
    assert_eq!(row.ch(2), 'x');
    assert_eq!(row.style_at(2).fg, Color::Default);
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
    assert!(term.grid().row(0).is_wide(0));
    term.advance(b"\x1b[1;2HY");
    assert_eq!(line(&term, 0), " Yx");
    assert!(!term.grid().row(0).is_wide(0));
    assert!(!term.grid().row(0).is_spacer(1));
    // Insert mode still shifts the line for ASCII.
    let mut term = Term::new(6, 1, 0);
    term.advance(b"\x1b[4habc\r\x1b[1;2HZ");
    assert_eq!(line(&term, 0), "aZbc");
    // The active SGR still applies to ASCII written through the fast path.
    let mut term = Term::new(6, 1, 0);
    term.advance(b"\x1b[1;38;5;196mA\x1b[0mB");
    let row = term.grid().row(0);
    assert!(row.style_at(0).flags.has(Flags::BOLD));
    assert_eq!(row.style_at(0).fg, Color::Indexed(196));
    assert_eq!(row.style_at(1).fg, Color::Default);
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

/// A terminal of `cols` × `rows` cells of 10×20 pixels.
fn pixel_term(cols: usize, rows: usize, scrollback: usize) -> Term {
    let mut term = Term::new(cols, rows, scrollback);
    term.set_cell_size(10, 20);
    term
}

/// A solid sixel image of `width` × `height` pixels, in whole bands.
fn sixel(width: usize, height: usize) -> Vec<u8> {
    let band = format!("#1!{width}~");
    let bands = vec![band; height.div_ceil(6)].join("-");
    format!("\x1bP0;1q#1;2;100;0;0{bands}\x1b\\").into_bytes()
}

/// The image's placements as `(column, line relative to the screen, crop)`.
fn placements(term: &Term) -> Vec<(usize, i64, [u32; 4])> {
    let base = term.grid().base();
    let images = term.grid().images();
    images
        .iter()
        .map(|p| (p.col, p.line - base, p.crop))
        .collect()
}

#[test]
fn a_sixel_sits_at_the_cursor_and_moves_it_below() {
    let mut term = pixel_term(20, 10, 10);
    term.advance(b"\x1b[2;3H");
    term.advance(&sixel(25, 30));
    assert_eq!(placements(&term), [(2, 1, [0, 0, 25, 30])]);
    // Two rows tall: the cursor keeps its column on the line after them.
    assert_eq!((term.cursor.x, term.cursor.y), (2, 3));
    // Beside it, when the mode asks.
    term.advance(b"\x1b[?8452h\x1b[8;1H");
    term.advance(&sixel(25, 30));
    assert_eq!((term.cursor.x, term.cursor.y), (3, 8));
    // Pinned to the top left without moving anything in display mode.
    term.advance(b"\x1b[?8452l\x1b[?80h\x1b[5;5H");
    term.advance(&sixel(5, 6));
    assert_eq!(placements(&term).last().unwrap().0, 0);
    assert_eq!(placements(&term).last().unwrap().1, 0);
    assert_eq!((term.cursor.x, term.cursor.y), (4, 4));
}

#[test]
fn an_image_scrolls_with_its_lines_and_leaves_with_them() {
    let mut term = pixel_term(10, 4, 3);
    term.advance(&sixel(10, 18));
    term.advance(b"\n\n\n\n");
    assert_eq!(placements(&term)[0].1, -2, "two lines into history");
    assert_eq!(term.grid().history_len(), 2);
    // The window on history shows it again, at the same line.
    term.scroll_display(2);
    assert_eq!(term.view_line(0), -2);
    // Enough output evicts the line, and the image goes with it.
    term.advance(&b"\n".repeat(300));
    assert!(term.grid().images().is_empty());
}

#[test]
fn a_full_screen_scroll_of_the_alternate_screen_moves_images_too() {
    let mut term = pixel_term(10, 4, 3);
    term.advance(b"\x1b[?1049h\x1b[2;1H");
    term.advance(&sixel(10, 18));
    assert_eq!(placements(&term)[0].1, 1);
    term.advance(b"\x1b[S");
    assert_eq!(placements(&term)[0].1, 0);
    term.advance(b"\x1b[T");
    assert_eq!(placements(&term)[0].1, 1);
    term.advance(b"\x1b[2S");
    assert!(placements(&term).is_empty());
    // Screens keep their own images.
    term.advance(&sixel(10, 18));
    term.advance(b"\x1b[?1049l");
    assert!(placements(&term).is_empty());
    term.advance(b"\x1b[?1049h");
    assert!(placements(&term).is_empty(), "entering clears the screen");
}

#[test]
fn a_scroll_region_leaves_the_lines_under_it_in_place() {
    let mut term = pixel_term(10, 4, 10);
    // An image on the last line, under a region that feeds history.
    term.advance(b"\x1b[4;1H\x1b_Ga=T,f=24,s=1,v=1,C=1;AAAA\x1b\\");
    term.advance(b"\x1b[1;3r\x1b[3;1H\n\n");
    assert_eq!(term.grid().history_len(), 2);
    assert_eq!(placements(&term)[0].1, 3, "still on the same row");
}

#[test]
fn text_replaces_the_sixel_cells_it_lands_on() {
    let mut term = pixel_term(10, 4, 0);
    term.advance(&sixel(30, 18));
    assert_eq!(term.grid().images().iter().count(), 1);
    term.advance(b"\x1b[1;2Hx");
    let crops: Vec<[u32; 4]> = placements(&term).iter().map(|p| p.2).collect();
    assert!(crops.contains(&[0, 0, 10, 18]) && crops.contains(&[20, 0, 10, 18]));
    assert_eq!(crops.len(), 2);
    // Non-ASCII text and erasing do the same.
    term.advance("\x1b[1;1H中".as_bytes());
    assert!(placements(&term).iter().all(|p| p.2[0] != 0));
    term.advance(b"\x1b[1;3H\x1b[K");
    assert!(term.grid().images().is_empty());
}

#[test]
fn clearing_the_screen_takes_what_is_on_it() {
    let mut term = pixel_term(10, 2, 10);
    let kitty = b"\x1b_Ga=T,f=24,s=1,v=1,C=1;AAAA\x1b\\";
    term.advance(kitty);
    term.advance(b"\n\n\n");
    term.advance(b"\x1b[H");
    term.advance(kitty);
    term.advance(&sixel(10, 6));
    assert_eq!(term.grid().images().iter().count(), 3);
    term.advance(b"\x1b[2J");
    assert_eq!(term.grid().images().iter().count(), 1, "history stays");
    term.advance(b"\x1b[3J");
    assert!(term.grid().images().is_empty());
}

#[test]
fn cancelled_and_empty_sixels_show_nothing() {
    let mut term = pixel_term(10, 4, 0);
    term.advance(b"\x1bP0;1q#1;2;100;0;0~~\x18");
    assert!(term.grid().images().is_empty());
    term.advance(b"ok\x1bPq\x1b\\!");
    assert_eq!(line(&term, 0), "ok!");
    // Other strings pass through without reaching the screen.
    term.advance(b"\x1b_Xnope\x1b\\\x1bP$qm\x1b\\.");
    assert_eq!(line(&term, 0), "ok!.");
    assert!(term.grid().images().is_empty());
}

#[test]
fn a_reset_drops_images_but_not_the_cell_size() {
    let mut term = pixel_term(10, 4, 0);
    term.advance(&sixel(10, 18));
    term.advance(b"\x1bc");
    assert!(term.grid().images().is_empty());
    term.advance(b"\x1b[16t");
    assert_eq!(term.take_responses().unwrap(), b"\x1b[6;20;10t");
}

#[test]
fn graphics_are_reported_to_applications() {
    let mut term = pixel_term(80, 24, 0);
    let mut ask = |query: &[u8]| {
        term.advance(query);
        String::from_utf8(term.take_responses().unwrap()).unwrap()
    };
    assert_eq!(ask(b"\x1b[c"), "\x1b[?62;4;22c");
    assert_eq!(ask(b"\x1b[?1;1;0S"), "\x1b[?1;0;1024S");
    assert_eq!(ask(b"\x1b[?2;1;0S"), "\x1b[?2;0;800;480S");
    assert_eq!(ask(b"\x1b[?3;1;0S"), "\x1b[?3;1;0S");
    assert_eq!(ask(b"\x1b[14t"), "\x1b[4;480;800t");
    assert_eq!(ask(b"\x1b[16t"), "\x1b[6;20;10t");
    assert_eq!(ask(b"\x1b[18t"), "\x1b[8;24;80t");
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
            let len = 1 + rng.below(40);
            for _ in 0..len {
                out.push(b' ' + rng.below(95) as u8);
            }
        }
        35..=44 if utf8 => {
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
            out.extend_from_slice(b"\x1b(0lqk\x1b(B");
        }
        45..=57 => {
            out.push(*rng.pick(b"\n\r\t\x08\x0e\x0f\x07\x00\x18\x1a\x7f"));
        }
        58..=80 => {
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
            out.push(0x1b);
            if rng.below(2) == 0 {
                out.push(*rng.pick(b"()#"));
            }
            out.push(*rng.pick(b"78DEHMc=>\\*"));
        }
        _ => {
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

#[test]
fn parses_random_ascii_output() {
    for seed in 1..=24u64 {
        let bytes = corpus(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 120, false);
        let mut term = Term::new(24, 8, 32);
        for piece in bytes.chunks(bytes.len().max(1)) {
            term.advance(piece);
        }
        for chunk in [1, 2, 3, 7, 16, 64] {
            let mut term = Term::new(24, 8, 32);
            for piece in bytes.chunks(chunk) {
                term.advance(piece);
            }
        }
    }
}

#[test]
fn parses_random_utf8_output() {
    for seed in 1..=16u64 {
        let bytes = corpus(seed.wrapping_mul(0x2545_f491_4f6c_dd1d), 120, true);
        let mut term = Term::new(24, 8, 32);
        for piece in char_splits(&bytes, bytes.len()) {
            term.advance(piece);
        }
        for chunk in [2, 5, 9, 40] {
            let mut term = Term::new(24, 8, 32);
            for piece in char_splits(&bytes, chunk) {
                term.advance(piece);
            }
        }
    }
}

#[test]
fn keeps_text_after_a_split_character() {
    let (cols, rows) = (24, 8);
    let mut term = Term::new(cols, rows, 32);
    term.advance(b"\xc3"); // the first byte of `å` alone
    term.advance(b"\xa5hi");
    let mut text = String::new();
    term.grid().row(0).text(0, cols, &mut text);
    assert_eq!(text, "åhi");

    let mut term = Term::new(cols, rows, 32);
    term.advance(b"\xf0\x9f");
    term.advance(b"\x99");
    term.advance(b"\x82!");
    let mut text = String::new();
    term.grid().row(0).text(0, cols, &mut text);
    assert_eq!(text, "🙂!");
}

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

        println!("{name}: {:.1} MiB/s", mib / ours);
    }
}
