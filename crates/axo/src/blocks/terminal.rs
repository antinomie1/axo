//! The terminal block: a canvas showing a renderer surface of one session.
//!
//! The canvas painter draws the terminal: it runs only when aegle repaints the
//! canvas (after output, input or a resize), at most once per frame, and
//! submits the renderer's GPU work ahead of aegle's frame.

use std::{
    cell::{OnceCell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};

use aegle::{
    prelude::*,
    ui::{
        Key as UiKey, KeyEvent, Modifiers, PointerButton, Size,
        scene::{Rect, SceneBuilder},
    },
    widgets::CanvasEvent,
};
use axo_render::{Frame, SurfaceId};
use axo_term::{
    Key, Mods, MouseAction, MouseButton, MouseMode, Options, Point as Cell, Selection,
    SelectionKind, Session, WinSize, encode_key, encode_mouse,
};

use super::{Block, next_id};
use crate::{
    config::Config,
    menu::BlockMenu,
    shared::{Msg, SharedRef},
};

pub struct Terminal {
    inner: Rc<Inner>,
}

struct Inner {
    id: u64,
    shared: SharedRef,
    session: Session,
    surface: SurfaceId,
    canvas: OnceCell<Canvas>,
    /// The right-click menu anchored to the canvas.
    menu: OnceCell<BlockMenu>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    grid: (u16, u16),
    focused: bool,
    selecting: bool,
    /// The last cell a drag reached, to skip redundant redraws.
    drag: (usize, usize),
    /// Fractional wheel lines carried between events.
    wheel: f32,
    /// Buttons reported to the application while mouse mode is on.
    reported: u8,
    title: String,
    /// Output arrived that no paint has drawn yet.
    pending: bool,
    /// A one-shot frame callback is waiting for the next presented frame.
    scheduled: bool,
    /// The right press of this batch already went to the application or
    /// opened the menu; the queued context menu request must not repeat it.
    suppress_menu: bool,
}

fn mods(m: Modifiers) -> Mods {
    Mods {
        shift: m.shift,
        ctrl: m.control,
        alt: m.alt,
    }
}

fn key(key: UiKey) -> Option<Key> {
    Some(match key {
        UiKey::Character(c) => Key::Char(c),
        UiKey::Enter => Key::Enter,
        UiKey::Tab => Key::Tab,
        UiKey::Escape => Key::Escape,
        UiKey::Backspace => Key::Backspace,
        UiKey::Delete => Key::Delete,
        UiKey::Left => Key::Left,
        UiKey::Right => Key::Right,
        UiKey::Up => Key::Up,
        UiKey::Down => Key::Down,
        UiKey::Home => Key::Home,
        UiKey::End => Key::End,
        UiKey::PageUp => Key::PageUp,
        UiKey::PageDown => Key::PageDown,
        UiKey::Function(n) => Key::F(n),
        _ => return None,
    })
}

impl Terminal {
    pub fn new(parent: &Container, shared: SharedRef, cwd: Option<PathBuf>) -> Result<Rc<Self>> {
        let id = next_id();
        let proxy = shared.proxy.get().ok_or("proxy not ready")?.clone();
        let session = {
            let config = shared.config.borrow();
            let shell = config.shell();
            Session::spawn(
                &Options {
                    shell: &shell,
                    cwd: cwd.as_deref(),
                    scrollback: config.scrollback,
                },
                WinSize {
                    cols: 80,
                    rows: 24,
                    width: 0,
                    height: 0,
                },
                move |event| {
                    let _ = proxy.send(match event {
                        axo_term::Event::Wakeup => Msg::Wake(id),
                        axo_term::Event::Exit => Msg::Exit(id),
                    });
                },
            )?
        };
        let surface = shared.renderer.borrow_mut().create_surface()?;
        let inner = Rc::new(Inner {
            id,
            shared,
            session,
            surface,
            canvas: OnceCell::new(),
            menu: OnceCell::new(),
            state: RefCell::default(),
        });
        inner.report_colors();

        let painter = inner.clone();
        let canvas = parent.canvas(move |builder, size| painter.paint(builder, size));
        canvas.set_grow(1.0);
        canvas.set_basis(0.0);
        canvas.set_min_width(0.0);
        canvas.set_min_height(0.0);
        canvas.set_cursor(Some(Cursor::Text));
        canvas.set_skin(Some(crate::chrome::canvas));
        canvas.set_accessible_label("Terminal");
        let input = Rc::downgrade(&inner);
        canvas.set_input(move |canvas, event| {
            if let Some(inner) = input.upgrade() {
                inner.input(&canvas, event);
            }
        });
        let has_selection: Rc<dyn Fn() -> bool> = {
            let weak = Rc::downgrade(&inner);
            Rc::new(move || {
                weak.upgrade()
                    .is_some_and(|inner| inner.session.term().selection_range().is_some())
            })
        };
        let menu = BlockMenu::new(&canvas, inner.id, &inner.shared, Some(has_selection));
        // Right press, Menu key and Shift+F10 open the menu. A right press
        // while the application reports the mouse keeps the button instead;
        // Shift+right click overrides that in the press handler.
        let shown = menu.clone();
        let input = Rc::downgrade(&inner);
        canvas.on_context_menu(move |_, at| {
            let Some(inner) = input.upgrade() else { return };
            let suppressed = std::mem::take(&mut inner.state.borrow_mut().suppress_menu);
            if !suppressed {
                shown.show_at(at);
            }
        });
        let _ = inner.menu.set(menu);
        let _ = inner.canvas.set(canvas);
        Ok(Rc::new(Self { inner }))
    }

    fn canvas(&self) -> &Canvas {
        self.inner.canvas()
    }

    /// Encodes and sends a key; false when it has no terminal meaning.
    pub fn key(&self, event: &KeyEvent<'_>) -> bool {
        if event.modifiers.meta {
            return false;
        }
        let mut out = Vec::new();
        {
            let mut term = self.inner.session.term();
            match key(event.key) {
                Some(key) => {
                    if !encode_key(
                        key,
                        event.text,
                        mods(event.modifiers),
                        &term.modes,
                        &mut out,
                    ) {
                        return false;
                    }
                }
                None if !event.text.is_empty() => out.extend_from_slice(event.text.as_bytes()),
                None => return false,
            }
            if term.display_offset() > 0 {
                term.scroll_to_bottom();
                self.canvas().invalidate();
            }
        }
        self.inner.session.write(out);
        true
    }

    pub fn copy(&self) {
        let text = self.inner.session.term().selection_text();
        if let Some(text) = text {
            self.canvas().copy_text(&text);
        }
    }

    pub fn paste(&self) {
        self.canvas().request_paste();
    }

    /// Scrolls the view by whole screens (positive goes back in history).
    pub fn scroll_pages(&self, pages: isize) {
        let mut term = self.inner.session.term();
        let rows = term.rows() as isize;
        term.scroll_display(pages * (rows - 1).max(1));
        self.canvas().invalidate();
    }

    /// Handles a reader wakeup. The paint is deferred to the next presented
    /// frame instead of running once per PTY chunk, so a flood costs at most
    /// one render per frame.
    pub fn wake(&self) {
        let title = {
            let mut term = self.inner.session.term();
            term.take_title().map(str::to_owned)
        };
        if let Some(title) = title {
            self.inner.state.borrow_mut().title = title;
        }
        let schedule = {
            let mut state = self.inner.state.borrow_mut();
            state.pending = true;
            !std::mem::replace(&mut state.scheduled, true)
        };
        if schedule {
            self.schedule_frame();
        }
    }

    /// Registers a one-shot frame callback, which runs once the compositor
    /// presents the next frame and repaints only if output is still pending.
    fn schedule_frame(&self) {
        let inner = Rc::downgrade(&self.inner);
        self.canvas().on_frame(move |_, _| {
            let Some(inner) = inner.upgrade() else {
                return false;
            };
            let pending = {
                let mut state = inner.state.borrow_mut();
                state.scheduled = false;
                std::mem::take(&mut state.pending)
            };
            if pending {
                inner.canvas().invalidate();
            }
            false
        });
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        self.inner.session.cwd()
    }

    pub fn redraw(&self) {
        self.canvas().invalidate();
    }

    pub fn is_focused(&self) -> bool {
        self.canvas().is_alive() && self.canvas().is_focused()
    }
}

impl Inner {
    fn canvas(&self) -> &Canvas {
        self.canvas.get().expect("set at creation")
    }

    /// A title for a program that set none: the running command and the
    /// directory it runs in, so tabs and the window stay informative.
    fn context_title(&self) -> String {
        let dir = self.session.cwd().as_deref().map(display_path);
        let command = self
            .session
            .foreground()
            .filter(|(_, shell)| !shell)
            .map(|(name, _)| name);
        match (command, dir) {
            (Some(command), Some(dir)) => format!("{command} — {dir}"),
            (Some(command), None) => command,
            (None, Some(dir)) => dir,
            (None, None) => "Terminal".into(),
        }
    }

    fn menu(&self) -> &BlockMenu {
        self.menu.get().expect("set at creation")
    }

    fn report_colors(&self) {
        let palette = self.shared.palette.borrow();
        self.session.term().report_colors = [palette.foreground, palette.background];
    }

    fn paint(&self, builder: &mut SceneBuilder, size: Size) {
        let scale = self.shared.scale.get();
        let px = [
            (size.width * scale).round() as u32,
            (size.height * scale).round() as u32,
        ];
        if px[0] == 0 || px[1] == 0 {
            return;
        }
        let config = self.shared.config.borrow();
        let palette = self.shared.palette.borrow();
        let padding = (config.padding * scale).round() as u32;
        let mut renderer = self.shared.renderer.borrow_mut();
        let cell = renderer.cell_size();
        let grid = (
            (px[0].saturating_sub(2 * padding) / cell.width.max(1)).clamp(2, u16::MAX as u32)
                as u16,
            (px[1].saturating_sub(2 * padding) / cell.height.max(1)).clamp(1, u16::MAX as u32)
                as u16,
        );
        let focused = {
            let mut state = self.state.borrow_mut();
            if state.grid != grid {
                state.grid = grid;
                self.session.resize(WinSize {
                    cols: grid.0,
                    rows: grid.1,
                    width: px[0].min(u16::MAX as u32) as u16,
                    height: px[1].min(u16::MAX as u32) as u16,
                });
            }
            state.focused
        };
        // Output arriving from here on wakes the UI again.
        self.session.take_dirty();
        let term = self.session.term();
        let frame = Frame {
            term: &term,
            palette: &palette,
            cursor: config.cursor,
            focused,
            padding,
        };
        if let Err(error) = renderer.render(self.surface, px, &frame) {
            eprintln!("axo: terminal render failed: {error}");
        }
        drop(term);
        renderer.paint(
            self.surface,
            builder,
            Rect::new(0.0, 0.0, size.width, size.height),
        );
    }

    /// The view cell under a canvas-local point.
    fn cell_at(&self, position: aegle::prelude::Point) -> (usize, usize) {
        let scale = self.shared.scale.get();
        let padding = self.shared.config.borrow().padding * scale;
        let cell = self.shared.renderer.borrow().cell_size();
        let (cols, rows) = self.state.borrow().grid;
        let at = |v: f32, size: u32, max: u16| {
            (((v * scale - padding) / size.max(1) as f32)
                .floor()
                .max(0.0) as usize)
                .min(max.max(1) as usize - 1)
        };
        (
            at(position.x, cell.width, cols),
            at(position.y, cell.height, rows),
        )
    }

    fn report(
        &self,
        button: MouseButton,
        action: MouseAction,
        at: (usize, usize),
        modifiers: Modifiers,
    ) {
        let mut out = Vec::new();
        let term = self.session.term();
        encode_mouse(
            button,
            action,
            at.0,
            at.1,
            mods(modifiers),
            &term.modes,
            &mut out,
        );
        drop(term);
        self.session.write(out);
    }

    fn input(&self, canvas: &Canvas, event: CanvasEvent) {
        let mode = self.session.term().modes.mouse;
        let reporting = mode != MouseMode::Off;
        match event {
            CanvasEvent::Press {
                position,
                clicks,
                modifiers,
                ..
            } => {
                self.shared.focused.set(self.id);
                let at = self.cell_at(position);
                if reporting && !modifiers.shift {
                    self.state.borrow_mut().reported |= 1;
                    return self.report(MouseButton::Left, MouseAction::Press, at, modifiers);
                }
                let mut term = self.session.term();
                let point = Cell {
                    line: term.view_line(at.1),
                    col: at.0,
                };
                let kind = match clicks {
                    1 => SelectionKind::Simple,
                    2 => SelectionKind::Word,
                    _ => SelectionKind::Line,
                };
                term.start_selection(Selection::new(point, kind));
                let mut state = self.state.borrow_mut();
                state.selecting = true;
                state.drag = at;
                canvas.invalidate();
            }
            CanvasEvent::Move {
                position,
                pressed,
                modifiers,
                ..
            } => {
                let at = self.cell_at(position);
                let reported = self.state.borrow().reported;
                if reported != 0 && matches!(mode, MouseMode::Drag | MouseMode::Motion) {
                    let button = if reported & 1 != 0 {
                        MouseButton::Left
                    } else {
                        MouseButton::Right
                    };
                    return self.report(button, MouseAction::Motion, at, modifiers);
                }
                if !pressed && mode == MouseMode::Motion {
                    return self.report(MouseButton::None, MouseAction::Motion, at, modifiers);
                }
                let mut state = self.state.borrow_mut();
                if pressed && state.selecting && state.drag != at {
                    state.drag = at;
                    let mut term = self.session.term();
                    let line = term.view_line(at.1);
                    term.extend_selection(Cell { line, col: at.0 });
                    canvas.invalidate();
                }
            }
            CanvasEvent::Release {
                position,
                modifiers,
                ..
            } => {
                let mut state = self.state.borrow_mut();
                if state.reported & 1 != 0 {
                    state.reported &= !1;
                    drop(state);
                    return self.report(
                        MouseButton::Left,
                        MouseAction::Release,
                        self.cell_at(position),
                        modifiers,
                    );
                }
                state.selecting = false;
                let mut term = self.session.term();
                if term.selection_range().is_none() {
                    term.clear_selection();
                }
            }
            CanvasEvent::ButtonPress {
                button,
                position,
                modifiers,
                ..
            } => {
                let report = match button {
                    PointerButton::Middle => MouseButton::Middle,
                    PointerButton::Secondary => MouseButton::Right,
                    _ => return,
                };
                if reporting {
                    // Shift+right click asks for the menu even while the
                    // application is reporting the mouse; otherwise the
                    // button belongs to the application.
                    if report == MouseButton::Right && modifiers.shift {
                        let bounds = canvas.bounds();
                        let at =
                            Point::new(bounds.origin.x + position.x, bounds.origin.y + position.y);
                        self.menu().show_at(at);
                    } else {
                        self.state.borrow_mut().reported |= 2;
                        self.report(
                            report,
                            MouseAction::Press,
                            self.cell_at(position),
                            modifiers,
                        );
                    }
                    if report == MouseButton::Right {
                        // A context menu request follows every right press.
                        self.state.borrow_mut().suppress_menu = true;
                    }
                } else if report == MouseButton::Middle {
                    canvas.request_paste();
                }
            }
            CanvasEvent::ButtonRelease {
                button,
                position,
                modifiers,
                ..
            } => {
                let report = match button {
                    PointerButton::Middle => MouseButton::Middle,
                    PointerButton::Secondary => MouseButton::Right,
                    _ => return,
                };
                if reporting {
                    self.state.borrow_mut().reported &= !2;
                    self.report(
                        report,
                        MouseAction::Release,
                        self.cell_at(position),
                        modifiers,
                    );
                }
            }
            CanvasEvent::Wheel {
                position,
                delta,
                modifiers,
                ..
            } => {
                let line = self.shared.renderer.borrow().cell_size().height as f32
                    / self.shared.scale.get();
                let lines = {
                    let mut state = self.state.borrow_mut();
                    state.wheel += delta.y / line.max(1.0);
                    let lines = state.wheel.trunc();
                    state.wheel -= lines;
                    lines as isize
                };
                if lines == 0 {
                    return;
                }
                // Positive deltas move content up: toward newer lines.
                let down = lines > 0;
                let mut term = self.session.term();
                if reporting {
                    drop(term);
                    let button = if down {
                        MouseButton::WheelDown
                    } else {
                        MouseButton::WheelUp
                    };
                    for _ in 0..lines.unsigned_abs().min(10) {
                        self.report(
                            button,
                            MouseAction::Press,
                            self.cell_at(position),
                            modifiers,
                        );
                    }
                } else if term.alt_screen() && term.modes.alternate_scroll {
                    let mut out = Vec::new();
                    let key = if down { Key::Down } else { Key::Up };
                    for _ in 0..lines.unsigned_abs() {
                        encode_key(key, "", Mods::default(), &term.modes, &mut out);
                    }
                    drop(term);
                    self.session.write(out);
                } else {
                    term.scroll_display(-lines);
                    canvas.invalidate();
                }
            }
            CanvasEvent::Focus(focused) => {
                self.state.borrow_mut().focused = focused;
                if focused && self.shared.focused.get() != self.id {
                    self.shared.focused.set(self.id);
                    if let Some(proxy) = self.shared.proxy.get() {
                        let _ = proxy.send(Msg::Focus(self.id));
                    }
                }
                if self.session.term().modes.focus_events {
                    self.session.write(if focused {
                        &b"\x1b[I"[..]
                    } else {
                        &b"\x1b[O"[..]
                    });
                }
                canvas.invalidate();
            }
            CanvasEvent::Paste(text) => self.paste(&text),
            _ => {}
        }
    }

    fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.session.term().modes.bracketed_paste;
        let bytes = match bracketed {
            // The payload cannot end the bracket early.
            true => format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")),
            false => text,
        };
        self.session.write(bytes);
    }
}

impl Block for Terminal {
    fn id(&self) -> u64 {
        self.inner.id
    }
    fn node(&self) -> Node {
        (**self.canvas()).clone()
    }
    fn focus(&self) {
        self.canvas().focus();
    }
    fn title(&self) -> String {
        let title = self.inner.state.borrow().title.clone();
        match title.is_empty() {
            true => self.inner.context_title(),
            false => title,
        }
    }
    fn apply(&self, config: &Config) {
        self.inner.session.term().set_scrollback(config.scrollback);
        self.inner.report_colors();
        self.redraw();
    }
    fn close(&self) {
        self.inner
            .shared
            .renderer
            .borrow_mut()
            .destroy_surface(self.inner.surface);
    }
    fn as_terminal(&self) -> Option<&Terminal> {
        Some(self)
    }
}

/// `path` with the home directory abbreviated to `~`.
fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return match rest.as_os_str().is_empty() {
            true => "~".into(),
            false => format!("~/{}", rest.display()),
        };
    }
    path.display().to_string()
}
