//! An imported assembly, written to a `.w3d` and opened again.
//!
//! `assembly_tree` beside this one weighs what comes *out of a STEP file*
//! against what that file holds. This is the next claim along and the one
//! version 2 of the document format exists for: that the tree survives being
//! written down. A four-node fixture in `format/tests/roundtrip.rs` proves the
//! manifest; this is eighteen bodies in ten assemblies, three levels deep, with
//! real OpenCASCADE geometry under every leaf — which is the shape that would
//! catch a walk that flattens, a reader that loses a level, or a writer that
//! drops the node it could not reach.
//!
//! Not a `cargo test`, for the reason the other examples here are not: the
//! sample is fetched by `make step-samples` and is not in the tree, and a test
//! that passes when its input is missing is a test that says yes for a living.
//!
//! Both halves of "survives" are asserted. The **shape** — every node's name,
//! depth, whether it is a group, and its identity — and the **geometry**, by
//! the bounds of each body, because a tree that came back perfectly around
//! bodies that did not is not a document anybody wanted.

use std::path::PathBuf;

use w3d_core::Document;
use w3d_kernel_occt::OcctKernel;

/// What the file holds. The same two numbers `assembly_tree` uses, restated
/// here so that this example fails on its own terms rather than by importing
/// somebody else's constant.
const BODIES: usize = 18;
const ASSEMBLIES: usize = 10;

/// A row of the tree, as this example compares it: no `NodeId` in it, because
/// arena slots are this session's and the point is what crossed the file.
type Row = (String, usize, bool, bool, u64);

fn rows(doc: &Document<OcctKernel>) -> Vec<Row> {
    doc.depth_first()
        .into_iter()
        .filter_map(|(id, depth)| {
            let node = doc.node(id).ok()?;
            Some((
                node.name.clone(),
                depth,
                node.is_group(),
                node.visible,
                node.uid.raw(),
            ))
        })
        .collect()
}

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: assembly_file <as1_pe_203.stp>");
        return std::process::ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            println!("FAIL  {}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut before = Document::new(OcctKernel::new());
    let imported = match before.import_step(&bytes, "AS1") {
        Ok(ids) => ids,
        Err(e) => {
            println!("FAIL  {}: {e}", path.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut wrong = Vec::new();
    if imported.len() != BODIES {
        wrong.push(format!("{} bodies imported, not {BODIES}", imported.len()));
    }
    if before.len() != BODIES + ASSEMBLIES {
        wrong.push(format!(
            "{} nodes in the document, and {BODIES} bodies in {ASSEMBLIES} assemblies is {}",
            before.len(),
            BODIES + ASSEMBLIES
        ));
    }

    // The bounds of each body, keyed by the identity that is about to cross the
    // file. Keyed by identity rather than by position, because the whole
    // question is whether identity means anything afterwards.
    let mut bounds_before: Vec<(u64, [f64; 6])> = before
        .nodes()
        .filter(|(_, n)| !n.is_group())
        .map(|(id, n)| {
            let b = before.bounds(id).expect("a body has bounds");
            (
                n.uid.raw(),
                [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z],
            )
        })
        .collect();
    bounds_before.sort_by_key(|(uid, _)| *uid);
    let shape_before = rows(&before);

    let file = match w3d_format::save(&before) {
        Ok(file) => file,
        Err(e) => {
            println!(
                "FAIL  {}: could not save the imported assembly: {e}",
                path.display()
            );
            return std::process::ExitCode::FAILURE;
        }
    };

    // A second kernel, so that nothing about the document depends on the one
    // that built it: this is a file being opened, not a document being copied.
    let after = match w3d_format::load(OcctKernel::new(), &file) {
        Ok(after) => after,
        Err(e) => {
            println!(
                "FAIL  {}: could not open what we wrote: {e}",
                path.display()
            );
            return std::process::ExitCode::FAILURE;
        }
    };

    let shape_after = rows(&after);
    if shape_after != shape_before {
        let first = shape_before
            .iter()
            .zip(&shape_after)
            .find(|(a, b)| a != b)
            .map(|(a, b)| format!("{a:?} became {b:?}"))
            .unwrap_or_else(|| {
                format!(
                    "{} rows went in and {} came out",
                    shape_before.len(),
                    shape_after.len()
                )
            });
        wrong.push(format!("the tree changed across the file: {first}"));
    }

    let mut bounds_after: Vec<(u64, [f64; 6])> = after
        .nodes()
        .filter(|(_, n)| !n.is_group())
        .filter_map(|(id, n)| {
            let b = after.bounds(id).ok()?;
            Some((
                n.uid.raw(),
                [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z],
            ))
        })
        .collect();
    bounds_after.sort_by_key(|(uid, _)| *uid);

    // Compared to a tolerance rather than exactly, and the number is reported
    // rather than assumed. BREP is text: a double goes out with as many digits
    // as OpenCASCADE writes and comes back as the nearest double to that,
    // which on this file moves a bound by around 1e-25 mm on a part 1524 mm
    // across. The bound to hold it to is the document's linear tolerance,
    // which is 1e-7 — anything near that is a solid that changed, not a
    // printed digit.
    const SLACK: f64 = 1.0e-9;
    let mut worst: f64 = 0.0;
    if bounds_after.len() != bounds_before.len() {
        wrong.push(format!(
            "{} bodies went in and {} came back",
            bounds_before.len(),
            bounds_after.len()
        ));
    }
    for ((uid, a), (other, b)) in bounds_before.iter().zip(&bounds_after) {
        if uid != other {
            wrong.push(format!("body {uid} came back as {other}"));
            continue;
        }
        let off = a
            .iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f64, f64::max);
        worst = worst.max(off);
        if off > SLACK {
            wrong.push(format!(
                "body {uid} came back a different size: {a:?} became {b:?}"
            ));
        }
    }

    if after.unit() != before.unit() {
        wrong.push(format!(
            "the document went in as {:?} and came back as {:?}",
            before.unit(),
            after.unit()
        ));
    }

    // Written again, for the half of reproducibility this format governs.
    //
    // **The manifest must be identical**: same identities, same tree, same
    // order, same blob names. Identities invented at save time, or an order
    // that depended on a `HashMap`, would show up here and nowhere else.
    //
    // **The blobs are not asserted to be**, and that is a finding rather than a
    // slack assertion. OpenCASCADE writes a line of shape flags per shape, and
    // a shape it has just built carries `Checked` where the same shape read
    // back from BREP does not — so `geometry/*.bin` differs in one bit per
    // shape after a reopen, with identical geometry under it. The blobs are the
    // kernel's own bytes and this format does not interpret them; what it can
    // say is that the same *document* saves identically twice, which
    // `two_saves_of_one_document_are_the_same_bytes` holds it to, and that a
    // reopened one keeps every solid it had, which the bounds above assert.
    match w3d_format::save(&after) {
        Ok(again) => {
            let (a, b) = (
                w3d_format::zip::read(&file).unwrap_or_default(),
                w3d_format::zip::read(&again).unwrap_or_default(),
            );
            if a.keys().collect::<Vec<_>>() != b.keys().collect::<Vec<_>>() {
                wrong.push(String::from(
                    "the second save holds different entries from the first",
                ));
            }
            if a.get("manifest.json") != b.get("manifest.json") {
                wrong.push(String::from(
                    "the manifest changed when the opened document was saved again",
                ));
            }
        }
        Err(e) => wrong.push(format!("the opened document would not save: {e}")),
    }

    if wrong.is_empty() {
        println!(
            "ok    {}: {BODIES} bodies in {ASSEMBLIES} assemblies, saved as {} KiB of .w3d and \
             opened with its tree · bounds moved at most {worst:.1e} mm",
            path.display(),
            file.len() / 1024
        );
        std::process::ExitCode::SUCCESS
    } else {
        for why in &wrong {
            println!("FAIL  {}: {why}", path.display());
        }
        std::process::ExitCode::FAILURE
    }
}
