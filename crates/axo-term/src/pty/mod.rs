//! A pseudo-terminal with a child shell: `openpty` on Unix, a ConPTY pseudo
//! console on Windows.
//!
//! Only the target's implementation is compiled; everything above sees
//! [`Pty`], its [`Child`], its [`Console`] and the two files it exposes, so
//! `session` has no `cfg` of its own.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{Console, Pty, process_cwd, process_name};
#[cfg(windows)]
pub use windows::{Console, Pty, process_cwd, process_name};

/// The terminal size reported to the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinSize {
    /// Columns in cells.
    pub cols: u16,
    /// Rows in cells.
    pub rows: u16,
    /// Width in pixels, zero when unknown.
    pub width: u16,
    /// Height in pixels, zero when unknown.
    pub height: u16,
}
