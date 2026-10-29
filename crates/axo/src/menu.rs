//! The right-click menu of a block: clipboard, pane and tab commands.
//!
//! Items send a [`MenuCommand`] for their block through the shared proxy, so
//! they apply to the block the menu belongs to, whatever has focus.

use std::rc::Rc;

use aegle::prelude::*;

use crate::shared::{Msg, SharedRef};

/// A command chosen in a block's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    Copy,
    Paste,
    /// Side by side.
    SplitRight,
    /// Stacked.
    SplitDown,
    ClosePane,
    NewTab,
    Settings,
}

/// A block's context menu, anchored to the block's node.
#[derive(Clone)]
pub struct BlockMenu {
    popup: Popup,
    copy: MenuItem,
    paste: MenuItem,
    new_tab: MenuItem,
    shared: SharedRef,
    /// Whether the block is a terminal, and whether it has a selection.
    /// `None` for blocks without a clipboard.
    clipboard: Option<Rc<dyn Fn() -> bool>>,
}

impl BlockMenu {
    /// Builds the menu anchored to `node` of block `id`; `clipboard` reports
    /// whether a terminal has a selection, and is `None` for blocks that
    /// cannot copy.
    pub fn new(
        node: &Node,
        id: u64,
        shared: &SharedRef,
        clipboard: Option<Rc<dyn Fn() -> bool>>,
    ) -> Self {
        let popup = node.menu();
        let run = |command: MenuCommand| {
            let shared = shared.clone();
            move |_: MenuItem| {
                if let Some(proxy) = shared.proxy.get() {
                    let _ = proxy.send(Msg::Menu(id, command));
                }
            }
        };
        let command = |text: &str, action: MenuCommand, shortcut: &str| {
            let item = popup.item(text);
            item.set_shortcut(Some(shortcut));
            item.on_click(run(action));
            item
        };
        let copy = command("Copy", MenuCommand::Copy, "Ctrl+Shift+C");
        let paste = command("Paste", MenuCommand::Paste, "Ctrl+Shift+V");
        popup.separator();
        command("Split Right", MenuCommand::SplitRight, "Ctrl+Shift+D");
        command("Split Down", MenuCommand::SplitDown, "Ctrl+Shift+E");
        popup.separator();
        command("Close Pane", MenuCommand::ClosePane, "Ctrl+Shift+W");
        let new_tab = command("New Tab", MenuCommand::NewTab, "Ctrl+Shift+T");
        popup.separator();
        command("Settings", MenuCommand::Settings, "Ctrl+,");
        Self {
            popup,
            copy,
            paste,
            new_tab,
            shared: shared.clone(),
            clipboard,
        }
    }

    /// Shows the menu at a logical window point, after updating which
    /// clipboard commands apply.
    pub fn show_at(&self, at: Point) {
        let (copy, paste) = match &self.clipboard {
            Some(has_selection) => (has_selection(), true),
            None => (false, false),
        };
        self.copy.set_enabled(copy);
        self.paste.set_enabled(paste);
        // New tabs need the tab bar; without it the workspace is one tab.
        let tabs = self.shared.config.borrow().tab_bar;
        self.new_tab.set_enabled(tabs);
        self.new_tab.set_visible(tabs);
        self.popup.show_at(at);
    }
}
