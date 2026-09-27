//! The native half of `make occt-wasm-check`: the same scenes the browser's
//! OpenCASCADE module is asked to build in `tools/occt_wasm_check.mjs`,
//! measured through `OcctKernel` on this machine's OCCT.
//!
//! Prints one `name<TAB>value` row per line. `tools/occt_wasm_check.py` runs
//! both halves and compares them. The two builds are the same OCCT release
//! from different compilers and different maths libraries, so topology — a
//! count of faces, edges, vertices — is required to agree exactly, and a mesh
//! is reported beside it rather than required to: a sine one ulp apart can
//! move a triangle.

use w3d_kernel::{BooleanOp, GeometryKernel, KernelError, Mat4, Quality, Tolerance, Vec3};
use w3d_kernel_occt::OcctKernel;

fn topo(k: &OcctKernel, b: w3d_kernel::Body) -> String {
    let t = k.topology(b).expect("topology");
    format!("{} {} {} {}", t.solids, t.faces, t.edges, t.vertices)
}

fn error_kind(e: &KernelError) -> &'static str {
    match e {
        KernelError::UnknownBody(_) => "unknown-body",
        KernelError::Degenerate(_) => "degenerate",
        KernelError::Unsupported(_) => "unsupported",
        _ => "failed",
    }
}

fn main() {
    let mut k = OcctKernel::new();
    let q = Quality::display_default();
    let row = |name: &str, value: String| println!("{name}\t{value}");

    let cube = k.create_box(Vec3::new(20.0, 20.0, 20.0)).expect("cube");
    row("cube.topology", topo(&k, cube));

    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("plate");
    let drill = k.create_cylinder(6.0, 20.0).expect("drill");
    let drill = k
        .transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .expect("move the drill");
    let cut = k
        .boolean(
            BooleanOp::Difference,
            plate,
            drill,
            Tolerance::document_default(),
        )
        .expect("the cut");
    row("cut.topology", topo(&k, cut));
    let m = k.tessellate(cut, q).expect("mesh the cut");
    row(
        "cut.mesh",
        format!(
            "{} {} {}",
            m.triangle_count(),
            m.positions.len(),
            m.line_count()
        ),
    );
    let b = k.bounds(cut).expect("bounds");
    row(
        "cut.bounds",
        format!(
            "{:.9} {:.9} {:.9} {:.9} {:.9} {:.9}",
            b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z
        ),
    );

    let fillet = k.fillet(cube, 2.0).expect("fillet");
    row("fillet.topology", topo(&k, fillet));
    let m = k.tessellate(fillet, q).expect("mesh the fillet");
    row(
        "fillet.mesh",
        format!(
            "{} {} {}",
            m.triangle_count(),
            m.positions.len(),
            m.line_count()
        ),
    );

    let chamfer = k.chamfer(cube, 2.0).expect("chamfer");
    row("chamfer.topology", topo(&k, chamfer));

    let shell = k.shell(cube, 0, 1.0).expect("shell");
    row("shell.topology", topo(&k, shell));

    // A fillet the cube cannot hold. At r = 10 OCCT says so; at r = 15 it
    // returns a solid with 78 edges and 40 vertices where a filleted cube has
    // 56 and 24, and the shim accepts it — a finding of this check, not a
    // behaviour it endorses. Both builds are held to the same answers.
    row(
        "fillet.r10",
        match k.fillet(cube, 10.0) {
            Ok(b) => topo(&k, b),
            Err(e) => error_kind(&e).into(),
        },
    );
    row(
        "fillet.r15",
        match k.fillet(cube, 15.0) {
            Ok(b) => topo(&k, b),
            Err(e) => error_kind(&e).into(),
        },
    );

    let blob = k.save_body(cut).expect("save");
    let back = k.load_body(&blob).expect("load");
    row("brep.topology", topo(&k, back));

    let step = k.export_step(&[cut]).expect("export STEP");
    let imported = k.import_step(&step).expect("import STEP");
    row("step.bodies", imported.bodies.len().to_string());
    row("step.topology", topo(&k, imported.bodies[0].body));
}
