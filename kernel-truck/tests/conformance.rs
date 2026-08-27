//! Conformance suite for `w3d-kernel-truck`.
//!
//! Validates the pure-Rust `truck` backend against the kernel seam contract.

use w3d_kernel::{Quality, Tolerance, conformance};
use w3d_kernel_truck::TruckKernel;

#[test]
fn the_truck_kernel_conforms() {
    let mut k = TruckKernel::new();
    let report = conformance::run(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    report.assert_passed();
}
