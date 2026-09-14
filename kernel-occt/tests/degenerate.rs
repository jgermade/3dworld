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
    assert!(
        topo.faces == 10 || topo.faces == 6,
        "OCCT coplanar faces count: got {}",
        topo.faces
    );
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

#[test]
fn revolve_profile_degrees_and_bounds() {
    let mut d = Document::new(OcctKernel::new());
    let prof = w3d_core::kernel::Profile::Rectangle {
        width: 10.0,
        height: 20.0,
    };

    // An axis through the middle of the profile is the degenerate case, and it
    // belongs in this file: a profile is centred on the origin, so a turn about
    // an axis through the origin sweeps each half of it over the other and the
    // result covers itself twice. OpenCASCADE refuses it, which is the right
    // answer and the one this suite exists to pin — it is *not* a crash, and it
    // is not a solid either. This test asked for it and passed until
    // 2026-09-14, because the backend was quietly substituting a cylinder for
    // the profile and never revolving anything at all.
    let refused = d.add_revolve(
        "Degenerate",
        &prof,
        Vec3::ZERO,
        Vec3::Y,
        std::f64::consts::PI,
    );
    let message = refused
        .expect_err("a profile turned about its own middle")
        .to_string();
    assert!(
        message.contains("revolved"),
        "refused, but without saying what was wrong: {message}"
    );

    // Beside the axis it is an ordinary half tube, and the arithmetic is
    // Pappus's: half of 2 pi R A, with R = 20 and A = 200.
    let revolved = d
        .add_revolve(
            "Revolved",
            &prof,
            Vec3::new(-20.0, 0.0, 0.0),
            Vec3::Y,
            std::f64::consts::PI,
        )
        .unwrap();
    let bounds = d.bounds(revolved).unwrap();
    // The profile spans x from -5 to 5, so its distance from the axis at
    // x = -20 runs 15 to 25. Half a turn leaves the starting face where it was
    // and carries its far edge round to x = -45, so the span is 50 — not 45,
    // which is what this test asked for on its first draft and is the radius
    // rather than the reach.
    assert!((bounds.size().x - 50.0).abs() < 1e-6, "{bounds:?}");
    assert!((bounds.size().y - 20.0).abs() < 1e-6, "{bounds:?}");
    assert!((bounds.size().z - 25.0).abs() < 1e-6, "{bounds:?}");
}

#[test]
fn sweep_and_loft_degenerate_and_valid_inputs() {
    let mut d = Document::new(OcctKernel::new());
    let prof = w3d_core::kernel::Profile::Circle { radius: 5.0 };
    let pts = [Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)];
    let swept = d.add_sweep("Swept", &prof, &pts).unwrap();
    assert!(d.bounds(swept).unwrap().size().z > 0.0);

    let profiles = [
        w3d_core::kernel::Profile::Circle { radius: 10.0 },
        w3d_core::kernel::Profile::Circle { radius: 5.0 },
    ];
    let planes = [
        w3d_core::kernel::SketchPlane::default(),
        w3d_core::kernel::SketchPlane {
            origin: Vec3::new(0.0, 0.0, 20.0),
            x_axis: Vec3::X,
            y_axis: Vec3::Y,
        },
    ];
    let lofted = d.add_loft("Lofted", &profiles, &planes).unwrap();
    assert!(d.bounds(lofted).unwrap().size().z > 0.0);
}
