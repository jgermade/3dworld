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

/// A node's identity **in a file**, and the only one that survives being
/// written down.
///
/// [`NodeId`] is an arena slot and a generation. It is exactly right for as
/// long as this document is in memory and worth nothing the moment it leaves:
/// two sessions that build the same document hand out the same slots for
/// different reasons, and a slot an undo is holding must not be handed out at
/// all. A `Uid` is what a parent in a `.w3d` file refers to, and what anything
/// outside the document — a selection remembered between sessions, a reference
/// from another file — has to hold.
///
/// It is a counter rather than a random number, for the property the container
/// was chosen for: two saves of the same document must produce identical
/// bytes. The counter is saved with the document and never rewinds, not even
/// when an undo takes the node that spent it — an identity handed out once may
/// already be written down somewhere this program cannot see. What it costs is
/// that two documents cannot be merged without renumbering one of them, and
/// merging documents is a feature that does not exist.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Uid(u64);

impl Uid {
    /// A node that is not in a document yet. [`Document`] stamps a real one on
    /// the way into the arena, and that is the only place identities are made.
    pub const UNASSIGNED: Uid = Uid(0);

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// For a writer. Readers in languages whose numbers are doubles are exact
    /// to 2^53, which is more nodes than anything here will create.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for Uid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What one unit of this document's numbers means.
///
/// A document used to be unitless: `tolerance` implied a scale and nothing
/// stated one, while STEP export wrote millimetres regardless — so the
/// assumption existed, it was just nowhere anybody could read it. This is that
/// assumption written down, and a version-2 file states it.
///
/// **Nothing converts.** Changing a document's unit relabels its numbers; it
/// does not scale its geometry, which would be a rebuild. What the unit buys
/// today is that a file says what it means and that an export which cannot
/// honour it refuses — see [`Document::export_step`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Millimetre,
    Centimetre,
    Metre,
    Inch,
    Foot,
}

impl Unit {
    /// The symbol a file and a status line both use. Stable: it is written
    /// into documents, so changing one of these strings is changing the format.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Millimetre => "mm",
            Self::Centimetre => "cm",
            Self::Metre => "m",
            Self::Inch => "in",
            Self::Foot => "ft",
        }
    }

    pub fn from_symbol(symbol: &str) -> Option<Self> {
        [
            Self::Millimetre,
            Self::Centimetre,
            Self::Metre,
            Self::Inch,
            Self::Foot,
        ]
        .into_iter()
        .find(|u| u.symbol() == symbol)
    }

    /// How many millimetres one of these is. Exact for the imperial pair
    /// because the inch *is* 25.4 mm by definition, not by measurement.
    pub const fn millimetres(self) -> f64 {
        match self {
            Self::Millimetre => 1.0,
            Self::Centimetre => 10.0,
            Self::Metre => 1000.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
        }
    }
}

/// One solid in the document, with the things a user gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// The identity that survives a save. [`Uid::UNASSIGNED`] until the node
    /// is in a document, which is the only thing that hands them out.
    pub uid: Uid,
    pub name: String,
    pub body: Body,
    pub visible: bool,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

/// The handle a group node carries where a body node carries geometry.
///
/// A group is a node with no solid in it, and this is how it says so. It is
/// `u32::MAX` and not `0` for a blunt reason: `0` is the first id every backend
/// here hands out — `OcctKernel`'s counter starts there — so a group used to
/// carry the handle of the *first body in the document*. Ten groups from an
/// imported assembly meant ten phantom copies of whatever that body was, drawn,
/// measured by `visible_bounds`, and kept alive by `collect_garbage` forever.
/// Nothing will allocate four billion bodies to collide with this one, and the
/// kernel's own "no body is at fault" handle is already `u32::MAX`.
pub const GROUP_BODY: Body = Body::from_raw(u32::MAX);

impl Node {
    /// Whether this node is a group — structure with no geometry of its own.
    ///
    /// Callers that walk every node and ask the kernel about each body want
    /// this: the kernel will refuse [`GROUP_BODY`], and a refusal is a worse
    /// answer than a question not asked.
    pub fn is_group(&self) -> bool {
        self.body == GROUP_BODY
    }

    pub fn new(name: impl Into<String>, body: Body) -> Self {
        Self {
            uid: Uid::UNASSIGNED,
            name: name.into(),
            body,
            visible: true,
            parent: None,
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentError {
    UnknownNode(NodeId),
    HistoryNotEmpty,
    Kernel(KernelError),
    /// A reparent that would put a node inside itself.
    Cycle {
        child: NodeId,
        parent: NodeId,
    },
    /// A file's nodes do not make a tree. Carries the first breach, naming the
    /// entry at fault rather than the file.
    NotATree(String),
    /// A STEP file states millimetres and this document says it is in
    /// something else. Refused rather than written wrong.
    NotMillimetres(Unit),
}

impl core::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "no such node: {id:?}"),
            Self::HistoryNotEmpty => write!(f, "cannot compact arena while history is non-empty"),
            Self::Kernel(e) => write!(f, "{e}"),
            Self::Cycle { child, parent } => write!(
                f,
                "{child:?} cannot go inside {parent:?}, because {parent:?} is already inside \
                 {child:?}"
            ),
            Self::NotATree(why) => write!(f, "these nodes are not a tree: {why}"),
            Self::NotMillimetres(unit) => write!(
                f,
                "a STEP file states millimetres and this document is in {}. Exporting it would \
                 write numbers that mean something else.",
                unit.symbol()
            ),
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
    /// What one of this document's numbers means, or `None` where nobody has
    /// said — which is every version-1 file, because version 1 had nowhere to
    /// write it.
    unit: Option<Unit>,
    /// The identity the next node to arrive will take. Saved with the document
    /// and never rewound: see [`Uid`].
    next_uid: u64,
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
            // Millimetres because that is what this program already assumed
            // everywhere it had to choose — `export_step` writes them — and an
            // assumption stated is one a user can disagree with.
            unit: Some(Unit::Millimetre),
            next_uid: 1,
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
            // A group is structure and carries no solid, so there is nothing
            // for the kernel to be asked about later.
            if !node.is_group() {
                doc.created.push(node.body);
            }
            doc.place(node);
        }
        doc
    }

    /// Rebuilds a document from a file that has a tree in it.
    ///
    /// [`Document::from_parts`] is this without the structure: it takes nodes
    /// that are all roots and hands each a fresh identity. This one takes the
    /// file's own identities and its parents, and is fallible because a file is
    /// not something this program gets to assume anything about — the whole of
    /// what it may assume is [`Loaded::validate`], and it is checked before the
    /// arena is touched so that a refused file leaves nothing half-built.
    ///
    /// **History starts empty**, for the reason in [`Document::from_parts`].
    pub fn from_loaded(kernel: K, loaded: Loaded) -> Result<Self> {
        loaded.validate().map_err(DocumentError::NotATree)?;

        let mut doc = Self::new(kernel);
        doc.tolerance = loaded.tolerance;
        doc.quality = loaded.quality;
        doc.unit = loaded.unit;
        doc.next_uid = loaded.next_uid.raw();

        // One pass, because `validate` has already established that a parent
        // appears before its children: the parent's `NodeId` is always in this
        // map by the time a child asks for it. That is the same rule, and for
        // the same reason, as the one the kernel seam's `Import` carries.
        let mut by_uid: HashMap<Uid, NodeId> = HashMap::new();
        for entry in loaded.nodes {
            let parent = entry.parent.map(|uid| by_uid[&uid]);
            let mut node = Node::new(entry.name, entry.body.unwrap_or(GROUP_BODY));
            node.uid = entry.uid;
            node.visible = entry.visible;
            node.parent = parent;
            if !node.is_group() {
                doc.created.push(node.body);
            }
            let (id, _) = doc.place(node);
            by_uid.insert(entry.uid, id);
            if let Some(slot) = parent.and_then(|parent| doc.nodes.get_mut(parent)) {
                slot.children.push(id);
            }
        }
        Ok(doc)
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

    /// What one of this document's numbers means, or `None` where nothing has
    /// said. A version-1 file is the second case, and so is anything built
    /// before units existed; a new document is millimetres.
    pub fn unit(&self) -> Option<Unit> {
        self.unit
    }

    /// Relabels the document's numbers. It does **not** scale any geometry —
    /// see [`Unit`] — so this says what the numbers already meant, and a user
    /// who wanted a conversion did not get one.
    pub fn set_unit(&mut self, unit: Option<Unit>) {
        self.unit = unit;
    }

    /// The identity the next node created here will be given. Saved with the
    /// document, because a reader that resumed from the largest identity
    /// *present* would hand a deleted node's identity to a new node.
    pub fn next_uid(&self) -> Uid {
        Uid(self.next_uid)
    }

    /// Finds a node by the identity that survives a save.
    ///
    /// Linear, deliberately: a document is a few thousand nodes and an index
    /// is another thing undo would have to keep right.
    pub fn by_uid(&self, uid: Uid) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|(_, n)| n.uid == uid)
            .map(|(id, _)| id)
    }

    /// Every node, parents before children and siblings in their own order —
    /// which is the order a file must list them in, and the order an outliner
    /// draws. The `usize` beside each is its depth, zero at a root.
    ///
    /// Iterative rather than recursive on purpose: the depth of this tree is
    /// the depth of whatever file was opened, so a deep one is a stack
    /// overflow in a recursive walk and a few more pushes here.
    pub fn depth_first(&self) -> Vec<(NodeId, usize)> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<(NodeId, usize)> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.parent.is_none())
            .map(|(id, _)| (id, 0usize))
            .collect();
        stack.reverse();
        while let Some((id, depth)) = stack.pop() {
            out.push((id, depth));
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            for &child in node.children.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        out
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

    /// Hands out an identity that survives a save, and spends it.
    fn mint(&mut self) -> Uid {
        let uid = Uid(self.next_uid);
        self.next_uid += 1;
        uid
    }

    /// Stamps a node with an identity unless it arrived with one, and puts it
    /// in the arena.
    ///
    /// **Every way a node enters this document goes through here**, which is
    /// what makes "every node in a document has an identity" a property rather
    /// than a hope. A node that arrives with one keeps it: that is a file being
    /// loaded, and the identities in a file are the file's.
    fn place(&mut self, mut node: Node) -> (NodeId, Node) {
        if node.uid == Uid::UNASSIGNED {
            node.uid = self.mint();
        }
        let id = self.nodes.insert(node.clone());
        (id, node)
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
        let (id, node) = self.place(node);
        self.history.record(Edit::Insert { id, node });
        self.history.commit();
        id
    }

    // ---- construction -------------------------------------------------

    pub fn add_group(&mut self, name: impl Into<String>) -> NodeId {
        self.insert("Add Group", Node::new(name, GROUP_BODY))
    }

    pub fn reparent(&mut self, child_id: NodeId, new_parent_id: Option<NodeId>) -> Result<()> {
        if !self.nodes.contains(child_id) {
            return Err(DocumentError::UnknownNode(child_id));
        }
        if let Some(parent_id) = new_parent_id {
            if !self.nodes.contains(parent_id) {
                return Err(DocumentError::UnknownNode(parent_id));
            }
            if child_id == parent_id {
                return Ok(());
            }
            // A node may not be put inside its own descendant. Refusing only
            // `child == parent` leaves the two-step version — drag A onto B,
            // then B onto A — and what it produces is a ring of nodes that no
            // longer reaches the root: the Outliner walks down from the roots
            // and would never terminate, and neither would anything else that
            // followed `parent` up. It became reachable from the mouse when the
            // Outliner learned to draw a tree.
            let mut up = Some(parent_id);
            while let Some(id) = up {
                if id == child_id {
                    return Err(DocumentError::Cycle {
                        child: child_id,
                        parent: parent_id,
                    });
                }
                up = self.nodes.get(id).and_then(|n| n.parent);
            }
        }

        let old_parent = self.nodes.get(child_id).and_then(|n| n.parent);
        if old_parent == new_parent_id {
            return Ok(());
        }

        // One transaction over up to three nodes — the child, the parent it
        // leaves, the parent it joins — because moving a node is one act and
        // half of it is not a state to undo into.
        //
        // It records history at all as of 2026-09-13. Until then this was the
        // one editing operation outside undo: a part dragged into the wrong
        // group in the Outliner stayed there, and Ctrl-Z reversed whatever the
        // user had done *before* the drag, which is worse than nothing
        // happening. Nothing in the document noticed, because the tree was
        // reachable from one button and nothing that could be undone used it.
        self.history.begin("Reparent");
        if let Some(old_p) = old_parent {
            self.amend(old_p, |n| n.children.retain(|&id| id != child_id));
        }
        self.amend(child_id, |n| n.parent = new_parent_id);
        if let Some(new_p) = new_parent_id {
            self.amend(new_p, |n| {
                if !n.children.contains(&child_id) {
                    n.children.push(child_id);
                }
            });
        }
        self.history.commit();

        Ok(())
    }

    /// Changes one node in place, recording the before and after so that undo
    /// and redo can move between them.
    ///
    /// Assumes an open transaction. A node that is not there is not an error
    /// here: every caller has already established that it is, and a silent
    /// no-op is better than a panic in a tree walk.
    fn amend(&mut self, id: NodeId, change: impl FnOnce(&mut Node)) {
        let Some(before) = self.nodes.get(id).cloned() else {
            return;
        };
        let mut after = before.clone();
        change(&mut after);
        if after == before {
            return;
        }
        if let Some(slot) = self.nodes.get_mut(id) {
            *slot = after.clone();
        }
        self.history.record(Edit::Replace { id, before, after });
    }

    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id).and_then(|n| n.parent)
    }

    pub fn children_of(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |n| &n.children)
    }

    pub fn add_box(&mut self, name: impl Into<String>, size: Vec3) -> Result<NodeId> {
        let body = self.kernel.create_box(size)?;
        let body = self.track(body);
        Ok(self.insert("Add box", Node::new(name, body)))
    }

    pub fn add_sphere(&mut self, name: impl Into<String>, radius: f64) -> Result<NodeId> {
        let body = self.kernel.create_sphere(radius)?;
        let body = self.track(body);
        Ok(self.insert("Add sphere", Node::new(name, body)))
    }

    pub fn add_cylinder(
        &mut self,
        name: impl Into<String>,
        radius: f64,
        height: f64,
    ) -> Result<NodeId> {
        let body = self.kernel.create_cylinder(radius, height)?;
        let body = self.track(body);
        Ok(self.insert("Add cylinder", Node::new(name, body)))
    }

    pub fn add_extrude(
        &mut self,
        name: impl Into<String>,
        profile: &w3d_kernel::Profile,
        distance: f64,
    ) -> Result<NodeId> {
        let body = self.kernel.extrude(profile, distance)?;
        let body = self.track(body);
        Ok(self.insert("Add Extrude", Node::new(name, body)))
    }

    pub fn add_revolve(
        &mut self,
        name: impl Into<String>,
        profile: &w3d_kernel::Profile,
        axis_origin: Vec3,
        axis_dir: Vec3,
        angle_rad: f64,
    ) -> Result<NodeId> {
        let body = self
            .kernel
            .revolve(profile, axis_origin, axis_dir, angle_rad)?;
        let body = self.track(body);
        Ok(self.insert("Add Revolve", Node::new(name, body)))
    }

    pub fn add_sweep(
        &mut self,
        name: impl Into<String>,
        profile: &w3d_kernel::Profile,
        path_points: &[Vec3],
    ) -> Result<NodeId> {
        let body = self.kernel.sweep(profile, path_points)?;
        let body = self.track(body);
        Ok(self.insert("Add Sweep", Node::new(name, body)))
    }

    pub fn add_loft(
        &mut self,
        name: impl Into<String>,
        profiles: &[w3d_kernel::Profile],
        planes: &[w3d_kernel::SketchPlane],
    ) -> Result<NodeId> {
        let body = self.kernel.loft(profiles, planes)?;
        let body = self.track(body);
        Ok(self.insert("Add Loft", Node::new(name, body)))
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

        // The result takes the place of the first operand: a boolean between
        // two parts of an assembly leaves one part in that assembly, not a
        // loose body at the document's root.
        let parent = na.parent;

        self.history.begin(label);
        self.detach(a, &na);
        self.nodes.remove(a);
        self.history.record(Edit::Remove { id: a, node: na });
        self.detach(b, &nb);
        self.nodes.remove(b);
        self.history.record(Edit::Remove { id: b, node: nb });
        let id = self.insert_child(Node::new(name, body), parent);
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

    pub fn chamfer(&mut self, id: NodeId, distance: f64) -> Result<()> {
        let before = self.node(id)?.clone();
        let body = self.kernel.chamfer(before.body, distance)?;
        let body = self.track(body);
        let after = Node {
            body,
            ..before.clone()
        };
        self.replace(id, "Chamfer", before, after);
        Ok(())
    }

    pub fn shell(&mut self, id: NodeId, face_id: u32, thickness: f64) -> Result<()> {
        let before = self.node(id)?.clone();
        let body = self.kernel.shell(before.body, face_id, thickness)?;
        let body = self.track(body);
        let after = Node {
            body,
            ..before.clone()
        };
        self.replace(id, "Shell", before, after);
        Ok(())
    }

    pub fn push_pull_face(&mut self, id: NodeId, face_id: u32, distance: f64) -> Result<()> {
        let mesh = self.mesh(id)?.clone();
        let metrics = mesh
            .face_metrics(face_id)
            .ok_or_else(|| KernelError::Failed(format!("face #{face_id} not found on body")))?;
        let translation = metrics.normal * distance;
        let m = Mat4::from_translation(translation);
        self.transform(id, &m)
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
        self.detach(id, &node);
        self.nodes.remove(id);
        self.history.record(Edit::Remove { id, node });
        self.history.commit();
        Ok(())
    }

    /// Takes a node out of the tree before it leaves the arena: its parent
    /// stops listing it, and whatever was inside it is promoted into its place.
    ///
    /// Until 2026-09-14 neither happened, and the state it left is one nothing
    /// downstream survives. A removed group's children kept a `parent` naming a
    /// slot that is no longer there, so every walk that starts at the roots —
    /// the Outliner's, and now the writer's — could not reach them: nodes that
    /// were still in the document, still drawn in the viewport, and invisible
    /// to the one panel that lists what a document contains. It became
    /// load-bearing when a file grew a tree, because a node a walk cannot reach
    /// is a node a save would silently drop.
    ///
    /// Promotion rather than deleting the subtree: a delete that takes bodies
    /// the user did not select with it is the worse of the two surprises, and
    /// this one is reversible by the same undo step.
    ///
    /// Assumes an open transaction.
    fn detach(&mut self, id: NodeId, node: &Node) {
        let promoted = node.children.clone();
        for child in &promoted {
            self.amend(*child, |n| n.parent = node.parent);
        }
        if let Some(parent) = node.parent {
            self.amend(parent, move |n| {
                match n.children.iter().position(|&c| c == id) {
                    // In its place, so that the order siblings are drawn in is
                    // the order they were in.
                    Some(at) => {
                        n.children.splice(at..=at, promoted);
                    }
                    None => n.children.extend(promoted),
                }
            });
        }
    }

    // ---- interchange --------------------------------------------------

    /// Writes these nodes as a STEP file, for another program to read.
    ///
    /// Nothing but geometry crosses: names, visibility, the tolerance, the
    /// document's structure and its history are all this document's and none
    /// of them are in a STEP file. What comes back from a round-trip is
    /// solids, in order, and that is the trade the format is for.
    pub fn export_step(&self, ids: &[NodeId]) -> Result<Vec<u8>> {
        // A STEP file states its unit and this writer states millimetres, so a
        // document that says it is in something else would be exported as
        // numbers meaning something they do not. Nothing here scales geometry
        // — that is a rebuild — so the honest answer is to refuse. A document
        // that states nothing is the case this program has always been in, and
        // millimetres is the assumption it has always made.
        if let Some(unit) = self.unit
            && unit != Unit::Millimetre
        {
            return Err(DocumentError::NotMillimetres(unit));
        }
        let bodies = ids
            .iter()
            .map(|id| self.body_of(*id))
            .collect::<Result<Vec<Body>>>()?;
        Ok(self.kernel.export_step(&bodies)?)
    }

    /// Adds one node per solid in a STEP file, in the assembly tree the file
    /// held, named after `name` where the file names nothing.
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
    ///
    /// **A group node per assembly**, and the bodies under the group they sat
    /// in. A group is a node with no geometry, so an imported assembly costs
    /// one node per interior node of the file's tree and buys the one thing a
    /// flat import cannot offer: hiding, selecting or deleting a subassembly as
    /// the thing it is. Where the file is flat, or where a backend's reader
    /// cannot see a tree, nothing is grouped and this behaves as it did.
    ///
    /// What it does **not** do is share geometry between two placements of one
    /// part: the seam hands over a body per placement, so an assembly holding
    /// five parts eighteen times arrives as eighteen independent bodies that
    /// happen to have the same shape. The tree says how they are arranged, not
    /// that any two of them are the same thing.
    ///
    /// Returns the body nodes, not the groups — they are what a user selects,
    /// and [`Document::parent_of`] reaches the groups from any of them.
    pub fn import_step(&mut self, bytes: &[u8], name: &str) -> Result<Vec<NodeId>> {
        let imported = self.kernel.import_step(bytes)?;
        // The seam's structural rule, re-checked on this side of it. A backend
        // is obliged to hand over a tree that holds together, and this is the
        // last place that can say so with the file still in hand rather than
        // panicking on an index much later.
        if let Err(why) = imported.validate() {
            return Err(DocumentError::Kernel(KernelError::Failed(format!(
                "the imported assembly tree is not one: {why}"
            ))));
        }
        let one = imported.bodies.len() == 1;

        let is_generic = |s: &str| {
            let t = s.trim();
            t.is_empty() || t.starts_with("Open CASCADE STEP translator")
        };

        self.history.begin("Import STEP");

        // Assemblies first, and in order: the seam guarantees a parent appears
        // before its children, so the parent's `NodeId` is always already here
        // by the time a child needs it. That is the whole reason the contract
        // asks for that ordering.
        let mut groups: Vec<NodeId> = Vec::with_capacity(imported.assemblies.len());
        for (n, asm) in imported.assemblies.iter().enumerate() {
            let group_name = match &asm.name {
                Some(pname) if !is_generic(pname) => pname.clone(),
                _ => format!("{name} assembly {}", n + 1),
            };
            let parent = asm.parent.map(|p| groups[p]);
            let id = self.insert_child(Node::new(group_name, GROUP_BODY), parent);
            groups.push(id);
        }

        let mut ids = Vec::with_capacity(imported.bodies.len());
        for (n, imp) in imported.bodies.into_iter().enumerate() {
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
            let parent = imp.parent.map(|p| groups[p]);
            ids.push(self.insert_child(Node::new(node_name, imp.body), parent));
        }
        self.history.commit();
        Ok(ids)
    }

    /// Inserts `node` under `parent`, recording enough for undo to reverse the
    /// link as well as the node.
    ///
    /// The link is two facts in two nodes — the child's `parent` and the
    /// parent's `children` — and both are recorded: the child's by inserting it
    /// already linked, so its `Insert` edit carries it, and the parent's as a
    /// `Replace` of the parent against itself. Undoing an import therefore
    /// leaves no group holding a child id that has been removed, which is the
    /// state that would outlive the transaction otherwise.
    ///
    /// Assumes an open transaction; the callers here all open one.
    fn insert_child(&mut self, mut node: Node, parent: Option<NodeId>) -> NodeId {
        node.parent = parent;
        let (id, node) = self.place(node);
        self.history.record(Edit::Insert { id, node });
        if let Some(parent) = parent {
            self.amend(parent, |n| n.children.push(id));
        }
        id
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
            .filter(|(_, n)| n.visible && !n.is_group())
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
            .filter(|(_, n)| !n.is_group())
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

/// A document as a file holds it, before anything has been built from it.
///
/// The counterpart of [`w3d_kernel::Import`] one level up: something arrived
/// from outside claiming to be a tree, and there is exactly one rule it has to
/// satisfy before any of it lands. Keeping that rule here rather than in the
/// reader is deliberate — the document is what must not be built broken, and
/// this is testable with no file, no zip and no JSON anywhere near it.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub tolerance: Tolerance,
    pub quality: Quality,
    /// What the file said one of its numbers means, or `None` where it did not
    /// say — which is every version-1 file.
    pub unit: Option<Unit>,
    /// The identity the next node created in the rebuilt document must take.
    pub next_uid: Uid,
    /// **Parents before children**, and in the order they are to be drawn.
    pub nodes: Vec<LoadedNode>,
}

/// One node as a file holds it: an identity, a place in the tree, and the
/// geometry it stands for — or none, where it is a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedNode {
    pub uid: Uid,
    pub name: String,
    /// `None` for a group: structure, with no solid under it. A body must be
    /// one this document's kernel just produced, because a [`Body`] means
    /// nothing to any other kernel.
    pub body: Option<Body>,
    pub visible: bool,
    /// The node this one sits inside, by identity, and always one that appears
    /// **earlier in the list** — see [`Loaded::validate`].
    pub parent: Option<Uid>,
}

impl Loaded {
    /// The structural rule of a document's tree, and the whole of it.
    ///
    /// Returns the first breach as a sentence naming the entry at fault, so
    /// that a refusal points at a node rather than at the file. Four things,
    /// and each of them is a way a document could be built that nothing
    /// downstream survives:
    ///
    /// - **Every node has an identity.** `0` is what an unplaced node carries,
    ///   so a file offering it is offering a node with no name to be referred
    ///   to by.
    /// - **No two nodes share one.** Identity that is not unique is not
    ///   identity, and the second node would take the first's children.
    /// - **A parent appears earlier in the list than its children.** Stronger
    ///   than "a parent exists", and stronger on purpose: it makes a cycle
    ///   *unrepresentable* rather than detectable, so the tree can be built in
    ///   one pass with no visited set and no recursion. It is the same rule,
    ///   for the same reason, that [`w3d_kernel::Import`] carries at the seam.
    /// - **The counter is ahead of every identity present.** A file whose
    ///   counter is behind one would hand a live node's identity to the next
    ///   node created, which is the bug identity exists to prevent.
    pub fn validate(&self) -> core::result::Result<(), String> {
        let mut seen: HashMap<Uid, usize> = HashMap::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            if node.uid == Uid::UNASSIGNED {
                return Err(format!("node {i} has no identity"));
            }
            // The parent is looked for **before** this node is put in `seen`,
            // which is what makes "earlier in the list" exclude the node
            // itself. Doing it the other way round let a node be its own
            // parent — a one-node cycle, which is exactly the thing this rule
            // exists to make unwritable, and which the negative control caught.
            if let Some(parent) = node.parent
                && !seen.contains_key(&parent)
            {
                return Err(format!(
                    "node {i} says it is inside {parent}, which is not a node earlier in the \
                     list, so these are not a tree"
                ));
            }
            if let Some(first) = seen.insert(node.uid, i) {
                return Err(format!(
                    "node {i} and node {first} are both {}, and an identity that is not unique \
                     is not one",
                    node.uid
                ));
            }
            if node.uid >= self.next_uid {
                return Err(format!(
                    "node {i} is {} and the next identity to hand out is {}, so the next node \
                     created would be given one that is already taken",
                    node.uid, self.next_uid
                ));
            }
        }
        Ok(())
    }
}
