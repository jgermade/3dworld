//! Writes the browser demo's scene as a `.w3d`, so that the page has a
//! document to *send* its worker rather than one the worker builds itself.
//!
//! This exists because of what item 4 in the register turned out to be. The
//! worker became the boot path on 2026-09-15, and it could only be because the
//! page's scene is fixed and both sides can construct it. A modeller's boot
//! path is the opposite: bytes come from a file, and the thread that draws
//! should never build a document at all — it hands the bytes on. So the bytes
//! have to exist, and something native has to write them.
//!
//! **The scene here is a second copy of `scene()` in `web/src/lib.rs`**, and
//! two copies of anything drift. What stops it is not discipline: the browser
//! test tessellates the built-in scene on the main thread and requires it to
//! agree, triangle for triangle, with the document the page loaded from this
//! file. A drift is then a failing check rather than a picture nobody compares.
//!
//! Run by `make web-scene`, which `make web` depends on. Deliberately not
//! committed as an artifact: a generated file in the tree is a file that can be
//! older than its generator and look authoritative.

use std::path::PathBuf;

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_truck::TruckKernel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| String::from("web/scene.w3d")),
    );

    let mut doc = Document::new(TruckKernel::default());
    let plate = doc.add_box("plate", Vec3::new(40.0, 40.0, 10.0))?;
    let drill = doc.add_cylinder("drill", 6.0, 20.0)?;
    doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))?;

    // The boolean is done *here*, once, at build time — which is the whole
    // point of sending a document instead of a recipe. On the page's old path
    // the worker spent 500 ms of every boot re-cutting a hole that never
    // changes.
    let modelled = doc.boolean(BooleanOp::Difference, plate, drill).is_ok();
    if !modelled {
        // Refused rather than written. A scene file that quietly contains an
        // uncut plate is how the browser spent nine days drawing one.
        return Err("the boolean declined, so this document is not the scene".into());
    }

    // Measured before the save, in the same process, so that the comparison
    // below is a round trip and not two different programs.
    let direct_ids: Vec<_> = doc.nodes().map(|(id, _)| id).collect();
    let mut direct = 0usize;
    for id in &direct_ids {
        direct += doc.mesh(*id)?.triangle_count();
    }

    let bytes = w3d_format::save(&doc)?;

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &bytes)?;

    // Read back with a kernel that never saw the geometry, and mesh it. The
    // same discipline `make step-check` uses at the other end of this
    // repository: a writer that cannot demonstrate a reader is a writer nobody
    // has checked. It is cheap here and it is the only thing standing between
    // a silently malformed file and a blank page in a browser.
    let mut reread = w3d_format::load(TruckKernel::default(), &bytes)?;
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
