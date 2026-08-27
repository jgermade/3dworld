//! A document survives a save and a load.
//!
//! Against `FakeKernel`, so this runs with no OCCT and no GPU. What it proves
//! is the *format* — the container, the manifest, the node graph, the refusals.
//! That the geometry itself survives is a different claim and belongs to the
//! conformance suite, which asserts it of every backend.

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, GeometryKernel, Mat4, Quality, Tolerance, Vec3};
use w3d_format::{FormatError, load, save};
use w3d_kernel_fake::FakeKernel;

fn drilled_plate() -> Document<FakeKernel> {
    let mut doc = Document::new(FakeKernel::default());
    let plate = doc.add_box("Plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = doc.add_cylinder("Drill", 6.0, 20.0).unwrap();
    doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .unwrap();
    doc.boolean(BooleanOp::Difference, plate, drill).unwrap();
    doc.add_sphere("Ball", 5.0).unwrap();
    let first = doc.nodes().next().unwrap().0;
    doc.set_visible(first, false).unwrap();
    doc
}

#[test]
fn a_document_comes_back_with_its_nodes_names_and_visibility() {
    let mut before = drilled_plate();
    before.set_tolerance(Tolerance::new(1.0e-6, 1.0e-4));
    before.set_quality(Quality::new(0.02, 0.3));

    let bytes = save(&before).unwrap();
    let after = load(FakeKernel::default(), &bytes).unwrap();

    assert_eq!(after.len(), before.len());
    let names = |d: &Document<FakeKernel>| -> Vec<(String, bool)> {
        d.nodes()
            .map(|(_, n)| (n.name.clone(), n.visible))
            .collect()
    };
    assert_eq!(names(&after), names(&before), "in document order");
    assert_eq!(after.tolerance(), before.tolerance());
    assert_eq!(after.quality(), before.quality());

    for (id, _) in after.nodes() {
        assert!(after.bounds(id).is_ok());
        assert!(after.topology(id).is_ok());
    }
}

#[test]
fn bounds_and_topology_survive_the_trip() {
    let before = drilled_plate();
    let expected: Vec<_> = before
        .nodes()
        .map(|(id, _)| (before.bounds(id).unwrap(), before.topology(id).unwrap()))
        .collect();

    let bytes = save(&before).unwrap();
    let after = load(FakeKernel::default(), &bytes).unwrap();
    let got: Vec<_> = after
        .nodes()
        .map(|(id, _)| (after.bounds(id).unwrap(), after.topology(id).unwrap()))
        .collect();

    assert_eq!(got, expected);
}

/// The property the whole container was chosen for: a person can look inside
/// with tools they already have.
#[test]
fn the_file_is_a_zip_with_a_readable_manifest() {
    let bytes = save(&drilled_plate()).unwrap();
    assert_eq!(&bytes[..2], b"PK", "not a zip");

    let entries = w3d_format::zip::read(&bytes).unwrap();
    let manifest = String::from_utf8(entries["manifest.json"].clone()).unwrap();
    assert!(
        manifest.contains("\"format\": \"w3d-document\""),
        "{manifest}"
    );
    assert!(
        manifest.contains("\"geometry\": \"fake-csg-1\""),
        "{manifest}"
    );
    // The boolean consumed "Plate" and "Drill" into one node, so the name to
    // look for is the result's — asserting on "Plate" alone passed for the
    // wrong reason.
    assert!(manifest.contains("Plate − Drill"), "{manifest}");
    assert!(manifest.contains("\"Ball\""), "{manifest}");
    assert!(
        entries.contains_key("geometry/0.bin"),
        "no geometry: {:?}",
        entries.keys().collect::<Vec<_>>()
    );
}

/// Bodies are immutable and shared, so a document with two nodes on one body
/// must not write it twice.
#[test]
fn a_shared_body_is_stored_once() {
    // Built from a kernel directly rather than through the document, because
    // sharing a body between two nodes is not something the editing API can
    // produce — only a file, or a future `copy` that does not deep-copy.
    let mut kernel = FakeKernel::default();
    let body = kernel.create_box(Vec3::new(1.0, 1.0, 1.0)).unwrap();
    let node = |name: &str| w3d_core::Node {
        name: String::from(name),
        body,
        visible: true,
    };
    let doc = Document::from_parts(
        kernel,
        Tolerance::document_default(),
        Quality::display_default(),
        [node("A"), node("A again")],
    );

    let entries = w3d_format::zip::read(&save(&doc).unwrap()).unwrap();
    let blobs = entries
        .keys()
        .filter(|k| k.starts_with("geometry/"))
        .count();
    assert_eq!(blobs, 1, "a shared body was written twice");
    assert_eq!(doc.len(), 2);

    // And it comes back as two nodes on one body.
    let after = load(FakeKernel::default(), &save(&doc).unwrap()).unwrap();
    let bodies: Vec<_> = after.nodes().map(|(_, n)| n.body).collect();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1], "the sharing was lost");
}

#[test]
fn a_document_written_by_another_kernel_is_refused_by_name() {
    let bytes = save(&drilled_plate()).unwrap();
    // Rewrite the manifest to claim a different backend, which is exactly what
    // an OCCT-written file looks like to a fake-kernel build.
    let mut entries = w3d_format::zip::read(&bytes).unwrap();
    let manifest = String::from_utf8(entries["manifest.json"].clone())
        .unwrap()
        .replace("fake-csg-1", "occt-brep-1");
    entries.insert(String::from("manifest.json"), manifest.into_bytes());
    let bytes = w3d_format::zip::write(&entries).unwrap();

    match load(FakeKernel::default(), &bytes) {
        Err(FormatError::WrongKernel { file, kernel }) => {
            assert_eq!(file, "occt-brep-1");
            assert_eq!(kernel, "fake-csg-1");
        }
        Err(e) => panic!("a foreign document was refused as {e}"),
        Ok(_) => panic!("a foreign document loaded"),
    }
}

#[test]
fn a_future_version_is_refused_with_both_numbers() {
    let bytes = save(&drilled_plate()).unwrap();
    let mut entries = w3d_format::zip::read(&bytes).unwrap();
    let manifest = String::from_utf8(entries["manifest.json"].clone())
        .unwrap()
        .replace("\"version\": 1", "\"version\": 99");
    entries.insert(String::from("manifest.json"), manifest.into_bytes());
    let bytes = w3d_format::zip::write(&entries).unwrap();

    match load(FakeKernel::default(), &bytes) {
        Err(FormatError::TooNew { found, understood }) => {
            assert_eq!((found, understood), (99, w3d_format::VERSION));
        }
        Err(e) => panic!("a future document was refused as {e}"),
        Ok(_) => panic!("a future document loaded"),
    }
}

#[test]
fn things_that_are_not_documents_say_which_kind_of_not() {
    assert!(matches!(
        load(FakeKernel::default(), b"not a zip at all"),
        Err(FormatError::Zip(_))
    ));

    // A valid zip with nothing of ours in it.
    let empty = w3d_format::zip::write(&std::collections::BTreeMap::from([(
        String::from("readme.txt"),
        b"hello".to_vec(),
    )]))
    .unwrap();
    assert!(matches!(
        load(FakeKernel::default(), &empty),
        Err(FormatError::NotADocument(_))
    ));
}

/// A blob named by the manifest but missing from the archive must fail, not
/// produce a document with a hole in it.
#[test]
fn a_missing_blob_fails_the_whole_load() {
    let bytes = save(&drilled_plate()).unwrap();
    let mut entries = w3d_format::zip::read(&bytes).unwrap();
    entries.remove("geometry/0.bin");
    let bytes = w3d_format::zip::write(&entries).unwrap();

    assert!(matches!(
        load(FakeKernel::default(), &bytes),
        Err(FormatError::Malformed(_))
    ));
}

#[test]
fn camera_pose_and_thumbnail_survive_w3d_round_trip() {
    let doc = drilled_plate();
    let camera = w3d_format::CameraPose {
        eye: [10.0, 20.0, 30.0],
        target: [0.0, 0.0, 0.0],
        up: [0.0, 0.0, 1.0],
    };
    let dummy_png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    let options = w3d_format::SaveOptions {
        camera: Some(camera.clone()),
        thumbnail_png: Some(&dummy_png),
    };

    let bytes = w3d_format::save_with_options(&doc, &options).unwrap();
    let loaded = w3d_format::load_with_metadata(FakeKernel::default(), &bytes).unwrap();

    assert_eq!(loaded.camera, Some(camera));
    assert_eq!(loaded.thumbnail_png, Some(dummy_png));
}
