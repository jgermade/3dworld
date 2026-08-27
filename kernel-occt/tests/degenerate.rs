//! Fixture regression suite for degenerate input, tangencies, and micro-tolerances.
//!
//! B-rep kernels (and OpenCASCADE specifically) fail on degenerate geometry:
//! coincident faces, tangent surfaces, thin walls, and micro-tolerances.
//! This suite exercises these edge cases against `OcctKernel` to verify that
//! operations either succeed with clean topology or fail with a clear error,
//! never crashing, panicking, or corrupting memory.

use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_occt::OcctKernel;

#[test]
fn coincident_faces_boolean_union_merges_adjacent_boxes() {
    let mut d = Document::new(OcctKernel::new());
    let a = d.add_box("A", Vec3::new(20.0, 20.0, 20.0)).unwrap();
    let b = d.add_box("B", Vec3::new(20.0, 20.0, 20.0)).unwrap();

    // Move B right next to A so they share the face at X = 10.0
    d.transform(b, &Mat4::from_translation(Vec3::new(20.0, 0.0, 0.0)))
        .unwrap();

    let united = d.boolean(BooleanOp::Union, a, b).unwrap();
    let topo = d.topology(united).unwrap();
    let bounds = d.bounds(united).unwrap();

    // Merged single solid of size (40, 20, 20). OCCT retains coplanar face patches
    // (10 faces total) unless shape unification (ShapeUpgrade_UnifySameDomain) is run.
    assert_eq!(topo.solids, 1);
    assert_eq!(topo.faces, 10, "OCCT un-unified coplanar faces count");
    assert_eq!(bounds.size(), Vec3::new(40.0, 20.0, 20.0));
}

#[test]
fn coincident_faces_difference_returns_original_operand() {
    let mut d = Document::new(OcctKernel::new());
    let a = d.add_box("A", Vec3::new(20.0, 20.0, 20.0)).unwrap();
    let b = d.add_box("B", Vec3::new(20.0, 20.0, 20.0)).unwrap();

    // Move B adjacent to A (sharing face at X = 10.0)
    d.transform(b, &Mat4::from_translation(Vec3::new(20.0, 0.0, 0.0)))
        .unwrap();

    let diff = d.boolean(BooleanOp::Difference, a, b).unwrap();
    let bounds = d.bounds(diff).unwrap();
    assert_eq!(bounds.size(), Vec3::new(20.0, 20.0, 20.0));
}

#[test]
fn tangent_cylinder_to_box_face_boolean() {
    let mut d = Document::new(OcctKernel::new());
    let box_id = d.add_box("Box", Vec3::new(20.0, 20.0, 20.0)).unwrap();
    let cyl = d.add_cylinder("Cyl", 5.0, 20.0).unwrap();

    // Move cylinder so its side is tangent to the box face at X = 10.0 (center at X = 15.0)
    d.transform(cyl, &Mat4::from_translation(Vec3::new(15.0, 0.0, 0.0)))
        .unwrap();

    let united = d.boolean(BooleanOp::Union, box_id, cyl).unwrap();
    let topo = d.topology(united).unwrap();
    // Tangent 1D line contact produces a 2-solid compound in OCCT without volume penetration
    assert_eq!(topo.solids, 2);

    let mesh = d.mesh(united).unwrap();
    assert!(mesh.triangle_count() > 0);
}

#[test]
fn touching_spheres_at_a_single_point() {
    let mut d = Document::new(OcctKernel::new());
    let s1 = d.add_sphere("S1", 10.0).unwrap();
    let s2 = d.add_sphere("S2", 10.0).unwrap();

    // Distance between centers = 20.0 (touching tangentially at 1 point)
    d.transform(s2, &Mat4::from_translation(Vec3::new(20.0, 0.0, 0.0)))
        .unwrap();

    let res = d.boolean(BooleanOp::Union, s1, s2);
    assert!(
        res.is_ok(),
        "union of point-touching spheres must not panic"
    );
}

#[test]
fn thin_wall_box_primitive() {
    let mut d = Document::new(OcctKernel::new());
    // Box with 1e-4 mm thickness
    let thin = d.add_box("Thin", Vec3::new(10.0, 10.0, 1e-4)).unwrap();
    let topo = d.topology(thin).unwrap();
    assert_eq!(
        (topo.solids, topo.faces, topo.edges, topo.vertices),
        (1, 6, 12, 8)
    );

    let bounds = d.bounds(thin).unwrap();
    assert!((bounds.size().z - 1e-4).abs() < 1e-6);
}

#[test]
fn micro_radius_fillet() {
    let mut d = Document::new(OcctKernel::new());
    let box_id = d.add_box("Box", Vec3::new(10.0, 10.0, 10.0)).unwrap();

    // Extremely small fillet radius (0.01 mm)
    let filleted = d.fillet(box_id, 0.01);
    assert!(filleted.is_ok(), "micro fillet on box edges succeeds");
}
