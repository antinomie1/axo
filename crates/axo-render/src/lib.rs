//! Terminal renderers behind one [`Renderer`] trait.
//!
//! A terminal block owns a surface of a renderer and asks it to draw the
//! terminal into it, then paints the surface into its aegle scene. Callers only
//! see the trait, so another backend (wgpu, CPU) plugs in without API changes.
//! Shared, backend-neutral pieces live here: glyph rasterization through the
//! aegle text stack ([`glyphs`]), the cell-to-quad layout ([`quads`]) and the
//! terminal color system ([`palette`]).

mod boxdraw;
pub mod glyphs;
pub mod palette;
pub mod quads;
// Vulkan is the GPU path on Linux and Windows; macOS has no ash backend in
// aegle yet, so only these targets compile the renderer.
#[cfg(all(feature = "vulkan", any(target_os = "linux", target_os = "windows")))]
pub mod vulkan;

use aegle::ui::scene::{Rect, SceneBuilder};
pub use axo_term::CursorShape;
use axo_term::Term;

pub use palette::Palette;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T = ()> = std::result::Result<T, Error>;

/// An sRGB color.
pub type Rgb = [u8; 3];

/// The terminal font.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// A family name list as for CSS, such as `"JetBrains Mono, monospace"`.
    pub family: String,
    /// Size in logical pixels.
    pub size: f32,
    /// Line height as a multiple of the font's natural line height.
    pub line_height: f32,
}

/// Cell size and baseline in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
    /// Baseline distance from the cell top.
    pub baseline: i32,
}

/// Input-method composition the child has not received yet, in UTF-8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preedit<'a> {
    /// Temporary text, drawn at the cursor in place of the cells it covers.
    pub text: &'a str,
    /// Fixed and active caret endpoints within `text`; `None` hides the caret.
    pub cursor: Option<(usize, usize)>,
}

/// Everything a renderer reads to draw one terminal.
pub struct Frame<'a> {
    pub term: &'a Term,
    pub palette: &'a Palette,
    /// The configured cursor; the application can override it (DECSCUSR).
    pub cursor: CursorShape,
    /// Unfocused terminals show a hollow cursor.
    pub focused: bool,
    /// Inner padding in device pixels.
    pub padding: u32,
    /// Composition shown at the cursor; the terminal receives it only on commit.
    pub preedit: Option<Preedit<'a>>,
}

/// A renderer's handle for one terminal's drawing target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceId(pub u32);

/// A terminal renderer. All calls happen on the UI thread.
pub trait Renderer {
    /// Selects the font at `scale` device pixels per logical pixel and returns
    /// the resulting cell size. Every surface redraws with it.
    fn set_font(&mut self, font: &FontSpec, scale: f32) -> Result<CellSize>;
    /// The current cell size.
    fn cell_size(&self) -> CellSize;
    fn create_surface(&mut self) -> Result<SurfaceId>;
    /// Releases a surface; its id is invalid afterwards.
    fn destroy_surface(&mut self, surface: SurfaceId);
    /// Draws `frame` into `surface` at `size` device pixels.
    fn render(&mut self, surface: SurfaceId, size: [u32; 2], frame: &Frame<'_>) -> Result;
    /// Records the surface's latest image into an aegle scene over `rect`
    /// (logical coordinates). Called from a canvas painter.
    fn paint(&self, surface: SurfaceId, builder: &mut SceneBuilder, rect: Rect);
}
