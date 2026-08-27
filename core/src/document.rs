//! The document: what the modeller edits.
//!
//! Generic over the kernel, and that is the point — nothing here names a
//! backend, and no `TopoDS_Shape` or OCCT handle can appear in this file
//! because [`w3d_kernel::Body`] is opaque. The seam holds or this file stops
//! compiling.

use crate::arena::{Arena, Id};
use crate::history::{Edit, History};
use std::collections::{HashMap, HashSet};
use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Quality, Tolerance, Topology,
    Vec3,
};

pub type NodeId = Id<Node>;

/// One solid in the document, with the things a user gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub body: Body,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentError {
    UnknownNode(NodeId),
    HistoryNotEmpty,
    Kernel(KernelError),
}

impl core::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "no such node: {id:?}"),
            Self::HistoryNotEmpty => write!(f, "cannot compact arena while history is non-empty"),
            Self::Kernel(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for DocumentError {}

impl From<KernelError> for DocumentError {
    fn from(e: KernelError) -> Self {
        Self::Kernel(e)
    }
}

pub type Result<T> = core::result::Result<T, DocumentError>;

pub struct Document<K: GeometryKernel> {
    kernel: K,
    nodes: Arena<Node>,
    selection: Vec<NodeId>,
    tolerance: Tolerance,
    quality: Quality,
    history: History,
    /// Every body this document has ever asked the kernel for. The kernel
    /// deliberately cannot enumerate its own bodies — a narrow trait is a
    /// short list of things that can break — so the document remembers, and
    /// `collect_garbage` is the only thing that reads it.
    created: Vec<Body>,
    /// Keyed by body, not by node, which makes it permanently valid: bodies
    /// are immutable, so a body's mesh at a given quality never changes. Only
    /// a quality change or garbage collection may evict.
    meshes: HashMap<u32, Mesh>,
}

impl<K: GeometryKernel> Document<K> {
    pub fn new(kernel: K) -> Self {
        Self {
            kernel,
            nodes: Arena::new(),
            selection: Vec::new(),
            tolerance: Tolerance::document_default(),
            quality: Quality::display_default(),
            history: History::default(),
            created: Vec::new(),
            meshes: HashMap::new(),
        }
    }

    /// Rebuilds a document from a file.
    ///
    /// Not general-purpose insertion, and deliberately one constructor rather
    /// than a sequence of mutations: a half-loaded document is not a state
    /// anything should be able to observe. The bodies must be ones this
    /// `kernel` just produced — from `GeometryKernel::load_body` — because a
    /// `Body` means nothing to any other kernel.
    ///
    /// **History starts empty.** A loaded document has nothing to undo back
    /// to, which is a property rather than an omission: the edits that built
    /// it happened in another process, and the bodies they referred to are not
    /// in this one.
    pub fn from_parts(
        kernel: K,
        tolerance: Tolerance,
        quality: Quality,
        nodes: impl IntoIterator<Item = Node>,
    ) -> Self {
        let mut doc = Self::new(kernel);
        doc.tolerance = tolerance;
        doc.quality = quality;
        for node in nodes {
            doc.created.push(node.body);
            doc.nodes.insert(node);
        }
        doc
    }

    pub fn kernel(&self) -> &K {
        &self.kernel
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn tolerance(&self) -> Tolerance {
        self.tolerance
    }

    /// Changing it does not re-evaluate anything already built. A document's
    /// tolerance is the tolerance operations *from here on* are asked to
    /// respect; retrofitting it to existing geometry is a rebuild, and a
    /// rebuild is a feature that does not exist yet.
    pub fn set_tolerance(&mut self, tolerance: Tolerance) {
        self.tolerance = tolerance;
    }

    pub fn quality(&self) -> Quality {
        self.quality
    }

    pub fn set_quality(&mut self, quality: Quality) {
        if quality != self.quality {
            self.quality = quality;
            self.meshes.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// In arena order, which is stable and identical on every machine.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes.iter()
    }

    pub fn node(&self, id: NodeId) -> Result<&Node> {
        self.nodes.get(id).ok_or(DocumentError::UnknownNode(id))
    }

    fn body_of(&self, id: NodeId) -> Result<Body> {
        self.node(id).map(|n| n.body)
    }

    fn track(&mut self, body: Body) -> Body {
        self.created.push(body);
        body
    }

    /// Begins a user-grouped transaction.
    ///
    /// Multi-step operations (e.g. mouse drag transforms) grouped between
    /// `begin_transaction` and `commit_transaction` form a single undo step.
    pub fn begin_transaction(&mut self, label: &'static str) {
        self.history.begin(label);
    }

    /// Commits a user-grouped transaction.
    pub fn commit_transaction(&mut self) {
        self.history.commit();
    }

    fn insert(&mut self, label: &'static str, node: Node) -> NodeId {
        self.history.begin(label);
        let id = self.nodes.insert(node.clone());
        self.history.record(Edit::Insert { id, node });
        self.history.commit();
        id
    }

    // ---- construction -------------------------------------------------

    pub fn add_box(&mut self, name: impl Into<String>, size: Vec3) -> Result<NodeId> {
        let body = self.kernel.create_box(size)?;
        let body = self.track(body);
        Ok(self.insert(
            "Add box",
            Node {
                name: name.into(),
                body,
                visible: true,
            },
        ))
    }

    pub fn add_sphere(&mut self, name: impl Into<String>, radius: f64) -> Result<NodeId> {
        let body = self.kernel.create_sphere(radius)?;
        let body = self.track(body);
        Ok(self.insert(
            "Add sphere",
            Node {
                name: name.into(),
                body,
                visible: true,
            },
        ))
    }

    pub fn add_cylinder(
        &mut self,
        name: impl Into<String>,
        radius: f64,
        height: f64,
    ) -> Result<NodeId> {
        let body = self.kernel.create_cylinder(radius, height)?;
        let body = self.track(body);
        Ok(self.insert(
            "Add cylinder",
            Node {
                name: name.into(),
                body,
                visible: true,
            },
        ))
    }

    // ---- editing ------------------------------------------------------

    /// Consumes both operands as *nodes* and leaves one behind. The kernel
    /// bodies underneath both survive, which is what lets undo put the nodes
    /// back without re-running the boolean.
    pub fn boolean(&mut self, op: BooleanOp, a: NodeId, b: NodeId) -> Result<NodeId> {
        let (na, nb) = (self.node(a)?.clone(), self.node(b)?.clone());
        // Everything that can fail happens before the arena is touched, so a
        // failed operation leaves no half-applied transaction behind.
        let body = self.kernel.boolean(op, na.body, nb.body, self.tolerance)?;
        let body = self.track(body);

        let name = match op {
            BooleanOp::Union => format!("{} ∪ {}", na.name, nb.name),
            BooleanOp::Difference => format!("{} − {}", na.name, nb.name),
            BooleanOp::Intersection => format!("{} ∩ {}", na.name, nb.name),
        };
        let label = match op {
            BooleanOp::Union => "Union",
            BooleanOp::Difference => "Difference",
            BooleanOp::Intersection => "Intersection",
        };

        self.history.begin(label);
        self.nodes.remove(a);
        self.history.record(Edit::Remove { id: a, node: na });
        self.nodes.remove(b);
        self.history.record(Edit::Remove { id: b, node: nb });
        let node = Node {
            name,
            body,
            visible: true,
        };
        let id = self.nodes.insert(node.clone());
        self.history.record(Edit::Insert { id, node });
        self.history.commit();
        Ok(id)
    }

    pub fn transform(&mut self, id: NodeId, m: &Mat4) -> Result<()> {
        let before = self.node(id)?.clone();
        let body = self.kernel.transform(before.body, m)?;
        let body = self.track(body);
        let after = Node {
            body,
            ..before.clone()
        };
        self.replace(id, "Transform", before, after);
        Ok(())
    }

    pub fn fillet(&mut self, id: NodeId, radius: f64) -> Result<()> {
        let before = self.node(id)?.clone();
        let body = self.kernel.fillet(before.body, radius)?;
        let body = self.track(body);
        let after = Node {
            body,
            ..before.clone()
        };
        self.replace(id, "Fillet", before, after);
        Ok(())
    }

    pub fn rename(&mut self, id: NodeId, name: impl Into<String>) -> Result<()> {
        let before = self.node(id)?.clone();
        let after = Node {
            name: name.into(),
            ..before.clone()
        };
        self.replace(id, "Rename", before, after);
        Ok(())
    }

    pub fn set_visible(&mut self, id: NodeId, visible: bool) -> Result<()> {
        let before = self.node(id)?.clone();
        let after = Node {
            visible,
            ..before.clone()
        };
        self.replace(id, "Visibility", before, after);
        Ok(())
    }

    fn replace(&mut self, id: NodeId, label: &'static str, before: Node, after: Node) {
        if before == after {
            return;
        }
        self.history.begin(label);
        if let Some(slot) = self.nodes.get_mut(id) {
            *slot = after.clone();
        }
        self.history.record(Edit::Replace { id, before, after });
        self.history.commit();
    }

    pub fn remove(&mut self, id: NodeId) -> Result<()> {
        let node = self.node(id)?.clone();
        self.history.begin("Delete");
        self.nodes.remove(id);
        self.history.record(Edit::Remove { id, node });
        self.history.commit();
        Ok(())
    }

    // ---- interchange --------------------------------------------------

    /// Writes these nodes as a STEP file, for another program to read.
    ///
    /// Nothing but geometry crosses: names, visibility, the tolerance, the
    /// document's structure and its history are all this document's and none
    /// of them are in a STEP file. What comes back from a round-trip is
    /// solids, in order, and that is the trade the format is for.
    pub fn export_step(&self, ids: &[NodeId]) -> Result<Vec<u8>> {
        let bodies = ids
            .iter()
            .map(|id| self.body_of(*id))
            .collect::<Result<Vec<Body>>>()?;
        Ok(self.kernel.export_step(&bodies)?)
    }

    /// Adds one node per solid in a STEP file, named after `name`.
    ///
    /// **One transaction**, so an import of forty solids is one undo and not
    /// forty. That is not a general fix for grouping — every other edit here
    /// is still its own step — it is that an import has an obvious boundary
    /// and undoing it halfway is not a state anybody asked for.
    ///
    /// The nodes are appended: importing adds to the document rather than
    /// replacing it, which is the opposite of opening a `.w3d` and is the
    /// difference between the two operations. A `.w3d` carries a whole
    /// document, kernel and all; a STEP file carries solids.
    pub fn import_step(&mut self, bytes: &[u8], name: &str) -> Result<Vec<NodeId>> {
        let imported = self.kernel.import_step(bytes)?;
        let one = imported.len() == 1;

        let is_generic = |s: &str| {
            let t = s.trim();
            t.is_empty() || t.starts_with("Open CASCADE STEP translator")
        };

        self.history.begin("Import STEP");
        let mut ids = Vec::with_capacity(imported.len());
        for (n, imp) in imported.into_iter().enumerate() {
            self.created.push(imp.body);
            let node_name = match imp.name {
                Some(pname) if !is_generic(&pname) => pname,
                _ => {
                    if one {
                        name.to_string()
                    } else {
                        format!("{name} {}", n + 1)
                    }
                }
            };
            let node = Node {
                name: node_name,
                body: imp.body,
                visible: true,
            };
            let id = self.nodes.insert(node.clone());
            self.history.record(Edit::Insert { id, node });
            ids.push(id);
        }
        self.history.commit();
        Ok(ids)
    }

    // ---- history ------------------------------------------------------

    pub fn undo(&mut self) -> Option<&'static str> {
        self.history.undo(&mut self.nodes)
    }

    pub fn redo(&mut self) -> Option<&'static str> {
        self.history.redo(&mut self.nodes)
    }

    /// Forgets every undo step. The bodies history was holding become
    /// collectable, so this is the other half of `collect_garbage` — the
    /// application decides when memory is worth more than the ability to go
    /// back.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Compacts the document's node arena by removing dead tombstone slots and
    /// re-indexing live nodes.
    ///
    /// # Errors
    /// Returns [`DocumentError::HistoryNotEmpty`] if history is not empty.
    /// History must be cleared via [`Document::clear_history`] prior to compaction,
    /// because re-indexing slot handles invalidates history undo handles.
    ///
    /// Returns the number of freed slots.
    pub fn compact(&mut self) -> Result<usize> {
        if self.history.can_undo() || self.history.can_redo() {
            return Err(DocumentError::HistoryNotEmpty);
        }
        let freed_slots = self.nodes.slot_count() - self.nodes.len();
        let id_map = self.nodes.compact();

        // Remap selection set
        let mut new_selection = Vec::with_capacity(self.selection.len());
        for old_id in &self.selection {
            if let Some(new_id) = id_map.get(old_id) {
                new_selection.push(*new_id);
            }
        }
        new_selection.sort_unstable();
        new_selection.dedup();
        self.selection = new_selection;

        Ok(freed_slots)
    }

    // ---- selection ----------------------------------------------------

    /// Sorted and deduplicated, and filtered to nodes that still exist —
    /// undoing past a node's creation must not leave it selected.
    pub fn selection(&self) -> impl Iterator<Item = NodeId> {
        self.selection
            .iter()
            .copied()
            .filter(|id| self.nodes.contains(*id))
    }

    pub fn is_selected(&self, id: NodeId) -> bool {
        self.nodes.contains(id) && self.selection.binary_search(&id).is_ok()
    }

    pub fn select(&mut self, id: NodeId) -> Result<()> {
        if !self.nodes.contains(id) {
            return Err(DocumentError::UnknownNode(id));
        }
        if let Err(at) = self.selection.binary_search(&id) {
            self.selection.insert(at, id);
        }
        Ok(())
    }

    pub fn deselect(&mut self, id: NodeId) {
        if let Ok(at) = self.selection.binary_search(&id) {
            self.selection.remove(at);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    // ---- queries ------------------------------------------------------

    pub fn bounds(&self, id: NodeId) -> Result<Aabb> {
        Ok(self.kernel.bounds(self.body_of(id)?)?)
    }

    pub fn topology(&self, id: NodeId) -> Result<Topology> {
        Ok(self.kernel.topology(self.body_of(id)?)?)
    }

    /// The bounds of everything visible — what a "zoom to fit" needs.
    pub fn visible_bounds(&self) -> Aabb {
        self.nodes
            .iter()
            .filter(|(_, n)| n.visible)
            .filter_map(|(_, n)| self.kernel.bounds(n.body).ok())
            .fold(Aabb::EMPTY, |acc, b| acc.union(&b))
    }

    /// Tessellates on first ask and caches by body.
    pub fn mesh(&mut self, id: NodeId) -> Result<&Mesh> {
        let body = self.body_of(id)?;
        if !self.meshes.contains_key(&body.raw()) {
            let mesh = self.kernel.tessellate(body, self.quality)?;
            self.meshes.insert(body.raw(), mesh);
        }
        Ok(&self.meshes[&body.raw()])
    }

    pub fn cached_mesh_count(&self) -> usize {
        self.meshes.len()
    }

    // ---- storage ------------------------------------------------------

    /// Deletes every kernel body no live node and no history entry refers to,
    /// and returns how many went.
    ///
    /// This is the price of state-based undo: intermediate bodies stay alive
    /// as long as something can undo back to them. Nothing calls this
    /// automatically — when to pay it is a policy the application owns, not
    /// the document.
    pub fn collect_garbage(&mut self) -> usize {
        let referenced: HashSet<u32> = self
            .nodes
            .iter()
            .map(|(_, n)| n.body.raw())
            .chain(self.history.bodies().map(|b| b.raw()))
            .collect();

        let created = core::mem::take(&mut self.created);
        let mut deleted = 0;
        for body in created {
            if referenced.contains(&body.raw()) {
                self.created.push(body);
            } else if self.kernel.delete(body).is_ok() {
                self.meshes.remove(&body.raw());
                deleted += 1;
            }
        }
        deleted
    }
}
