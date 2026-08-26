//! Undo as a stack of transactions over document state.
//!
//! It records *states*, not inverse operations, and that is a decision the
//! kernel contract makes for us: bodies are immutable, so a boolean does not
//! consume its operands and the previous state is still sitting there,
//! addressable. Inverting a boolean is not a thing a B-rep kernel can do; not
//! having to is the whole reason the contract is written that way.
//!
//! The cost is that the kernel holds every intermediate body for as long as
//! history refers to it. That is what `Document::collect_garbage` is for, and
//! it is the reason history has a limit at all.

use crate::arena::Arena;
use crate::document::{Node, NodeId};
use w3d_kernel::Body;

pub(crate) enum Edit {
    Insert {
        id: NodeId,
        node: Node,
    },
    Remove {
        id: NodeId,
        node: Node,
    },
    Replace {
        id: NodeId,
        before: Node,
        after: Node,
    },
}

impl Edit {
    fn undo(&self, nodes: &mut Arena<Node>) {
        match self {
            Self::Insert { id, .. } => {
                nodes.remove(*id);
            }
            Self::Remove { id, node } => {
                nodes.restore(*id, node.clone());
            }
            Self::Replace { id, before, .. } => {
                if let Some(slot) = nodes.get_mut(*id) {
                    *slot = before.clone();
                }
            }
        }
    }

    fn redo(&self, nodes: &mut Arena<Node>) {
        match self {
            Self::Insert { id, node } => {
                nodes.restore(*id, node.clone());
            }
            Self::Remove { id, .. } => {
                nodes.remove(*id);
            }
            Self::Replace { id, after, .. } => {
                if let Some(slot) = nodes.get_mut(*id) {
                    *slot = after.clone();
                }
            }
        }
    }

    fn bodies(&self) -> impl Iterator<Item = Body> {
        match self {
            Self::Insert { node, .. } | Self::Remove { node, .. } => vec![node.body].into_iter(),
            Self::Replace { before, after, .. } => vec![before.body, after.body].into_iter(),
        }
    }
}

pub(crate) struct Transaction {
    pub label: &'static str,
    pub edits: Vec<Edit>,
    pub depth: u32,
}

pub struct History {
    done: Vec<Transaction>,
    undone: Vec<Transaction>,
    open: Option<Transaction>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::with_limit(200)
    }
}

impl History {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            done: Vec::new(),
            undone: Vec::new(),
            open: None,
            limit: limit.max(1),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// The label of what undo would reverse, for a menu item that names it.
    pub fn undo_label(&self) -> Option<&'static str> {
        self.done.last().map(|t| t.label)
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.undone.last().map(|t| t.label)
    }

    /// There is deliberately no `abort`. The document does everything that can
    /// fail — every kernel call — *before* opening a transaction, so a
    /// transaction that has begun cannot fail partway. Adding an abort path
    /// would be adding the possibility it exists to handle.
    pub(crate) fn begin(&mut self, label: &'static str) {
        if let Some(open) = self.open.as_mut() {
            open.depth += 1;
        } else {
            self.open = Some(Transaction {
                label,
                edits: Vec::new(),
                depth: 1,
            });
        }
    }

    pub(crate) fn record(&mut self, edit: Edit) {
        if let Some(open) = self.open.as_mut() {
            open.edits.push(edit);
        }
    }

    pub(crate) fn commit(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.depth > 1 {
            open.depth -= 1;
            return;
        }
        let open = self.open.take().unwrap();
        if open.edits.is_empty() {
            return;
        }
        self.done.push(open);
        // A new edit is what makes the redo branch unreachable, so it goes.
        self.undone.clear();
        if self.done.len() > self.limit {
            self.done.remove(0);
        }
    }

    pub(crate) fn undo(&mut self, nodes: &mut Arena<Node>) -> Option<&'static str> {
        let t = self.done.pop()?;
        for edit in t.edits.iter().rev() {
            edit.undo(nodes);
        }
        let label = t.label;
        self.undone.push(t);
        Some(label)
    }

    pub(crate) fn redo(&mut self, nodes: &mut Arena<Node>) -> Option<&'static str> {
        let t = self.undone.pop()?;
        for edit in &t.edits {
            edit.redo(nodes);
        }
        let label = t.label;
        self.done.push(t);
        Some(label)
    }

    pub(crate) fn bodies(&self) -> impl Iterator<Item = Body> {
        self.done
            .iter()
            .chain(&self.undone)
            .chain(self.open.as_ref())
            .flat_map(|t| t.edits.iter().flat_map(Edit::bodies))
    }

    /// Forgets everything. The bodies it was holding become collectable.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
        self.open = None;
    }
}
