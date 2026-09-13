//! The assembly tree of one particular file, weighed against what that file
//! contains.
//!
//! `import_step` reports what came out of a file it knows nothing about, which
//! catches a reader that refuses or crashes and cannot catch a reader that
//! answers confidently with the wrong shape. This knows the answer: the AS1
//! assembly from Pro/ENGINEER holds **five distinct parts placed eighteen
//! times**, arranged three levels deep, and every number below is read off the
//! file rather than off this program's output.
//!
//! Not a `cargo test`, for the same reason the other two here are not: the file
//! is fetched by `make step-samples` and is not in the tree, and a test that
//! passes when its input is missing is a test that says yes for a living. It is
//! run by `make step-check`, where the samples are.
//!
//! What it is really for is the two failures a count cannot see. A walk that
//! reads solids out of an assembly's compound emits each of them once per
//! ancestor — this file imported as thirty-six bodies until 2026-09-13, and
//! "thirty-six" was the only symptom. And a walk that visits parts instead has
//! to multiply the placements on the way down, because a bolt's position is the
//! product of every location above it; drop that and eighteen bodies still
//! arrive, six of them stacked inside each other at the origin. Hence the
//! centres.

use std::collections::BTreeMap;
use std::path::PathBuf;

use w3d_kernel::{GeometryKernel, Mesh, Quality};
use w3d_kernel_occt::OcctKernel;

/// What the file holds: a part name, and how many times it is placed.
const PLACEMENTS: [(&str, usize); 5] = [
    ("BOLT", 6),
    ("L-BRACKET", 2),
    ("NUT", 8),
    ("PLATE", 1),
    ("ROD", 1),
];

/// One root product, two L-bracket subassemblies, three nut-and-bolt
/// subassemblies in each of those, and the rod's own.
const ASSEMBLIES: usize = 10;

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: assembly_tree <as1_pe_203.stp>");
        return std::process::ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            println!("FAIL  {}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut k = OcctKernel::new();
    let imported = match k.import_step(&bytes) {
        Ok(imported) => imported,
        Err(e) => {
            println!("FAIL  {}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut wrong = Vec::new();
    if let Err(why) = imported.validate() {
        wrong.push(why);
    }

    // ---- the counts ----------------------------------------------------
    let total: usize = PLACEMENTS.iter().map(|(_, n)| n).sum();
    if imported.bodies.len() != total {
        wrong.push(format!(
            "{} bodies, and the file places {total} solids",
            imported.bodies.len()
        ));
    }
    if imported.assemblies.len() != ASSEMBLIES {
        wrong.push(format!(
            "{} assemblies, and the file has {ASSEMBLIES}",
            imported.assemblies.len()
        ));
    }

    // ---- the parts, by name --------------------------------------------
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for b in &imported.bodies {
        *census
            .entry(b.name.clone().unwrap_or_else(|| "<unnamed>".into()))
            .or_default() += 1;
    }
    let wanted: BTreeMap<String, usize> = PLACEMENTS
        .iter()
        .map(|(n, c)| ((*n).to_string(), *c))
        .collect();
    if census != wanted {
        wrong.push(format!(
            "the parts are {census:?}, and the file's are {wanted:?}"
        ));
    }

    // ---- the arrangement -----------------------------------------------
    let depth_of = |mut a: Option<usize>| {
        let mut depth = 0;
        while let Some(i) = a {
            depth += 1;
            a = imported.assemblies[i].parent;
        }
        depth
    };
    let deepest = imported.bodies.iter().map(|b| depth_of(b.parent)).max();
    if deepest != Some(3) {
        wrong.push(format!(
            "the deepest solid sits {deepest:?} assemblies down, and a bolt in this \
             file sits three: the file, a bracket, a nut-and-bolt"
        ));
    }
    let roots = imported
        .assemblies
        .iter()
        .filter(|a| a.parent.is_none())
        .count();
    if roots != 1 {
        wrong.push(format!(
            "{roots} root assemblies, and the file is one product"
        ));
    }
    if imported.bodies.iter().any(|b| b.parent.is_none()) {
        wrong.push("a solid sits outside the assembly, and every one in this file is in it".into());
    }

    // ---- the placements ------------------------------------------------
    //
    // Two placements of one part are the same shape in two places, and both
    // halves of that are asserted.
    //
    // *Different centres*, because a stack of six bolts at the origin is what a
    // walk that forgets to accumulate locations produces, and the count is
    // right either way.
    //
    // *The same volume*, and deliberately not the same bounding box. The first
    // draft of this check compared extents and failed: the two nuts on the rod
    // are 76.2 x 381 x 508 where the six on the bolts are 381 x 76.2 x 508,
    // because the rod runs along x and its nuts are turned a quarter turn to
    // face it. A box is not invariant under rotation and a placement is allowed
    // to rotate, so the box was the wrong invariant — what a rigid placement
    // cannot change is how much material the solid encloses.
    for (part, count) in PLACEMENTS {
        let mut centres = Vec::new();
        let mut volumes = Vec::new();
        for b in imported
            .bodies
            .iter()
            .filter(|b| b.name.as_deref() == Some(part))
        {
            match k.bounds(b.body) {
                Ok(bb) => centres.push(bb.center()),
                Err(e) => wrong.push(format!("{part} has no bounds: {e}")),
            }
            match k.tessellate(b.body, Quality::display_default()) {
                Ok(mesh) => volumes.push(enclosed_volume(&mesh)),
                Err(e) => wrong.push(format!("{part} cannot be meshed: {e}")),
            }
        }
        for (i, a) in centres.iter().enumerate() {
            for b in centres.iter().skip(i + 1) {
                if (*a - *b).length() < 1e-6 {
                    wrong.push(format!(
                        "two of the {count} {part} placements share a centre at {a:?}, so the \
                         locations above them were not applied"
                    ));
                }
            }
        }
        if let Some(first) = volumes.first() {
            for v in &volumes {
                // A curved face is triangulated afresh in each placement's own
                // orientation, so two placements of a bolt do not agree to the
                // last bit. A tenth of a percent is far tighter than the
                // difference between one part and another.
                if (v - first).abs() > first.abs() * 1e-3 {
                    wrong.push(format!(
                        "the {part} placements enclose different volumes: {first:.1} and {v:.1}"
                    ));
                }
            }
        }
    }

    if !wrong.is_empty() {
        for why in &wrong {
            println!("FAIL  {}: {why}", path.display());
        }
        return std::process::ExitCode::FAILURE;
    }
    println!(
        "ok    {}: {} parts placed {} times, {} assemblies, three deep",
        path.display(),
        PLACEMENTS.len(),
        total,
        imported.assemblies.len()
    );
    std::process::ExitCode::SUCCESS
}

/// How much material a closed mesh encloses, by the divergence theorem — the
/// signed volume of the tetrahedra each triangle makes with the origin.
///
/// The conformance suite has this function and keeps it private, which is the
/// right call there: it is an implementation detail of how that suite weighs a
/// body, not a promise the crate makes. Ten lines here is cheaper than a public
/// one everything would then be able to depend on.
fn enclosed_volume(mesh: &Mesh) -> f64 {
    let mut sum = 0.0;
    for t in mesh.indices.as_chunks::<3>().0 {
        let p = |i: u32| mesh.positions[i as usize].map(f64::from);
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        sum += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
    }
    (sum / 6.0).abs()
}
