//! State every block reads: the renderer, settings and the UI-thread proxy.

use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use aegle::app::UiProxy;
use axo_render::{Palette, Renderer};

use crate::{config::Config, keys::Action, menu::MenuCommand};

/// Messages routed to the UI thread outside any window or UI borrow.
#[derive(Clone, Copy, Debug)]
pub enum Msg {
    /// New output for terminal block `id`.
    Wake(u64),
    /// Block `id` took keyboard focus.
    Focus(u64),
    /// The shell of terminal block `id` exited.
    Exit(u64),
    /// An application shortcut matched in `on_key`.
    Action(Action),
    /// A command chosen in block `id`'s right-click menu.
    Menu(u64, MenuCommand),
}

pub struct Shared {
    pub renderer: RefCell<Box<dyn Renderer>>,
    pub config: RefCell<Config>,
    pub palette: RefCell<Palette>,
    /// Device pixels per logical pixel.
    pub scale: Cell<f32>,
    /// Whether the window was created with alpha; opacity applies only then.
    pub transparent: bool,
    pub proxy: OnceCell<UiProxy<Msg>>,
    /// The block that last took focus.
    pub focused: Cell<u64>,
}

pub type SharedRef = Rc<Shared>;
