//! Application shortcuts, matched before keys reach a block.

use aegle::ui::{Key, KeyEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseBlock,
    /// Side by side.
    SplitRight,
    /// Stacked.
    SplitDown,
    Copy,
    Paste,
    Settings,
    NextTab,
    PrevTab,
    GoTab(usize),
    Focus(Dir),
    FontSize(i8),
    /// Scrolls the focused terminal by pages; positive goes back.
    Scroll(isize),
}

/// | Keys | Action |
/// | --- | --- |
/// | Ctrl+Shift+T / W | new tab / close block |
/// | Ctrl+Shift+D / E | split right / down |
/// | Ctrl+Shift+C / V | copy / paste |
/// | Ctrl+, | settings |
/// | Ctrl+Tab, Ctrl+PageDown / Ctrl+Shift+Tab, Ctrl+PageUp | next / previous tab |
/// | Alt+1…9 | tab n |
/// | Alt+Arrow | focus the neighbouring block |
/// | Ctrl+= / - / 0 (or Ctrl+Shift) | font size up / down / reset |
/// | Shift+PageUp / PageDown | scroll history |
pub fn shortcut(event: &KeyEvent<'_>) -> Option<Action> {
    let m = event.modifiers;
    if m.meta {
        return None;
    }
    let (ctrl, shift, alt) = (m.control, m.shift, m.alt);
    Some(match event.key {
        Key::Character(c) if ctrl && !alt => match (shift, c.to_ascii_lowercase()) {
            (true, 't') => Action::NewTab,
            (true, 'w') => Action::CloseBlock,
            (true, 'd') => Action::SplitRight,
            (true, 'e') => Action::SplitDown,
            (true, 'c') => Action::Copy,
            (true, 'v') => Action::Paste,
            (false, ',') => Action::Settings,
            (_, '=' | '+') => Action::FontSize(1),
            (_, '-' | '_') => Action::FontSize(-1),
            (_, '0' | ')') => Action::FontSize(0),
            _ => return None,
        },
        Key::Character(c @ '1'..='9') if alt && !ctrl && !shift => {
            Action::GoTab(c as usize - '1' as usize)
        }
        Key::Tab if ctrl && !alt => match shift {
            true => Action::PrevTab,
            false => Action::NextTab,
        },
        Key::PageDown if ctrl && !shift && !alt => Action::NextTab,
        Key::PageUp if ctrl && !shift && !alt => Action::PrevTab,
        Key::PageUp if shift && !ctrl && !alt => Action::Scroll(1),
        Key::PageDown if shift && !ctrl && !alt => Action::Scroll(-1),
        Key::Left if alt && !ctrl && !shift => Action::Focus(Dir::Left),
        Key::Right if alt && !ctrl && !shift => Action::Focus(Dir::Right),
        Key::Up if alt && !ctrl && !shift => Action::Focus(Dir::Up),
        Key::Down if alt && !ctrl && !shift => Action::Focus(Dir::Down),
        _ => return None,
    })
}
