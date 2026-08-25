//! The fake is held to exactly the same suite as any real backend. That is the
//! point of the suite: "a backend is one that passes this" has to be true of
//! the first one too, or it will not be true of the fourth.

use w3d_kernel::{Quality, Tolerance, conformance};
use w3d_kernel_fake::FakeKernel;

#[test]
fn the_fake_kernel_conforms() {
    let mut k = FakeKernel::new();
    let report = conformance::run(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    report.assert_passed();
}
