//! The modeller.
//!
//! Split in two on purpose, and the split is the only structural decision in
//! here:
//!
//! - [`editor`] is what the modeller *does* — commands, selection, the rule
//!   that a drag is not a click — with no window and no GPU in it, so
//!   `cargo test` can drive it.
//! - [`shell`] is winit, egui and a surface, and is the only part that needs a
//!   display. It is thin by construction: everything it could get wrong that a
//!   test could catch has been moved out of it.
//!
//! [`scene`] sits between them and owns the map from the document to the GPU,
//! which neither `w3d-core` nor `w3d-render` may own — the document must not
//! know what a GPU is, and the renderer must not know what a document is.

pub mod editor;
pub mod scene;
pub mod shell;

pub use editor::{Button, Command, Editor, Input, Reaction};
pub use scene::Scene;
