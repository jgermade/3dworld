//! A blend that rounds the edge it was asked for, and leaves the rest sharp.
//!
//! The whole-solid `fillet` has been checked since 2026-09-16 and it cannot
//! answer the question this file exists for: a backend that rounds *every* edge
//! passes every assertion about "a fillet takes material off". So the numbers
//! here are about **one** edge, and the arithmetic is what separates the two —
//! rounding one edge of a cube of side `l` at radius `r` removes
//! `l * r^2 * (1 - pi/4)` and rounding twelve removes several times that.
//!
//! Like `push_pull.rs`, it runs against whichever backend the build has, and
//! the two are pinned apart only where they genuinely differ: OpenCASCADE
//! blends, `truck` has no rolling-ball surface and says so.
#![cfg(any(feature = "truck", feature = "occt"))]

use w3d_core::Document;
use w3d_core::kernel::{Mesh, Vec3};

#[cfg(feature = "occt")]
use w3d_kernel_occt::OcctKernel as Kernel;
#[cfg(all(feature = "truck", not(feature = "occt")))]
use w3d_kernel_truck::TruckKernel as Kernel;

/// The volume the mesh encloses, by the divergence theorem.
fn volume(mesh: &Mesh) -> f64 {
    let p = |i: usize| {
        let v = mesh.positions[i];
        Vec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))
    };
    (0..mesh.triangle_count())
        .map(|t| {
            let a = p(mesh.indices[3 * t] as usize);
            let b = p(mesh.indices[3 * t + 1] as usize);
            let c = p(mesh.indices[3 * t + 2] as usize);
            a.dot(b.cross(c)) / 6.0
        })
        .sum()
}

/// The edge id of one *named* edge of a box: the one where the `+y` and `+z`
/// faces meet, found by its geometry rather than by guessing an index.
///
/// Going through `edge_of_line` on purpose — that is the map the editor uses
/// when a user clicks, so a test that took the id another way would be checking
/// a path nobody walks.
fn top_front_edge(mesh: &Mesh, half: f32) -> Option<u32> {
    let on = |v: [f32; 3]| (v[1] - half).abs() < 0.01 && (v[2] - half).abs() < 0.01;
    (0..mesh.line_count()).find_map(|seg| {
        let a = mesh.line_positions[mesh.line_indices[seg * 2] as usize];
        let b = mesh.line_positions[mesh.line_indices[seg * 2 + 1] as usize];
        (on(a) && on(b)).then(|| mesh.edge_of_line(seg)).flatten()
    })
}

#[test]
fn a_per_edge_fillet_rounds_that_edge_and_leaves_the_others_sharp() {
    let mut doc = Document::new(Kernel::default());
    let id = doc
        .add_box("Cube", Vec3::new(20.0, 20.0, 20.0))
        .expect("box");

    let before = doc.mesh(id).expect("mesh").clone();
    let v0 = volume(&before);
    assert!((v0 - 8000.0).abs() < 1.0, "a 20-cube is 8000, got {v0}");

    let edge = top_front_edge(&before, 10.0);

    #[cfg(feature = "occt")]
    {
        let edge = edge.expect("OpenCASCADE reports which edge a segment came from");
        let faces_before = doc.topology(id).expect("topology").faces;

        doc.fillet_edges(id, &[edge], 2.0).expect("fillet one edge");

        let after = doc.mesh(id).expect("mesh");
        let v1 = volume(after);

        // One edge of a cube, rounded: the material lost is the difference
        // between the square corner and the quarter-cylinder inside it, along
        // the whole 20 mm of the edge.
        let expected = 20.0 * 2.0 * 2.0 * (1.0 - std::f64::consts::FRAC_PI_4);
        let removed = v0 - v1;
        assert!(
            (removed - expected).abs() < 1.0,
            "rounding one edge at r=2 should remove {expected:.2}, removed {removed:.2}"
        );

        // And the rest of the solid is untouched: exactly one face was added —
        // the blend surface — where rounding every edge adds twelve and eight
        // more at the corners.
        let faces_after = doc.topology(id).expect("topology").faces;
        assert_eq!(
            faces_after,
            faces_before + 1,
            "one rounded edge should add exactly one face"
        );

        // The box still reaches as far as it did on every side but the one the
        // blend cut into, which is the cheapest statement of "the others are
        // still sharp".
        let bounds = doc.bounds(id).expect("bounds");
        assert!(
            (bounds.min.x + 10.0).abs() < 0.01 && (bounds.max.x - 10.0).abs() < 0.01,
            "a blend on one edge moved the solid's extent in x: {bounds:?}"
        );
    }

    #[cfg(all(feature = "truck", not(feature = "occt")))]
    {
        // This backend does report edge identity — it has a topology, it simply
        // has no rolling-ball surface to blend with — so the id is found and the
        // refusal comes from the operation rather than from a missing map.
        let edge = edge.expect("truck reports which edge a segment came from");
        let err = doc
            .fillet_edges(id, &[edge], 2.0)
            .expect_err("truck has no blender and must say so");
        assert!(
            err.to_string().contains("blending"),
            "the refusal should name what is missing, got {err}"
        );
        // And the part is untouched by a refusal.
        let v1 = volume(doc.mesh(id).expect("mesh"));
        assert!((v0 - v1).abs() < 1.0, "a refused blend changed the solid");
    }
}

#[test]
fn an_empty_edge_selection_is_refused_rather_than_taken_as_all_of_them() {
    let mut doc = Document::new(Kernel::default());
    let id = doc
        .add_box("Cube", Vec3::new(20.0, 20.0, 20.0))
        .expect("box");
    let v0 = volume(&doc.mesh(id).expect("mesh").clone());

    doc.fillet_edges(id, &[], 2.0)
        .expect_err("an empty selection must not mean every edge");

    let v1 = volume(doc.mesh(id).expect("mesh"));
    assert!(
        (v0 - v1).abs() < 1.0,
        "an empty edge selection changed the solid: {v0} became {v1}"
    );
}

#[test]
fn a_per_edge_fillet_takes_less_than_rounding_the_whole_solid() {
    // The assertion the whole-solid check cannot make, and the one that would
    // have caught the gizmo issuing `FilletRadius` for a handle drawn on one
    // edge: if these two came out equal, the per-edge form would be a rename.
    #[cfg(feature = "occt")]
    {
        let mut one = Document::new(Kernel::default());
        let a = one
            .add_box("Cube", Vec3::new(20.0, 20.0, 20.0))
            .expect("box");
        let edge = top_front_edge(&one.mesh(a).expect("mesh").clone(), 10.0).expect("edge id");
        one.fillet_edges(a, &[edge], 2.0).expect("one edge");
        let v_one = volume(one.mesh(a).expect("mesh"));

        let mut all = Document::new(Kernel::default());
        let b = all
            .add_box("Cube", Vec3::new(20.0, 20.0, 20.0))
            .expect("box");
        all.fillet(b, 2.0).expect("every edge");
        let v_all = volume(all.mesh(b).expect("mesh"));

        assert!(
            v_all < v_one - 10.0,
            "rounding every edge ({v_all:.2}) should take clearly more than \
             rounding one ({v_one:.2})"
        );
    }
}
