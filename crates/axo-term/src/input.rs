//! Keyboard and mouse input encoded as the bytes a terminal application reads.

use crate::term::Modes;

/// Keys with a terminal encoding, independent of any GUI toolkit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 through F12.
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    /// The xterm modifier parameter: 1 plus shift, alt and control bits.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Appends the bytes for `key` (with `text` the platform translated) to `out`.
/// Returns false when the key has no encoding.
pub fn encode_key(key: Key, text: &str, mods: Mods, modes: &Modes, out: &mut Vec<u8>) -> bool {
    let m = mods.param();
    let csi_tilde = |out: &mut Vec<u8>, n: u8| match m {
        1 => out.extend_from_slice(format!("\x1b[{n}~").as_bytes()),
        _ => out.extend_from_slice(format!("\x1b[{n};{m}~").as_bytes()),
    };
    // Cursor-style keys: SS3 in application mode, CSI 1;m with modifiers.
    let cursor = |out: &mut Vec<u8>, c: char| match (m, modes.app_cursor) {
        (1, true) => out.extend_from_slice(format!("\x1bO{c}").as_bytes()),
        (1, false) => out.extend_from_slice(format!("\x1b[{c}").as_bytes()),
        _ => out.extend_from_slice(format!("\x1b[1;{m}{c}").as_bytes()),
    };
    let alt = |out: &mut Vec<u8>| {
        if mods.alt {
            out.push(0x1b);
        }
    };
    match key {
        Key::Up => cursor(out, 'A'),
        Key::Down => cursor(out, 'B'),
        Key::Right => cursor(out, 'C'),
        Key::Left => cursor(out, 'D'),
        Key::Home => cursor(out, 'H'),
        Key::End => cursor(out, 'F'),
        Key::Insert => csi_tilde(out, 2),
        Key::Delete => csi_tilde(out, 3),
        Key::PageUp => csi_tilde(out, 5),
        Key::PageDown => csi_tilde(out, 6),
        Key::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            match m {
                1 => out.extend_from_slice(format!("\x1bO{c}").as_bytes()),
                _ => out.extend_from_slice(format!("\x1b[1;{m}{c}").as_bytes()),
            }
        }
        Key::F(n @ 5..=12) => csi_tilde(out, [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        Key::F(_) => return false,
        Key::Enter => {
            alt(out);
            out.push(b'\r');
        }
        Key::Tab if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => {
            alt(out);
            out.push(b'\t');
        }
        Key::Escape => {
            alt(out);
            out.push(0x1b);
        }
        Key::Backspace => {
            alt(out);
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Char(c) => {
            alt(out);
            match (mods.ctrl, ctrl_byte(c)) {
                (true, Some(byte)) => out.push(byte),
                _ if !text.is_empty() => out.extend_from_slice(text.as_bytes()),
                _ => {
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }
    true
}

/// A mouse button or wheel direction for reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion without a held button.
    None,
}

/// What happened to the button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Appends a mouse report at cell `(col, row)` in the requested protocol.
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
    modes: &Modes,
    out: &mut Vec<u8>,
) {
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    code += 4 * mods.shift as u32 + 8 * mods.alt as u32 + 16 * mods.ctrl as u32;
    if modes.mouse_sgr {
        let end = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        out.extend_from_slice(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).as_bytes());
    } else {
        if action == MouseAction::Release {
            code = 3 + (code & !3);
        }
        let clamp = |v: usize| (32 + 1 + v).min(255) as u8;
        out.extend_from_slice(&[
            0x1b,
            b'[',
            b'M',
            (32 + code).min(255) as u8,
            clamp(col),
            clamp(row),
        ]);
    }
}
