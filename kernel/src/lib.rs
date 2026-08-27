//! The seam.
//!
//! Everything above this crate — the document, history, selection, rendering,
//! the modeller itself — is written against [`GeometryKernel`] and knows
//! nothing about which backend is underneath. That is what makes the kernel
//! decision reversible; see `AGENTS.md` § Rules that are not style.
//!
//! Two properties of the contract are load-bearing and easy to lose:
//!
//! - **Bodies are immutable.** Every operation produces a new [`Body`] and
//!   leaves its operands intact. Undo is then a matter of keeping the old
//!   handle, not of inverting an operation — which is the only version of undo
//!   a B-rep kernel can implement honestly.
//! - **Tolerance is an argument.** No operation reads a global epsilon, and no
//!   crate above this one invents one.

pub mod math;

#[cfg(feature = "conformance")]
pub mod conformance;

use core::fmt;

pub use math::{Aabb, Mat4, Vec3};

/// An opaque handle to a solid owned by a backend.
///
/// It is deliberately not an index into anything a caller can reach. Backends
/// mint these with [`Body::from_raw`]; nobody else calls it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Body(u32);

impl Body {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "body#{}", self.0)
    }
}

/// How close is "the same".
///
/// Carried explicitly through every predicate. A kernel that reads a global
/// epsilon cannot be asked the same question twice at two scales, and a
/// document at millimetre scale and one at metre scale are the same question
/// at two scales.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    /// Distance below which two points are one point, in model units.
    pub linear: f64,
    /// Angle below which two directions are one direction, in radians.
    pub angular: f64,
}

impl Tolerance {
    pub const fn new(linear: f64, angular: f64) -> Self {
        Self { linear, angular }
    }

    /// The default a *document* starts with — not a constant any predicate may
    /// reach for. Millimetre-scale modelling, matching OCCT's own default.
    pub const fn document_default() -> Self {
        Self::new(1.0e-7, 1.0e-5)
    }
}

/// How finely to approximate curved geometry for display.
///
/// This is display quality, not model precision: it is allowed to be coarse,
/// and changing it must never change the model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    /// Maximum deviation of a chord from the true surface, in model units.
    pub sag: f64,
    /// Maximum angle between adjacent facet normals, in radians.
    pub max_angle: f64,
}

impl Quality {
    pub const fn new(sag: f64, max_angle: f64) -> Self {
        Self { sag, max_angle }
    }

    pub const fn display_default() -> Self {
        Self::new(0.01, 0.35)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    Difference,
    Intersection,
}

/// Counts only. Anything richer belongs behind a method on the trait, so that
/// no caller starts pattern-matching on a backend's own topology types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Topology {
    pub solids: u32,
    pub faces: u32,
    pub edges: u32,
    pub vertices: u32,
}

/// Triangles for the GPU, and the only place `f32` appears in this crate.
///
/// `face_of_triangle` is what makes ID-buffer picking and per-face selection
/// possible at all, so it is part of the contract rather than an extra: a
/// backend that cannot say which face a triangle came from cannot drive the
/// modeller.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub face_of_triangle: Vec<u32>,
    pub line_positions: Vec<[f32; 3]>,
    pub line_indices: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn line_count(&self) -> usize {
        self.line_indices.len() / 2
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum KernelError {
    /// The handle was never valid, or has been deleted.
    UnknownBody(Body),
    /// The input could not describe a solid — a zero radius, a negative extent.
    Degenerate(&'static str),
    /// A real operation this backend does not implement.
    Unsupported(&'static str),
    /// The operation was attempted and failed. Backends put their own message
    /// here; it is for a log, never for a caller to match on.
    Failed(String),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBody(b) => write!(f, "unknown or deleted {b}"),
            Self::Degenerate(what) => write!(f, "degenerate input: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported by this kernel: {what}"),
            Self::Failed(msg) => write!(f, "kernel operation failed: {msg}"),
        }
    }
}

impl core::error::Error for KernelError {}

pub type Result<T> = core::result::Result<T, KernelError>;

/// What a backend must provide for the modeller above it to work.
///
/// Kept narrow on purpose. Every method here is a method some backend has to
/// implement well, so a short list is a short list of things that can be
/// wrong. Widen it when the modeller genuinely needs a capability — declared
/// here first, then implemented, then used.
pub trait GeometryKernel {
    /// For logs and for the conformance report. Not for branching on.
    fn name(&self) -> &'static str;

    /// Origin-centred, `size` across.
    fn create_box(&mut self, size: Vec3) -> Result<Body>;

    /// Origin-centred.
    fn create_sphere(&mut self, radius: f64) -> Result<Body>;

    /// Origin-centred, axis along +Z.
    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body>;

    /// Produces a new body. `a` and `b` remain valid — see the note on
    /// immutability in the crate docs; undo depends on it.
    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, tol: Tolerance) -> Result<Body>;

    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body>;

    fn copy(&mut self, body: Body) -> Result<Body>;

    /// Releases the backend's storage. Callers must not delete a body that any
    /// history entry still refers to.
    fn delete(&mut self, body: Body) -> Result<()>;

    /// Fillets / rounds all sharp edges of a solid with the specified radius.
    ///
    /// The input `body` is unmodified; a new handle is returned for the
    /// resulting solid. If `radius <= 0.0` or exceeds the solid's bounds,
    /// returns `KernelError::Degenerate`.
    fn fillet(&mut self, body: Body, radius: f64) -> Result<Body>;

    /// Chamfers / bevels all sharp edges of a solid with the specified distance.
    ///
    /// The input `body` is unmodified; a new handle is returned for the
    /// resulting solid. If `distance <= 0.0` or exceeds the solid's bounds,
    /// returns `KernelError::Degenerate`.
    fn chamfer(&mut self, body: Body, distance: f64) -> Result<Body>;

    fn topology(&self, body: Body) -> Result<Topology>;

    fn bounds(&self, body: Body) -> Result<Aabb>;

    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh>;

    // ---- persistence --------------------------------------------------
    //
    // A `Body` is a handle into a backend, so a document cannot be saved by
    // writing its handles down. These three are how geometry leaves and
    // re-enters a kernel, and they are the narrowest widening that makes a
    // file possible.
    //
    // What they are deliberately **not** is an interchange format. The bytes
    // are the backend's own — OCCT writes BREP — and another backend must
    // refuse them rather than guess. That is why `geometry_format` exists and
    // why a file records it: a document written by one kernel and opened by
    // another fails with a sentence, instead of silently producing wrong
    // geometry. Moving between kernels is what STEP is for.

    /// Names the format [`GeometryKernel::save_body`] writes, for a file to
    /// record so that a different backend can refuse it.
    ///
    /// A short stable identifier with a version in it, like `occt-brep-1`.
    /// Changing what `save_body` produces means changing this, and a backend
    /// that does not keep that promise breaks every file anyone has saved.
    fn geometry_format(&self) -> &'static str;

    /// Serialises one body into bytes only this kernel need understand.
    ///
    /// Lossless: `load_body` on the result must produce a body with the same
    /// topology and the same bounds, which is what `conformance` checks.
    fn save_body(&self, body: Body) -> Result<Vec<u8>>;

    /// The inverse, producing a new body.
    ///
    /// Returns [`KernelError::Unsupported`] for bytes this kernel does not
    /// recognise and [`KernelError::Failed`] for its own bytes that are
    /// damaged — a caller can tell "wrong kernel" from "broken file", and a
    /// user deserves to know which.
    fn load_body(&mut self, bytes: &[u8]) -> Result<Body>;

    // ---- interchange --------------------------------------------------
    //
    // The pair above moves geometry *through time* — a backend's own bytes,
    // which every other backend must refuse. This pair moves it *between
    // kernels*, and it is the only thing in the contract that does. The two
    // are deliberately not one mechanism with a flag: `save_body` is lossless
    // and unreadable elsewhere, `export_step` is readable everywhere and
    // lossy, and a caller choosing between them is choosing between those two
    // sentences.
    //
    // It is also what makes the file format's refusal message honest. A `.w3d`
    // written by another kernel is refused with "export it to STEP from a
    // build that can open it", and that instruction is only true if the
    // build the user reaches for can read what the other one wrote.
    //
    // **Named for STEP, not for "an exchange format".** There is one case, and
    // an abstraction over one case hides which one it is: what a caller has to
    // know here — that units are stated in the file and nowhere else, that
    // names, history and node structure do not survive, that a solid comes
    // back with its faces renumbered — is knowledge about STEP, not about a
    // genre. A second format, if one is ever owed, gets its own pair and its
    // own paragraph of what it costs.
    //
    // **A backend may honestly not do STEP.** [`KernelError::Unsupported`]
    // from *both* of these is a conforming answer, and `conformance` requires
    // that it be both: a kernel that reads STEP but cannot write it leaves a
    // user's work stuck inside it, and one that writes but cannot read is a
    // door that only opens outwards. Half a bridge is not a bridge.

    /// Writes `bodies` into one ISO 10303-21 file — a STEP file.
    ///
    /// The unit is the backend's to state and the file's to carry: a
    /// [`Body`] is a bare number in a document that has no units, so a
    /// backend that writes STEP is deciding what those numbers meant and
    /// **must say so in the file**. Whatever it decides has to be the same
    /// decision [`GeometryKernel::import_step`] reads back, or a
    /// round-trip scales the model.
    ///
    /// Errors:
    ///
    /// - [`KernelError::Unsupported`] if this backend does not do STEP.
    /// - [`KernelError::Degenerate`] for an empty `bodies`. A STEP file with
    ///   no product in it is a file that opens with nothing in it, three
    ///   programs later, and blames the wrong one.
    /// - [`KernelError::UnknownBody`] if any handle is stale, and *before*
    ///   anything is written: a partial export is worse than a refused one.
    ///
    /// The bodies survive, like every other operation's operands.
    fn export_step(&self, bodies: &[Body]) -> Result<Vec<u8>>;

    /// Reads a STEP file, producing one body per solid in it.
    ///
    /// One per *solid*, not one per file and not one per assembly node: the
    /// document above this crate has a node per body, and a user who imports
    /// a bracket and a bolt expects two things they can select. Curves,
    /// surfaces and free-standing shells are not solids and are dropped —
    /// this contract is about the solids.
    ///
    /// **Never `Ok(vec![])`.** A file that imports into nothing at all is a
    /// bug report about the modeller; if there is no solid to be had, say so
    /// with [`KernelError::Failed`] and a message naming what was in the file
    /// instead.
    ///
    /// Errors:
    ///
    /// - [`KernelError::Unsupported`] if this backend does not do STEP — and
    ///   for no other reason. Bytes that are not a STEP file at all are
    ///   [`KernelError::Failed`], because a caller has to be able to tell "this
    ///   build cannot read STEP" from "this file is not STEP" and the two
    ///   sentences send a user to different places.
    fn import_step(&mut self, bytes: &[u8]) -> Result<Vec<ImportedBody>>;
}

/// A body imported from an exchange file (e.g. STEP), carrying an optional product name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedBody {
    pub body: Body,
    pub name: Option<String>,
}
