//! The document driven against real geometry.
//!
//! `core`'s own tests prove the document against a fake, which proves the
//! machinery and nothing about geometry. This file is the other half: the same
//! `Document`, unchanged and still naming no backend, with OpenCASCADE
//! underneath actually cutting metal.

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_occt::OcctKernel;

#[test]
fn drilling_a_plate_produces_the_topology_a_drilled_plate_has() {
    let mut d = Document::new(OcctKernel::new());
    let plate = d.add_box("Plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = d.add_cylinder("Drill", 6.0, 30.0).unwrap();

    let plate_topo = d.topology(plate).unwrap();
    assert_eq!(
        (plate_topo.faces, plate_topo.edges, plate_topo.vertices),
        (6, 12, 8)
    );
    // OCCT's cylinder: the lateral surface plus two caps, and a seam edge.
    let drill_topo = d.topology(drill).unwrap();
    assert_eq!(drill_topo.faces, 3);

    let holed = d.boolean(BooleanOp::Difference, plate, drill).unwrap();
    let topo = d.topology(holed).unwrap();

    // Six planar faces and one cylindrical one; the hole adds its seam edge and
    // two circles, and the seam's two vertices. If a future kernel disagrees
    // with these numbers it has done something different, not something wrong —
    // but it should have to say so out loud.
    assert_eq!(
        (topo.solids, topo.faces, topo.edges, topo.vertices),
        (1, 7, 15, 10),
        "a plate with a hole through it"
    );

    // Drilling removes material; it cannot move the bounding box.
    assert_eq!(d.bounds(holed).unwrap(), {
        let half = Vec3::new(20.0, 20.0, 5.0);
        w3d_core::kernel::Aabb::new(-half, half)
    });
}

#[test]
fn every_face_reaches_the_mesh_with_its_own_id() {
    // This is what ID-buffer picking and per-face selection will read, so a
    // face that tessellates into no triangles is a face a user cannot click.
    let mut d = Document::new(OcctKernel::new());
    let plate = d.add_box("Plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = d.add_cylinder("Drill", 6.0, 30.0).unwrap();
    let holed = d.boolean(BooleanOp::Difference, plate, drill).unwrap();

    let face_count = d.topology(holed).unwrap().faces;
    let mesh = d.mesh(holed).unwrap();
    assert!(mesh.triangle_count() > 0);

    let ids: std::collections::BTreeSet<u32> = mesh.face_of_triangle.iter().copied().collect();
    assert_eq!(
        ids.len() as u32,
        face_count,
        "{} of {face_count} faces produced triangles",
        ids.len()
    );
    assert_eq!(ids.iter().copied().max(), Some(face_count - 1));

    // Normals are unit, or the shading is wrong in a way nothing else catches.
    for (i, n) in mesh.normals.iter().enumerate() {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-4, "normal {i} has length {len}");
    }
}

#[test]
fn undo_restores_the_operands_of_a_real_boolean() {
    // The contract's immutability rule, end to end: OCCT keeps the operands
    // alive, so history can put the nodes back without re-running the cut.
    let mut d = Document::new(OcctKernel::new());
    let a = d.add_box("A", Vec3::splat(10.0)).unwrap();
    let b = d.add_sphere("B", 6.0).unwrap();
    let a_bounds = d.bounds(a).unwrap();

    let cut = d.boolean(BooleanOp::Difference, a, b).unwrap();
    assert!(d.node(a).is_err());
    assert!(d.topology(cut).unwrap().faces > 6, "the sphere left a dish");

    assert_eq!(d.undo(), Some("Difference"));
    assert_eq!(d.bounds(a).unwrap(), a_bounds);
    assert_eq!(d.node(b).unwrap().name, "B");
}

#[test]
fn a_transform_is_a_new_body_and_the_original_is_untouched() {
    let mut d = Document::new(OcctKernel::new());
    let id = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let before = d.bounds(id).unwrap();

    d.transform(id, &Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)))
        .unwrap();
    let after = d.bounds(id).unwrap();
    assert_eq!(after.center().x, 100.0);
    assert_eq!(after.size(), before.size(), "a translation is not a scale");

    d.undo();
    assert_eq!(d.bounds(id).unwrap(), before);
}

#[test]
fn the_document_releases_opencascade_shapes_it_no_longer_needs() {
    let mut d = Document::new(OcctKernel::new());
    let before = d.kernel().live_bodies();

    let a = d.add_box("A", Vec3::splat(10.0)).unwrap();
    let b = d.add_sphere("B", 6.0).unwrap();
    d.boolean(BooleanOp::Union, a, b).unwrap();
    assert_eq!(d.kernel().live_bodies(), before + 3);

    assert_eq!(d.collect_garbage(), 0, "history still refers to all three");
    d.clear_history();
    assert_eq!(d.collect_garbage(), 2);
    assert_eq!(d.kernel().live_bodies(), before + 1);
}
