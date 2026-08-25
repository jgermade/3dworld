//! The document, history, selection and tessellation cache — everything the
//! modeller is, above the seam.
//!
//! It is generic over [`w3d_kernel::GeometryKernel`] and names no backend. The
//! whole of it is testable against `w3d-kernel-fake` with no OCCT, no browser
//! and no `.wasm` anywhere.

pub mod arena;
pub mod document;
pub mod history;

pub use arena::{Arena, Id};
pub use document::{Document, DocumentError, Node, NodeId};
pub use history::History;

/// Re-exported so callers need one dependency, not two, and so that the
/// kernel's vocabulary is the document's vocabulary.
pub use w3d_kernel as kernel;
