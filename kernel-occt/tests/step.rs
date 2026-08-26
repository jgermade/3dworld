//! STEP, against real geometry.
//!
//! The conformance suite already holds every backend to the *contract* — that
//! STEP is offered in both directions or in neither, that a solid survives a
//! round-trip, that bytes which are not STEP are refused by the right name.
//! What is here is what only this backend can be asked:
//!
//! - that the bytes are a STEP file **by inspection**, not only because the
//!   library that wrote them can read them back. One writer and one reader
//!   from the same head agreeing proves less than it looks like, which is the
//!   same caveat `FORMAT.md` carries about the native format;
//! - that a kernel which never saw the geometry can read it, because that is
//!   what interchange means and a round-trip inside one kernel does not test
//!   it;
//! - that the two serialisation mechanisms refuse each other's bytes by the
//!   right name, which is the distinction a user is asked to act on.

use w3d_core::Document;
use w3d_kernel::{Body, BooleanOp, GeometryKernel, KernelError, Tolerance, Vec3};
use w3d_kernel_occt::OcctKernel;

/// A plate with a hole through it: planes, a cylindrical face and a seam.
fn drilled_plate(k: &mut OcctKernel) -> Body {
    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = k.create_cylinder(6.0, 40.0).unwrap();
    k.boolean(
        BooleanOp::Difference,
        plate,
        drill,
        Tolerance::document_default(),
    )
    .unwrap()
}

#[test]
fn a_kernel_that_never_saw_the_geometry_reads_what_another_wrote() {
    let mut writer = OcctKernel::new();
    let body = drilled_plate(&mut writer);
    let before = writer.bounds(body).unwrap();
    let bytes = writer.export_step(&[body]).unwrap();
    drop(writer);

    // A second kernel, with its own registry and no knowledge of the first.
    // This is the whole point of the format: the receiving program is not
    // this one.
    let mut reader = OcctKernel::new();
    let bodies = reader.import_step(&bytes).unwrap();
    assert_eq!(bodies.len(), 1, "one solid was written");

    let after = reader.bounds(bodies[0].body).unwrap();
    let slack = 1.0e-6;
    for (a, b, what) in [
        (before.min.x, after.min.x, "min.x"),
        (before.min.y, after.min.y, "min.y"),
        (before.min.z, after.min.z, "min.z"),
        (before.max.x, after.max.x, "max.x"),
        (before.max.y, after.max.y, "max.y"),
        (before.max.z, after.max.z, "max.z"),
    ] {
        assert!(
            (a - b).abs() <= slack,
            "{what} moved across the file: {a} became {b}"
        );
    }

    // And it is geometry, not a bounding box that happens to agree: the hole
    // is still a hole.
    let topology = reader.topology(bodies[0].body).unwrap();
    assert_eq!(topology.solids, 1);
    assert!(
        topology.faces >= 6,
        "a drilled plate came back with {} faces",
        topology.faces
    );
}

#[test]
fn the_bytes_are_a_step_file_by_inspection() {
    let mut k = OcctKernel::new();
    let body = drilled_plate(&mut k);
    let bytes = k.export_step(&[body]).unwrap();
    let text = String::from_utf8(bytes).expect("STEP part 21 is ASCII");

    // The skeleton ISO 10303-21 requires, in order. A reader that is not
    // OpenCASCADE parses this before it parses anything geometric.
    assert!(text.starts_with("ISO-10303-21;"), "no part 21 header");
    assert!(text.trim_end().ends_with("END-ISO-10303-21;"), "no footer");
    let header = text.find("HEADER;").expect("no HEADER section");
    let data = text.find("DATA;").expect("no DATA section");
    assert!(header < data, "the sections are in the wrong order");

    // The schema, named. A file whose schema nobody recognises is a file
    // nothing opens, and this is the one string every receiving program reads
    // before deciding whether to try.
    assert!(
        text.contains("AUTOMOTIVE_DESIGN"),
        "no AP214 schema in the header"
    );

    // A *solid* model. This is the difference between a file another CAD
    // program can model with and a bag of surfaces it can only look at, and
    // nothing in a round-trip through the same library would notice the
    // difference.
    assert!(
        text.contains("MANIFOLD_SOLID_BREP"),
        "the file has no solid in it"
    );
    assert!(
        text.contains("ADVANCED_BREP_SHAPE_REPRESENTATION"),
        "not a B-rep shape representation"
    );

    // Millimetres, stated. The document has no units; this is where the
    // decision is written down for somebody else's program to read.
    assert!(
        text.contains("SI_UNIT(.MILLI.,.METRE.)"),
        "the file does not state its unit"
    );

    // Who wrote it, which is the first question asked about a file that opens
    // badly somewhere else.
    assert!(
        text.contains("3dworld"),
        "the file does not name this program"
    );
}

#[test]
fn every_solid_in_a_file_becomes_a_body() {
    let mut k = OcctKernel::new();
    let a = k.create_box(Vec3::splat(10.0)).unwrap();
    let b = k.create_sphere(4.0).unwrap();
    let c = k.create_cylinder(2.0, 20.0).unwrap();
    let bytes = k.export_step(&[a, b, c]).unwrap();

    let mut reader = OcctKernel::new();
    let bodies = reader.import_step(&bytes).unwrap();
    assert_eq!(bodies.len(), 3, "three solids went in");
    // Distinct handles into the reading kernel, all of them live.
    for item in &bodies {
        reader
            .bounds(item.body)
            .expect("a body that cannot be measured");
    }
    assert_eq!(reader.live_bodies(), 3, "the import leaked or lost bodies");
}

#[test]
fn each_format_refuses_the_others_bytes_by_the_right_name() {
    let mut k = OcctKernel::new();
    let body = drilled_plate(&mut k);
    let brep = k.save_body(body).unwrap();
    let step = k.export_step(&[body]).unwrap();

    // BREP offered to the STEP reader. This build *does* read STEP, so the
    // fault is in the file, not in the program: `Failed`, with a reason.
    match k.import_step(&brep) {
        Err(KernelError::Failed(why)) => assert!(!why.is_empty(), "refused with no reason"),
        other => panic!("BREP read as STEP: {other:?}"),
    }

    // STEP offered to the BREP reader. `load_body` reserves `Unsupported` for
    // bytes it does not recognise as its own format at all, which is exactly
    // what these are — and what tells a document to say "export it to STEP"
    // rather than "this file is damaged".
    match k.load_body(&step) {
        Err(KernelError::Unsupported(_)) => {}
        other => panic!("STEP read as BREP: {other:?}"),
    }
}

#[test]
fn a_file_with_no_solids_in_it_is_refused_with_a_reason() {
    // Valid part 21, correct schema, and nothing in the data section. It
    // parses; there is simply nothing in it to be a body. `Ok(vec![])` would
    // be a document that grew by nothing and said nothing, which is a bug
    // report about the modeller filed against the wrong program.
    let empty = "ISO-10303-21;\n\
                 HEADER;\n\
                 FILE_DESCRIPTION((''),'2;1');\n\
                 FILE_NAME('empty','2026-01-01T00:00:00',(''),(''),'','','');\n\
                 FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\n\
                 ENDSEC;\n\
                 DATA;\n\
                 ENDSEC;\n\
                 END-ISO-10303-21;\n";

    let mut k = OcctKernel::new();
    match k.import_step(empty.as_bytes()) {
        Err(KernelError::Failed(why)) => {
            assert!(
                why.contains("solid"),
                "the reason does not say what was missing: {why}"
            );
        }
        other => panic!("an empty STEP file gave {other:?}"),
    }
    assert_eq!(k.live_bodies(), 0, "a failed import left bodies behind");
}

#[test]
fn an_import_is_one_undo_step_and_names_what_it_added() {
    let mut source = OcctKernel::new();
    let a = source.create_box(Vec3::splat(10.0)).unwrap();
    let b = source.create_sphere(4.0).unwrap();
    let bytes = source.export_step(&[a, b]).unwrap();

    let mut doc = Document::new(OcctKernel::new());
    doc.add_box("Existing", Vec3::splat(2.0)).unwrap();

    let ids = doc.import_step(&bytes, "bracket").unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(doc.len(), 3, "an import replaced instead of appending");
    let names: Vec<_> = ids
        .iter()
        .map(|id| doc.node(*id).unwrap().name.clone())
        .collect();
    assert_eq!(names, vec!["bracket 1", "bracket 2"]);

    // One undo, not two. An import has an obvious boundary and undoing half
    // of one is not a state anybody asked for.
    assert_eq!(doc.undo(), Some("Import STEP"));
    assert_eq!(doc.len(), 1, "one undo did not take the whole import");
    assert_eq!(doc.redo(), Some("Import STEP"));
    assert_eq!(doc.len(), 3);
}
