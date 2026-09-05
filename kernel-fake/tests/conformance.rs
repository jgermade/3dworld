//! The fake is held to exactly the same suite as any real backend. That is the
//! point of the suite: "a backend is one that passes this" has to be true of
//! the first one too, or it will not be true of the fourth.
//!
//! Since 2026-09-05 "the same suite" has two halves, and this backend says it
//! does not do geometry, so it is held to one of them. The second test below
//! is what keeps that from being an excuse: the half it is let off **must
//! fail** here, and must fail on the boolean.

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

/// The negative control for the geometry half of the suite.
///
/// `FakeKernel`'s boolean keeps a tree and answers from bounding boxes, which
/// is a fair description of what a stub does and an exact description of what
/// `TruckKernel` did until the record file for 2026-09-05. If the geometry
/// half cannot tell that from a boolean, it is not worth running against the
/// backends that do claim geometry — so it is pointed at the one backend that
/// is honest about not doing any, and required to say no.
#[test]
fn the_geometry_half_fails_the_backend_that_does_not_do_geometry() {
    let mut k = FakeKernel::new();
    let report = conformance::geometry(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    assert!(
        !report.passed(),
        "the fake kernel passed the geometry half, so the geometry half proves \
         nothing about the backends that do claim geometry"
    );

    let failed: Vec<&str> = report.failures().map(|c| c.name).collect();
    for expected in [
        "a difference removes exactly what the tool covers",
        "a union of two boxes is both, counted once",
    ] {
        assert!(
            failed.contains(&expected),
            "{expected:?} passed against bounding boxes. The failures were {failed:?}"
        );
    }
}
