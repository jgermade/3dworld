//! Writes a STEP file, for something that is not this program to read.
//!
//! Not a test and not a demo: it exists so that `make step-check` has a file
//! to hand to a parser that shares no code with OpenCASCADE. A test could
//! write one, but a test that leaves artefacts behind for another tool to
//! find is two jobs pretending to be one — and this needs no display, which
//! `make app-test-step` does.
//!
//! The shape is the same drilled plate the conformance suite uses: planes, a
//! cylindrical face and a seam. A file of nothing but boxes would prove that
//! the easy half survives.

use std::path::PathBuf;

use w3d_kernel::{BooleanOp, GeometryKernel, Tolerance, Vec3};
use w3d_kernel_occt::OcctKernel;

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: export_step <path.step>");
        return std::process::ExitCode::FAILURE;
    };

    let mut k = OcctKernel::new();
    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("box");
    let drill = k.create_cylinder(6.0, 40.0).expect("cylinder");
    let drilled = k
        .boolean(
            BooleanOp::Difference,
            plate,
            drill,
            Tolerance::document_default(),
        )
        .expect("difference");
    // A second body, unrelated and elsewhere, so the file has to carry more
    // than one product and a reader has to find both.
    let ball = k.create_sphere(8.0).expect("sphere");

    let bytes = match k.export_step(&[drilled, ball]) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("could not export: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&path, &bytes) {
        eprintln!("could not write {}: {e}", path.display());
        return std::process::ExitCode::FAILURE;
    }
    println!("{} bytes into {}", bytes.len(), path.display());
    std::process::ExitCode::SUCCESS
}
