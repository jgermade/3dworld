//! Writes `web/scene-occt.w3d`: the browser's second document, and the first
//! one its own kernel cannot open.
//!
//! The page's scene (`format/examples/scene_w3d.rs`) is a plate with a hole,
//! written by `truck`. This is the same plate with **its edges rounded** before
//! the hole goes in — a fillet, which `truck` does not have — written by
//! OpenCASCADE on this machine, so its manifest says `occt-brep-1` and a page
//! that opens it has to fetch the OpenCASCADE module to do so. That is the
//! point of it: `?doc=occt` is the one path where the browser models, or at
//! least opens, what only the exact kernel can make.
//!
//! As its sibling does, it refuses to write a scene whose operations
//! declined, and reads the file back with a kernel that never saw it before
//! reporting success. Needs OCCT, so it is `make web-scene-occt` and not part
//! of `make web`.

use std::path::PathBuf;

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_occt::OcctKernel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| String::from("web/scene-occt.w3d")),
    );

    let mut doc = Document::new(OcctKernel::new());
    let plate = doc.add_box("plate", Vec3::new(40.0, 40.0, 10.0))?;
    doc.fillet(plate, 2.0)?;
    let drill = doc.add_cylinder("drill", 6.0, 20.0)?;
    doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))?;
    let cut = doc.boolean(BooleanOp::Difference, plate, drill)?;

    let direct = doc.mesh(cut)?.triangle_count();
    let bytes = w3d_format::save(&doc)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &bytes)?;

    let mut reread = w3d_format::load(OcctKernel::new(), &bytes)?;
    let ids: Vec<_> = reread.nodes().map(|(id, _)| id).collect();
    let mut triangles = 0usize;
    for id in &ids {
        triangles += reread.mesh(*id)?.triangle_count();
    }
    if triangles == 0 {
        return Err("the document read back with nothing to draw".into());
    }
    println!(
        "{} — {} bytes, {} node(s), {} triangles direct, {} when read back",
        out.display(),
        bytes.len(),
        doc.len(),
        direct,
        triangles
    );
    Ok(())
}
