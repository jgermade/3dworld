//! The same suite the fake passes, against real geometry.
//!
//! This is the run that says whether `w3d_kernel::conformance` encodes *the
//! contract* or merely encodes the fake. Read every failure as a question about
//! the suite before treating it as a bug in the backend — that reading is only
//! available the first time.

use w3d_kernel::{Quality, Tolerance, conformance};
use w3d_kernel_occt::OcctKernel;

#[test]
fn opencascade_conforms() {
    let mut k = OcctKernel::new();
    let report = conformance::run(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    for check in &report.checks {
        println!(
            "{:<7} {}",
            if check.outcome.is_ok() {
                "ok"
            } else {
                "FAILED"
            },
            check.name
        );
    }
    report.assert_passed();
}
