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
    assert!(manifest.contains("\"version\": 2"), "{manifest}");
    assert!(manifest.contains("\"unit\": \"mm\""), "{manifest}");
    assert!(manifest.contains("\"next_uid\""), "{manifest}");
    assert!(manifest.contains("\"uid\""), "{manifest}");
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
    let node = |name: &str| w3d_core::Node::new(name, body);
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
        .replace(
            &format!("\"version\": {}", w3d_format::VERSION),
            "\"version\": 99",
        );
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

#[test]
fn an_assembly_tree_survives_a_save() {
    // The limitation this file used to pin, taken. A version-1 file had a flat
    // `nodes[]` and no way for one node to refer to another, so a document with
    // groups in it saved as its bodies, flat; version 2 writes the group and
    // the parent, and this is that sentence the other way round.
    let mut before = Document::new(FakeKernel::default());
    let outer = before.add_group("Engine");
    let inner = before.add_group("Cylinder 1");
    let part = before.add_box("Piston", Vec3::new(1.0, 1.0, 1.0)).unwrap();
    let loose = before.add_sphere("Loose", 1.0).unwrap();
    before.reparent(inner, Some(outer)).unwrap();
    before.reparent(part, Some(inner)).unwrap();
    before.set_visible(inner, false).unwrap();

    let bytes = save(&before).unwrap();

    // A group is still not geometry: two bodies, two blobs, and nothing
    // written for the two groups but their entries.
    let entries = w3d_format::zip::read(&bytes).unwrap();
    assert_eq!(
        entries
            .keys()
            .filter(|k| k.starts_with("geometry/"))
            .count(),
        2
    );

    let after = load(FakeKernel::default(), &bytes).unwrap();
    assert_eq!(after.len(), 4);

    let shape = |d: &Document<FakeKernel>| -> Vec<(String, usize, bool, bool)> {
        d.depth_first()
            .into_iter()
            .map(|(id, depth)| {
                let n = d.node(id).unwrap();
                (n.name.clone(), depth, n.is_group(), n.visible)
            })
            .collect()
    };
    assert_eq!(
        shape(&after),
        vec![
            (String::from("Engine"), 0, true, true),
            (String::from("Cylinder 1"), 1, true, false),
            (String::from("Piston"), 2, false, true),
            (String::from("Loose"), 0, false, true),
        ],
        "the tree, its depths, and which of them are groups"
    );
    // The parent is a link both ways, not just a field.
    let engine = after.depth_first()[0].0;
    let cylinder = after.depth_first()[1].0;
    assert_eq!(after.children_of(engine), [cylinder]);
    assert_eq!(after.parent_of(cylinder), Some(engine));

    // And the bodies are still there under the structure.
    for (id, node) in after.nodes() {
        if !node.is_group() {
            assert!(after.bounds(id).is_ok(), "{} lost its geometry", node.name);
        }
    }

    // Saving what was loaded produces the same file again, which is the
    // property that makes a tree in a file worth having.
    assert_eq!(save(&after).unwrap(), bytes, "the second save differs");
    assert!(
        after.by_uid(before.node(loose).unwrap().uid).is_some(),
        "a root beside the tree did not come back under its own identity"
    );
}

#[test]
fn identities_survive_a_save_and_are_not_handed_out_again() {
    let mut before = drilled_plate();
    let uids: Vec<_> = before.nodes().map(|(_, n)| n.uid).collect();
    let next = before.next_uid();

    // A node created and deleted spends an identity that must not come back.
    let doomed = before.add_sphere("Doomed", 1.0).unwrap();
    let spent = before.node(doomed).unwrap().uid;
    before.remove(doomed).unwrap();
    before.clear_history();

    let after = load(FakeKernel::default(), &save(&before).unwrap()).unwrap();
    assert_eq!(
        after.nodes().map(|(_, n)| n.uid).collect::<Vec<_>>(),
        uids,
        "the nodes came back under different identities"
    );
    assert!(after.next_uid() > spent, "the counter went backwards");
    assert!(next < after.next_uid());

    let mut after = after;
    let fresh = after.add_box("Fresh", Vec3::splat(1.0)).unwrap();
    let fresh_uid = after.node(fresh).unwrap().uid;
    assert_ne!(
        fresh_uid, spent,
        "a deleted node's identity was handed out to a new node"
    );
    assert!(
        !uids.contains(&fresh_uid),
        "a live node's identity was handed out twice"
    );
}

#[test]
fn the_unit_is_in_the_file() {
    let mut doc = drilled_plate();
    doc.set_unit(Some(w3d_core::Unit::Inch));
    let after = load(FakeKernel::default(), &save(&doc).unwrap()).unwrap();
    assert_eq!(after.unit(), Some(w3d_core::Unit::Inch));

    // A unit this build does not know is a refusal, not a document that
    // quietly states nothing.
    let bytes = save(&doc).unwrap();
    let mut entries = w3d_format::zip::read(&bytes).unwrap();
    let manifest = String::from_utf8(entries["manifest.json"].clone())
        .unwrap()
        .replace("\"unit\": \"in\"", "\"unit\": \"furlong\"");
    entries.insert(String::from("manifest.json"), manifest.into_bytes());
    assert!(matches!(
        load(
            FakeKernel::default(),
            &w3d_format::zip::write(&entries).unwrap()
        ),
        Err(FormatError::Malformed(_))
    ));
}

/// A file this program wrote before version 2 existed: a flat `nodes[]`, every
/// entry with geometry, and nothing that says what a number means. It must
/// still open, and it must open as what it is rather than as a document that
/// claims a unit nobody wrote down.
#[test]
fn a_version_one_file_still_opens_flat_and_unitless() {
    let mut entries = w3d_format::zip::read(&save(&drilled_plate()).unwrap()).unwrap();
    entries.insert(
        String::from("manifest.json"),
        VERSION_ONE_MANIFEST.as_bytes().to_vec(),
    );
    let bytes = w3d_format::zip::write(&entries).unwrap();

    let doc = load(FakeKernel::default(), &bytes).unwrap();
    assert_eq!(
        doc.nodes().map(|(_, n)| n.name.clone()).collect::<Vec<_>>(),
        vec![String::from("Plate − Drill"), String::from("Ball")]
    );
    assert_eq!(doc.unit(), None, "a version-1 file states no unit");
    assert!(doc.nodes().all(|(id, _)| doc.parent_of(id).is_none()));

    // Identities are invented on the way in, because the file has none — and
    // they are still identities: distinct, non-zero, and behind the counter.
    let uids: Vec<_> = doc.nodes().map(|(_, n)| n.uid).collect();
    assert_eq!(uids.len(), 2);
    assert_ne!(uids[0], uids[1]);
    assert!(uids.iter().all(|u| *u != w3d_core::Uid::UNASSIGNED));
    assert!(uids.iter().all(|u| *u < doc.next_uid()));

    // Version 1 had no groups in it, so an entry with no geometry is damage
    // rather than structure — reading it as a group would invent a tree.
    let mut entries = w3d_format::zip::read(&bytes).unwrap();
    entries.insert(
        String::from("manifest.json"),
        VERSION_ONE_MANIFEST
            .replace(
                "\"visible\": true,\n      \"geometry\": \"geometry/1.bin\"",
                "\"visible\": true",
            )
            .as_bytes()
            .to_vec(),
    );
    assert!(matches!(
        load(
            FakeKernel::default(),
            &w3d_format::zip::write(&entries).unwrap()
        ),
        Err(FormatError::Malformed(_))
    ));
}

/// Written out rather than produced, because the writer that produced it no
/// longer exists. This is the version-1 specification as `FORMAT.md` stated it.
const VERSION_ONE_MANIFEST: &str = r#"{
  "format": "w3d-document",
  "version": 1,
  "geometry": "fake-csg-1",
  "tolerance": { "linear": 1e-7, "angular": 0.00001 },
  "quality": { "sag": 0.01, "max_angle": 0.35 },
  "nodes": [
    {
      "name": "Plate − Drill",
      "visible": false,
      "geometry": "geometry/0.bin"
    },
    {
      "name": "Ball",
      "visible": true,
      "geometry": "geometry/1.bin"
    }
  ]
}"#;

/// The four ways a file can claim a tree it does not have. Each must be a
/// refusal with nothing built, and none of them may be a hang: the rule the
/// reader enforces — a parent appears earlier in `nodes` than its children —
/// is what makes a cycle unwritable rather than something a reader has to
/// survive.
#[test]
fn a_file_whose_nodes_are_not_a_tree_is_refused() {
    let bytes = save(&drilled_plate()).unwrap();
    let manifest =
        String::from_utf8(w3d_format::zip::read(&bytes).unwrap()["manifest.json"].clone()).unwrap();
    let load_with = |manifest: String| {
        let mut entries = w3d_format::zip::read(&bytes).unwrap();
        entries.insert(String::from("manifest.json"), manifest.into_bytes());
        load(
            FakeKernel::default(),
            &w3d_format::zip::write(&entries).unwrap(),
        )
    };

    // Unaltered it loads, so each failure below is the alteration and not the
    // fixture. Two nodes, with identities the writer chose.
    assert!(load_with(manifest.clone()).is_ok());
    let uids: Vec<&str> = manifest
        .match_indices("\"uid\": ")
        .map(|(at, key)| {
            let from = at + key.len();
            manifest[from..].split(',').next().unwrap().trim()
        })
        .collect();
    assert_eq!(uids.len(), 2, "{manifest}");

    // A parent that is not in the file.
    let text = manifest.replacen("\"uid\"", "\"parent\": 4096,\n      \"uid\"", 1);
    assert!(
        matches!(load_with(text), Err(FormatError::NotATree(_))),
        "a node inside something that is not there"
    );

    // A parent that is in the file, but later: the shape a cycle would need.
    let text = manifest.replacen(
        "\"uid\"",
        &format!("\"parent\": {},\n      \"uid\"", uids[1]),
        1,
    );
    assert!(
        matches!(load_with(text), Err(FormatError::NotATree(_))),
        "a parent later in the list than its child"
    );

    // Two nodes sharing one identity.
    let at = manifest.match_indices("\"uid\": ").nth(1).unwrap().0 + "\"uid\": ".len();
    let end = at + manifest[at..].find(',').unwrap();
    let text = format!("{}{}{}", &manifest[..at], uids[0], &manifest[end..]);
    assert!(
        matches!(load_with(text), Err(FormatError::NotATree(_))),
        "an identity that is not unique"
    );

    // A counter behind an identity in the file: the next node created would be
    // given one that is already taken.
    let text = manifest.replacen("\"next_uid\": ", "\"next_uid\": 1, \"was_next_uid\": ", 1);
    assert!(
        matches!(load_with(text), Err(FormatError::NotATree(_))),
        "a counter that has already been passed"
    );

    // And a version-2 file with no identity at all is damaged rather than
    // silently renumbered.
    let text = manifest.replacen("\"uid\"", "\"was_uid\"", 1);
    assert!(matches!(load_with(text), Err(FormatError::Malformed(_))));
}

#[test]
fn two_saves_of_one_document_are_the_same_bytes() {
    // What the zip's zero timestamps and sorted entries are for, and now also
    // what the identity counter is for: a save that invented identities would
    // produce a different file every time.
    let doc = drilled_plate();
    assert_eq!(save(&doc).unwrap(), save(&doc).unwrap());
}
