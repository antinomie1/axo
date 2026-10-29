//! Blocks: the content of one region of a tab. A tab holds a tree of them
//! ([`crate::layout`]); each block owns one aegle node placed in its pane.

pub mod settings;
pub mod terminal;

use std::rc::Rc;

use aegle::prelude::Node;

use crate::config::Config;

pub use settings::Settings;
pub use terminal::Terminal;

pub trait Block {
    /// Stable identity for lookups and messages.
    fn id(&self) -> u64;
    /// The block's root node inside its pane.
    fn node(&self) -> Node;
    fn focus(&self);
    fn title(&self) -> String;
    /// Applies changed settings.
    fn apply(&self, _config: &Config) {}
    /// Releases resources before the node is removed.
    fn close(&self) {}
    fn as_terminal(&self) -> Option<&Terminal> {
        None
    }
}

pub type BlockRef = Rc<dyn Block>;

pub fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
