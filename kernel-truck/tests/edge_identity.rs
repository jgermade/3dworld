//! What `Mesh::edge_of_line` says an edge is, checked against where the
//! segments actually are.
//!
//! `mesh_face` reads an edge's id from the *untriangulated* face, zipped
//! alongside the meshed one, because `triangulation` rebuilds the wires with
//! polylines for curves and an id taken off the copy belongs to a numbering
//! nothing else shares. A zip is only sound while the two walks have the same
//! shape, and `zip` truncates rather than complaining — so a mismatch would hand
//! back ids that are plausible and wrong, on a backend that then declines the
//! blend and never finds out.
//!
//! These assertions are the ones that would notice. They are about geometry the
//! test knows independently: a box has twelve edges, and the segments of one of
//! them are collinear.

use std::collections::BTreeMap;
use w3d_kernel::{GeometryKernel, Quality, Vec3};
use w3d_kernel_truck::TruckKernel;

/// A wireframe segment as this test reads one: its two endpoints.
type Segment = ([f32; 3], [f32; 3]);

#[test]
fn a_boxs_segments_group_into_its_twelve_edges() {
    let mut k = TruckKernel::new();
    let body = k.create_box(Vec3::new(2.0, 3.0, 4.0)).expect("box");
    let mesh = k
        .tessellate(body, Quality::display_default())
        .expect("tessellate");

    assert_eq!(
        mesh.edge_of_line.len(),
        mesh.line_count(),
        "one edge id per segment"
    );

    let mut by_edge: BTreeMap<u32, Vec<Segment>> = BTreeMap::new();
    for seg in 0..mesh.line_count() {
        let a = mesh.line_positions[mesh.line_indices[seg * 2] as usize];
        let b = mesh.line_positions[mesh.line_indices[seg * 2 + 1] as usize];
        let id = mesh.edge_of_line(seg).expect("truck reports edge identity");
        by_edge.entry(id).or_default().push((a, b));
    }

    // A box has twelve edges. Not "at most twelve": a zip that slipped would
    // merge two of them under one id or invent a thirteenth.
    assert_eq!(
        by_edge.len(),
        12,
        "a box's segments should group into 12 edges, got {}",
        by_edge.len()
    );

    // And each group is one straight edge of the box: every segment under one id
    // is parallel to the same axis and pinned to the same two coordinates on the
    // other two. This is what distinguishes a correct grouping from a
    // consistent-looking permutation of one.
    for (id, segments) in &by_edge {
        let (a0, b0) = segments[0];
        let axis = (0..3)
            .find(|&i| (b0[i] - a0[i]).abs() > 1.0e-4)
            .unwrap_or_else(|| panic!("edge {id} has a zero-length first segment"));
        for &(a, b) in segments {
            for i in 0..3 {
                if i == axis {
                    continue;
                }
                assert!(
                    (a[i] - a0[i]).abs() < 1.0e-4 && (b[i] - a0[i]).abs() < 1.0e-4,
                    "edge {id} has segments that are not on one line: \
                     {a0:?}-{b0:?} and {a:?}-{b:?}"
                );
            }
            assert!(
                (b[axis] - a[axis]).abs() > 1.0e-4,
                "edge {id} has a segment across its own axis"
            );
        }
    }
}

#[test]
fn a_cylinders_ids_stay_within_its_own_edges() {
    // A curved body, where the polylines the zip walks are many segments per
    // edge rather than one — the case where a slip is easiest and least visible.
    let mut k = TruckKernel::new();
    let body = k.create_cylinder(1.0, 3.0).expect("cylinder");
    let mesh = k
        .tessellate(body, Quality::display_default())
        .expect("tessellate");
    let topology = k.topology(body).expect("topology");

    assert_eq!(mesh.edge_of_line.len(), mesh.line_count());
    let highest = mesh.edge_of_line.iter().copied().max().expect("segments");
    assert!(
        highest < topology.edges,
        "a segment names edge {highest} of a body with {} edges",
        topology.edges
    );

    // Every segment of one id lies on one circle or one straight seam, so at the
    // very least no id spans the whole height *and* wanders in z — the shape of
    // failure a mismatched zip produces.
    let mut spans: BTreeMap<u32, (f32, f32)> = BTreeMap::new();
    for seg in 0..mesh.line_count() {
        let a = mesh.line_positions[mesh.line_indices[seg * 2] as usize];
        let b = mesh.line_positions[mesh.line_indices[seg * 2 + 1] as usize];
        let id = mesh.edge_of_line(seg).expect("identity");
        let e = spans.entry(id).or_insert((f32::MAX, f32::MIN));
        for z in [a[2], b[2]] {
            e.0 = e.0.min(z);
            e.1 = e.1.max(z);
        }
    }
    for (id, (lo, hi)) in spans {
        assert!(
            (hi - lo) < 3.0 + 1.0e-3,
            "edge {id} spans {lo}..{hi} in z, more than the cylinder's own height"
        );
    }
}
