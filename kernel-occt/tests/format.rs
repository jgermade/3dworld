//! A document with real geometry, saved and opened.
//!
//! `w3d-format`'s own tests run against the fake kernel and prove the
//! container. This proves the thing the container is for: OpenCASCADE BREP
//! goes out and comes back as the same solid.

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Vec3};
use w3d_format::{FormatError, load, save};
use w3d_kernel_occt::OcctKernel;

fn drilled_plate() -> Document<OcctKernel> {
    let mut d = Document::new(OcctKernel::new());
    let plate = d.add_box("Plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = d.add_cylinder("Drill", 6.0, 30.0).unwrap();
    d.boolean(BooleanOp::Difference, plate, drill).unwrap();
    d
}

#[test]
fn a_drilled_plate_survives_a_save_and_a_load() {
    let before = drilled_plate();
    let (id, _) = before.nodes().next().unwrap();
    let topology = before.topology(id).unwrap();
    let bounds = before.bounds(id).unwrap();
    assert_eq!(
        (topology.faces, topology.edges, topology.vertices),
        (7, 15, 10),
        "the fixture is not the drilled plate any more"
    );

    let bytes = save(&before).unwrap();
    let mut after = load(OcctKernel::new(), &bytes).unwrap();

    let (id, node) = after.nodes().next().unwrap();
    assert_eq!(node.name, "Plate − Drill");
    assert_eq!(
        after.topology(id).unwrap(),
        topology,
        "OpenCASCADE lost topology across BREP"
    );
    assert_eq!(after.bounds(id).unwrap(), bounds);

    // And it still tessellates, which is the thing a viewport needs and the
    // thing a shape that "loaded" but is subtly broken tends to fail at.
    let mesh = after.mesh(id).unwrap();
    assert!(mesh.triangle_count() > 0);
    assert_eq!(
        mesh.face_of_triangle.iter().copied().max().unwrap() + 1,
        topology.faces,
        "a face went missing from the mesh"
    );
}

#[test]
fn the_manifest_names_opencascade_so_another_build_can_refuse_it() {
    let bytes = save(&drilled_plate()).unwrap();
    let entries = w3d_format::zip::read(&bytes).unwrap();
    let manifest = String::from_utf8(entries["manifest.json"].clone()).unwrap();
    assert!(
        manifest.contains("\"geometry\": \"occt-brep-1\""),
        "{manifest}"
    );
}

/// The other half of the refusal: a file written by the fake kernel must not
/// open here either. Both directions matter — a one-way check would pass while
/// half the failure mode is live.
#[test]
fn a_document_from_the_fake_kernel_is_refused() {
    let mut fake = Document::new(w3d_kernel_fake::FakeKernel::default());
    fake.add_box("A", Vec3::new(1.0, 1.0, 1.0)).unwrap();
    let bytes = save(&fake).unwrap();

    match load(OcctKernel::new(), &bytes) {
        Err(FormatError::WrongKernel { file, kernel }) => {
            assert_eq!(file, "fake-csg-1");
            assert_eq!(kernel, "occt-brep-1");
        }
        Err(e) => panic!("refused for the wrong reason: {e}"),
        Ok(_) => panic!("a fake-kernel document opened in OpenCASCADE"),
    }
}
