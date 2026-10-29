//! Axo terminal core: a pseudo-terminal session, VT parsing into a grid with
//! scrollback, selection, and input encoding. No GUI or renderer dependency.

mod cell;
mod grid;
mod input;
mod pty;
mod selection;
mod session;
mod term;

pub use cell::{Cell, Color, Flags, Row};
pub use grid::Grid;
pub use input::{Key, Mods, MouseAction, MouseButton, encode_key, encode_mouse};
pub use pty::WinSize;
pub use selection::{Point, Range, Selection, SelectionKind};
pub use session::{Event, Options, Session};
pub use term::{Cursor, CursorShape, Modes, MouseMode, Term};
