//! Axo terminal core: a pseudo-terminal session, VT parsing into a grid with
//! scrollback, selection, and input encoding. No GUI or renderer dependency.

mod cell;
mod grid;
mod image;
mod input;
mod kitty;
mod parser;
mod pty;
mod selection;
mod session;
mod sixel;
mod term;

pub use cell::{Color, Flags, Row, Style};
pub use grid::Grid;
pub use image::{Images, Pixels, Placement};
pub use input::{Key, Mods, MouseAction, MouseButton, encode_key, encode_mouse, quote_paths};
pub use pty::WinSize;
pub use selection::{Point, Range, Selection, SelectionKind};
pub use session::{Event, Options, Session};
pub use term::{Cursor, CursorShape, Modes, MouseMode, Term};
