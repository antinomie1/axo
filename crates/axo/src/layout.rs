//! A tab's split tree: leaves are blocks, inner nodes are aegle splitters.
//!
//! Every pane container holds exactly one child (a block's node or a nested
//! splitter), so splitting and closing only move nodes between containers.

use aegle::prelude::{Container, Orientation, Splitter, Widgets};

use crate::blocks::BlockRef;

pub enum Pane {
    Leaf(BlockRef),
    Split {
        splitter: Splitter,
        first: Box<Pane>,
        second: Box<Pane>,
    },
}

impl Pane {
    fn node(&self) -> aegle::prelude::Node {
        match self {
            Pane::Leaf(block) => block.node(),
            Pane::Split { splitter, .. } => (**splitter).clone(),
        }
    }

    fn is(&self, id: u64) -> bool {
        matches!(self, Pane::Leaf(block) if block.id() == id)
    }

    /// Every block, in reading order.
    pub fn blocks(&self, out: &mut Vec<BlockRef>) {
        match self {
            Pane::Leaf(block) => out.push(block.clone()),
            Pane::Split { first, second, .. } => {
                first.blocks(out);
                second.blocks(out);
            }
        }
    }

    /// Splits the block `id` (inside `parent`) and builds the new block in the
    /// second pane. Returns the new block, or `None` when `id` is elsewhere.
    pub fn split(
        &mut self,
        parent: &Container,
        id: u64,
        orientation: Orientation,
        make: &mut dyn FnMut(&Container) -> aegle::prelude::Result<BlockRef>,
    ) -> aegle::prelude::Result<Option<BlockRef>> {
        match self {
            Pane::Leaf(block) if block.id() == id => {
                let splitter = parent.splitter(orientation);
                block.node().reparent(splitter.first());
                let new = make(splitter.second())?;
                let old = std::mem::replace(self, Pane::Leaf(new.clone()));
                *self = Pane::Split {
                    first: Box::new(old),
                    second: Box::new(Pane::Leaf(new.clone())),
                    splitter,
                };
                Ok(Some(new))
            }
            Pane::Leaf(_) => Ok(None),
            Pane::Split {
                splitter,
                first,
                second,
            } => match first.split(splitter.first(), id, orientation, make)? {
                Some(new) => Ok(Some(new)),
                None => second.split(splitter.second(), id, orientation, make),
            },
        }
    }

    /// Removes block `id` (not the root leaf) and lets its sibling take the
    /// splitter's place in `parent`. Returns whether it was found.
    pub fn remove(&mut self, parent: &Container, id: u64) -> bool {
        let Pane::Split {
            splitter,
            first,
            second,
        } = self
        else {
            return false;
        };
        let survivor = if first.is(id) {
            Some(std::mem::replace(second.as_mut(), Pane::Leaf(dummy())))
        } else if second.is(id) {
            Some(std::mem::replace(first.as_mut(), Pane::Leaf(dummy())))
        } else {
            None
        };
        match survivor {
            Some(survivor) => {
                survivor.node().reparent(parent);
                splitter.remove();
                *self = survivor;
                true
            }
            None => {
                let (a, b) = (splitter.first().clone(), splitter.second().clone());
                first.remove(&a, id) || second.remove(&b, id)
            }
        }
    }
}

/// A placeholder leaf that lives only while the tree is rearranged.
fn dummy() -> BlockRef {
    struct Gone;
    impl crate::blocks::Block for Gone {
        fn id(&self) -> u64 {
            0
        }
        fn node(&self) -> aegle::prelude::Node {
            unreachable!("placeholder leaf")
        }
        fn focus(&self) {}
        fn title(&self) -> String {
            String::new()
        }
    }
    std::rc::Rc::new(Gone)
}
