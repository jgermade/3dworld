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
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
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

    fn topology(&self, body: Body) -> Result<Topology>;

    fn bounds(&self, body: Body) -> Result<Aabb>;

    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh>;
}
