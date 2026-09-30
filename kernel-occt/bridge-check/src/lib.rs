//! `OcctKernel` on wasm32, reaching the browser's OpenCASCADE through
//! `kernel-occt/src/remote.rs`, and asked two things from Node by
//! `tools/occt_bridge_check.mjs`.
//!
//! **The conformance suite**, the same one `make test-occt` runs against the
//! desktop's OCCT — so every check in the contract is a check on the bridge:
//! a pointer rewritten wrong, a buffer copied short, an error message lost on
//! the way back, each fails something there.
//!
//! **The reference scenes**, from the very file `make occt-wasm-check` runs
//! natively, included here rather than copied: the two columns it compares
//! are one function built twice.

use w3d_kernel::{Quality, Tolerance, conformance};
use w3d_kernel_occt::OcctKernel;
use wasm_bindgen::prelude::*;

#[path = "../../examples/occt_wasm_reference.rs"]
mod reference;

/// One line per check, `ok` or `FAILED`, then the failure's message.
#[wasm_bindgen]
pub fn conformance() -> String {
    let mut k = OcctKernel::new();
    let report = conformance::run(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    report
        .checks
        .iter()
        .map(|c| match &c.outcome {
            Ok(()) => format!("ok\t{}", c.name),
            Err(e) => format!("FAILED\t{}\t{e}", c.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The reference rows, `name<TAB>value`, as the native example prints them.
#[wasm_bindgen]
pub fn reference() -> String {
    let mut k = OcctKernel::new();
    reference::rows(&mut k)
        .into_iter()
        .map(|(n, v)| format!("{n}\t{v}"))
        .collect::<Vec<_>>()
        .join("\n")
}
