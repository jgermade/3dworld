//! Crate for operationg shapes. Provides boolean operations to Solid, and shape healing for importing shapes from other CAD systems.

// Patched in w3d (see PATCHED.md): `deny(warnings)` in release removed. Cargo
// caps a registry dependency's lints; it does not cap a path dependency's.
#![deny(clippy::all, rust_2018_idioms)]
#![warn(
    missing_docs,
    missing_debug_implementations,
    trivial_casts,
    trivial_numeric_casts,
    unsafe_code,
    unstable_features,
    unused_import_braces,
    unused_qualifications
)]

mod healing;
pub use healing::{RobustSplitClosedEdgesAndFaces, SplitClosedEdgesAndFaces};
mod transversal;
pub use transversal::{and, or, ShapeOpsCurve, ShapeOpsSurface};
mod alternative;
