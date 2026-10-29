//! The window's tabs: a tab bar on top and, per tab, a page holding a split
//! tree of blocks. Routes shortcuts, terminal messages and settings changes.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
};

use aegle::{app::Window, prelude::*, ui::KeyEvent};

use crate::{
    blocks::{Block, BlockRef, Settings, Terminal},
    config::Config,
    keys::{self, Action, Dir},
    layout::Pane,
    menu::MenuCommand,
    shared::{Msg, SharedRef},
};

struct Tab {
    id: u64,
    item: Container,
    title: Button,
    page: Container,
    root: Pane,
    /// The block focused last in this tab.
    focused: u64,
}

impl Tab {
    fn blocks(&self) -> Vec<BlockRef> {
        let mut out = Vec::new();
        self.root.blocks(&mut out);
        out
    }
}

pub struct Workspace {
    this: Weak<Workspace>,
    window: Window,
    bar: Container,
    tab_row: Container,
    pages: Container,
    tabs: RefCell<Vec<Tab>>,
    active: Cell<usize>,
    shared: SharedRef,
}

enum Kind {
    Terminal,
    Settings,
}

const TITLE_CHARS: usize = 28;

impl Workspace {
    pub fn new(
        window: Window,
        bar: Container,
        tab_row: Container,
        pages: Container,
        shared: SharedRef,
    ) -> Rc<Self> {
        let workspace = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            window,
            bar,
            tab_row,
            pages,
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(0),
            shared,
        });
        workspace.style_bar();
        let ws = workspace.this.clone();
        workspace
            .window
            .ui()
            .on_key(move |event| ws.upgrade().is_some_and(|ws| ws.key(&event)));
        workspace
    }

    fn rc(&self) -> Rc<Self> {
        self.this.upgrade().expect("workspace alive")
    }

    fn style_bar(&self) {
        self.bar
            .set_background(self.shared.config.borrow().bar_color());
    }

    /// The focused terminal's directory, where new terminals start.
    fn cwd(&self) -> Option<PathBuf> {
        self.focused_terminal()
            .and_then(|t| t.as_terminal().and_then(Terminal::cwd))
    }

    /// Builds a block in `parent`; must not borrow `tabs` (callers may hold it).
    fn make(&self, kind: Kind, parent: &Container, cwd: Option<PathBuf>) -> Result<BlockRef> {
        Ok(match kind {
            Kind::Terminal => Terminal::new(parent, self.shared.clone(), cwd)?,
            Kind::Settings => {
                let ws = self.this.clone();
                let apply = Rc::new(move |config: Config| {
                    if let Some(ws) = ws.upgrade() {
                        ws.apply_config(config);
                    }
                });
                Settings::new(parent, self.shared.clone(), apply)?
            }
        })
    }

    // ---- tabs ----

    pub fn new_tab(&self) -> Result<()> {
        let page = self.pages.column();
        page.set_grow(1.0);
        page.set_basis(0.0);
        page.set_min_width(0.0);
        page.set_min_height(0.0);
        page.set_gap(0.0, 0.0);
        page.set_padding(0.0);
        let cwd = self.cwd();
        let block = match self.make(Kind::Terminal, &page, cwd) {
            Ok(block) => block,
            Err(error) => {
                page.remove();
                return Err(error);
            }
        };
        let id = crate::blocks::next_id();
        let item = self.tab_row.row();
        item.set_gap(0.0, 0.0);
        item.set_min_width(0.0);
        let title = item.button("Terminal");
        title.set_min_width(0.0);
        let close = item.button("×");
        close.set_tooltip("Close tab");
        let ws = self.this.clone();
        title.on_click(move |_| {
            if let Some(ws) = ws.upgrade()
                && let Some(index) = ws.tab_index(id)
            {
                ws.select(index);
            }
        });
        let ws = self.this.clone();
        close.on_click(move |_| {
            if let Some(ws) = ws.upgrade()
                && let Some(index) = ws.tab_index(id)
            {
                ws.close_tab(index);
            }
        });
        let focused = block.id();
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.push(Tab {
                id,
                item,
                title,
                page,
                root: Pane::Leaf(block),
                focused,
            });
            tabs.len() - 1
        };
        self.select(index);
        Ok(())
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.borrow().iter().position(|tab| tab.id == id)
    }

    fn tab_of_block(&self, id: u64) -> Option<usize> {
        self.tabs
            .borrow()
            .iter()
            .position(|tab| tab.blocks().iter().any(|b| b.id() == id))
    }

    pub fn select(&self, index: usize) {
        let focus = {
            let tabs = self.tabs.borrow();
            let Some(tab) = tabs.get(index) else { return };
            for (i, tab) in tabs.iter().enumerate() {
                tab.page.set_visible(i == index);
                tab.title.set_accent(i == index);
            }
            self.active.set(index);
            let blocks = tab.blocks();
            blocks
                .iter()
                .find(|b| b.id() == tab.focused)
                .or(blocks.first())
                .cloned()
        };
        if let Some(block) = focus {
            self.shared.focused.set(block.id());
            block.focus();
        }
        self.update_titles();
    }

    fn close_tab(&self, index: usize) {
        let tab = self.tabs.borrow_mut().remove(index);
        for block in tab.blocks() {
            block.close();
        }
        tab.item.remove();
        tab.page.remove();
        let len = self.tabs.borrow().len();
        if len == 0 {
            let _ = self.window.close();
        } else {
            self.select(index.min(len - 1));
        }
    }

    fn cycle(&self, step: isize) {
        let len = self.tabs.borrow().len() as isize;
        if len > 0 {
            self.select((self.active.get() as isize + step).rem_euclid(len) as usize);
        }
    }

    /// Shows each tab's focused block title, and the active one on the window.
    fn update_titles(&self) {
        let tabs = self.tabs.borrow();
        for (i, tab) in tabs.iter().enumerate() {
            let blocks = tab.blocks();
            let block = blocks
                .iter()
                .find(|b| b.id() == tab.focused)
                .or(blocks.first());
            let title = block.map_or_else(|| "Terminal".into(), |b| b.title());
            let short: String = match title.chars().count() > TITLE_CHARS {
                true => title.chars().take(TITLE_CHARS - 1).chain(['…']).collect(),
                false => title.clone(),
            };
            if tab.title.text() != short {
                tab.title.set_text(&short);
                tab.title.set_tooltip(title.as_str());
            }
            if i == self.active.get() {
                let _ = self.window.set_title(&format!("{title} — Axo"));
            }
        }
    }

    // ---- blocks ----

    /// The focused block of the active tab, remembering it on the tab.
    fn focused_block(&self) -> Option<BlockRef> {
        let mut tabs = self.tabs.borrow_mut();
        let tab = tabs.get_mut(self.active.get())?;
        let blocks = tab.blocks();
        let focused = self.shared.focused.get();
        let block = blocks
            .iter()
            .find(|b| b.id() == focused)
            .or_else(|| blocks.iter().find(|b| b.id() == tab.focused))
            .or(blocks.first())?
            .clone();
        tab.focused = block.id();
        Some(block)
    }

    fn focused_terminal(&self) -> Option<Rc<dyn Block>> {
        self.focused_block().filter(|b| b.as_terminal().is_some())
    }

    /// The block with `id`, in whichever tab holds it.
    fn block(&self, id: u64) -> Option<BlockRef> {
        self.tabs
            .borrow()
            .iter()
            .find_map(|tab| tab.blocks().into_iter().find(|b| b.id() == id))
    }

    fn split(&self, orientation: Orientation, kind: Kind) -> Result<()> {
        match self.focused_block() {
            Some(target) => self.split_block(target.id(), orientation, kind),
            None => self.new_tab(),
        }
    }

    /// Splits block `id` in its tab and focuses the new block.
    fn split_block(&self, id: u64, orientation: Orientation, kind: Kind) -> Result<()> {
        let Some(index) = self.tab_of_block(id) else {
            return Ok(());
        };
        // New terminals start in the split terminal's directory.
        let mut kind = Some(kind);
        let mut cwd = self
            .block(id)
            .and_then(|b| b.as_terminal().and_then(Terminal::cwd));
        let new = {
            let mut tabs = self.tabs.borrow_mut();
            let tab = &mut tabs[index];
            let page = tab.page.clone();
            let mut make = |parent: &Container| {
                self.make(
                    kind.take().expect("one block per split"),
                    parent,
                    cwd.take(),
                )
            };
            tab.root.split(&page, id, orientation, &mut make)?
        };
        if let Some(new) = new
            && index == self.active.get()
        {
            self.focus_block(&new);
        }
        Ok(())
    }

    fn focus_block(&self, block: &BlockRef) {
        self.shared.focused.set(block.id());
        if let Some(tab) = self.tabs.borrow_mut().get_mut(self.active.get()) {
            tab.focused = block.id();
        }
        block.focus();
        self.update_titles();
    }

    /// Closes block `id`; the last block of a tab closes the tab.
    pub fn close_block(&self, id: u64) {
        let Some(index) = self.tab_of_block(id) else {
            return;
        };
        let last = matches!(self.tabs.borrow()[index].root, Pane::Leaf(_));
        if last {
            return self.close_tab(index);
        }
        let next = {
            let mut tabs = self.tabs.borrow_mut();
            let tab = &mut tabs[index];
            if let Some(block) = tab.blocks().into_iter().find(|b| b.id() == id) {
                block.close();
            }
            let page = tab.page.clone();
            tab.root.remove(&page, id);
            tab.blocks().into_iter().next()
        };
        if index == self.active.get()
            && let Some(next) = next
        {
            self.focus_block(&next);
        }
    }

    fn open_settings(&self) -> Result<()> {
        let existing = self
            .tabs
            .borrow()
            .get(self.active.get())
            .and_then(|tab| tab.blocks().into_iter().find(|b| b.as_terminal().is_none()));
        match existing {
            Some(block) => {
                self.focus_block(&block);
                Ok(())
            }
            None => self.split(Orientation::Horizontal, Kind::Settings),
        }
    }

    /// Focuses the nearest block whose center lies in `dir` from the focused one.
    fn move_focus(&self, dir: Dir) {
        let Some(current) = self.focused_block() else {
            return;
        };
        let center = |b: &BlockRef| {
            let r = b.node().bounds();
            (
                r.origin.x + r.size.width / 2.0,
                r.origin.y + r.size.height / 2.0,
            )
        };
        let (cx, cy) = center(&current);
        let blocks = self.tabs.borrow()[self.active.get()].blocks();
        let best = blocks
            .into_iter()
            .filter(|b| b.id() != current.id())
            .filter_map(|b| {
                let (x, y) = center(&b);
                let (dx, dy) = (x - cx, y - cy);
                let ahead = match dir {
                    Dir::Left => -dx,
                    Dir::Right => dx,
                    Dir::Up => -dy,
                    Dir::Down => dy,
                };
                (ahead > 1.0).then_some((dx * dx + dy * dy, b))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, block)) = best {
            self.focus_block(&block);
        }
    }

    // ---- events ----

    pub fn handle(&self, msg: Msg) {
        match msg {
            Msg::Wake(id) => {
                self.refresh_scale();
                let block = self
                    .tabs
                    .borrow()
                    .iter()
                    .find_map(|tab| tab.blocks().into_iter().find(|b| b.id() == id));
                if let Some(terminal) = block.as_ref().and_then(|b| b.as_terminal())
                    && terminal.wake().is_some()
                {
                    self.update_titles();
                }
            }
            Msg::Exit(id) => self.close_block(id),
            Msg::Action(action) => {
                if let Err(error) = self.run(action) {
                    eprintln!("axo: {error}");
                }
            }
            Msg::Menu(id, command) => {
                if let Err(error) = self.menu(id, command) {
                    eprintln!("axo: {error}");
                }
            }
        }
    }

    fn key(&self, event: &KeyEvent<'_>) -> bool {
        if !event.pressed {
            return false;
        }
        if let Some(action) = keys::shortcut(event) {
            if let Some(proxy) = self.shared.proxy.get() {
                let _ = proxy.send(Msg::Action(action));
            }
            return true;
        }
        match self.focused_terminal() {
            Some(block) => {
                let terminal = block.as_terminal().unwrap();
                terminal.is_focused() && terminal.key(event)
            }
            None => false,
        }
    }

    pub fn run(&self, action: Action) -> Result<()> {
        let terminal = || self.focused_terminal();
        match action {
            Action::NewTab => self.new_tab()?,
            Action::CloseBlock => {
                if let Some(block) = self.focused_block() {
                    self.close_block(block.id());
                }
            }
            Action::SplitRight => self.split(Orientation::Horizontal, Kind::Terminal)?,
            Action::SplitDown => self.split(Orientation::Vertical, Kind::Terminal)?,
            Action::Copy => {
                if let Some(block) = terminal() {
                    block.as_terminal().unwrap().copy();
                }
            }
            Action::Paste => {
                if let Some(block) = terminal() {
                    block.as_terminal().unwrap().paste();
                }
            }
            Action::Settings => self.open_settings()?,
            Action::NextTab => self.cycle(1),
            Action::PrevTab => self.cycle(-1),
            Action::GoTab(index) => self.select(index),
            Action::Focus(dir) => self.move_focus(dir),
            Action::FontSize(step) => {
                let mut config = self.shared.config.borrow().clone();
                config.font_size = match step {
                    0 => Config::default().font_size,
                    step => (config.font_size + step as f32).clamp(4.0, 96.0),
                };
                self.apply_config(config);
            }
            Action::Scroll(pages) => {
                if let Some(block) = terminal() {
                    block.as_terminal().unwrap().scroll_pages(pages);
                }
            }
        }
        Ok(())
    }

    /// Runs a command chosen in the right-click menu of block `id`.
    fn menu(&self, id: u64, command: MenuCommand) -> Result<()> {
        let block = self.block(id);
        let terminal = || block.as_ref().and_then(|b| b.as_terminal());
        match command {
            MenuCommand::Copy => {
                if let Some(terminal) = terminal() {
                    terminal.copy();
                }
            }
            MenuCommand::Paste => {
                if let Some(terminal) = terminal() {
                    terminal.paste();
                }
            }
            MenuCommand::SplitRight => {
                self.split_block(id, Orientation::Horizontal, Kind::Terminal)?;
            }
            MenuCommand::SplitDown => {
                self.split_block(id, Orientation::Vertical, Kind::Terminal)?;
            }
            MenuCommand::ClosePane => self.close_block(id),
            MenuCommand::NewTab => self.new_tab()?,
            MenuCommand::Settings => self.open_settings()?,
        }
        Ok(())
    }

    /// Re-rasterizes at the window's current scale when it changed.
    pub fn refresh_scale(&self) {
        let scale = self.window.scale().unwrap_or(1.0);
        if scale == self.shared.scale.get() {
            return;
        }
        self.shared.scale.set(scale);
        let font = self.shared.config.borrow().font();
        if let Err(error) = self.shared.renderer.borrow_mut().set_font(&font, scale) {
            eprintln!("axo: font: {error}");
        }
        self.redraw_all();
    }

    fn redraw_all(&self) {
        for tab in self.tabs.borrow().iter() {
            for block in tab.blocks() {
                if let Some(terminal) = block.as_terminal() {
                    terminal.redraw();
                }
            }
        }
    }

    pub fn apply_config(&self, config: Config) {
        let old = self.shared.config.replace(config.clone());
        if old.font() != config.font() {
            let scale = self.shared.scale.get();
            if let Err(error) = self
                .shared
                .renderer
                .borrow_mut()
                .set_font(&config.font(), scale)
            {
                eprintln!("axo: font: {error}");
                self.shared.config.borrow_mut().font_family = old.font_family.clone();
            }
        }
        if old.theme != config.theme || old.opacity != config.opacity {
            *self.shared.palette.borrow_mut() = config.palette();
            self.window.set_theme(config.theme(self.shared.transparent));
            self.style_bar();
        }
        for tab in self.tabs.borrow().iter() {
            for block in tab.blocks() {
                block.apply(&config);
            }
        }
    }
}

/// Opens the first tab and the add/settings buttons.
pub fn start(workspace: &Rc<Workspace>, add: &Button, settings: &Button) -> Result<()> {
    let ws = workspace.clone();
    add.on_click(move |_| ws.run(Action::NewTab));
    let ws = workspace.clone();
    settings.on_click(move |_| ws.run(Action::Settings));
    let ws = workspace.clone();
    workspace.window.on_frame(move |_, _| {
        ws.refresh_scale();
        false
    });
    workspace.rc().new_tab()
}
