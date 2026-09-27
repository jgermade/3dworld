//! A face id and an edge id name the same face and edge whatever order the
//! topology holds them in.
//!
//! Ids are positions in `face_key` order and in the walk `number_edges` makes,
//! and a selection, a pick and a per-edge blend are stored against them. The
//! order a boolean hands its faces back in comes from a map keyed by address,
//! so it changes between processes; an id that followed it would move a user's
//! selection between two runs. As with `save_order.rs`, the shuffle is done on
//! purpose rather than waited for: the solid is saved, every free order in it
//! changed, and loaded again.

mod common;

use common::{cut, scramble, slot};
use w3d_kernel::{Body, GeometryKernel, Mesh, Quality};
use w3d_kernel_truck::TruckKernel;

/// Where each id's triangles (or segments) are, in id order: the mean of the
/// points it owns. Two meshes that number the same geometry the same way give
/// the same list; a renumbering gives the same points in another order.
fn centres(points: &[[f32; 3]], owners: impl Iterator<Item = (u32, usize)>) -> Vec<[i64; 3]> {
    let mut sums: Vec<[f64; 4]> = Vec::new();
    for (id, point) in owners {
        let id = id as usize;
        if sums.len() <= id {
            sums.resize(id + 1, [0.0; 4]);
        }
        for a in 0..3 {
            sums[id][a] += f64::from(points[point][a]);
        }
        sums[id][3] += 1.0;
    }
    // Rounded to a micrometre: the claim is about which face has which id,
    // and the two meshes are of one solid, so they agree far below this.
    sums.iter()
        .map(|s| [0, 1, 2].map(|a| (s[a] / s[3] * 1000.0).round() as i64))
        .collect()
}

fn face_centres(m: &Mesh) -> Vec<[i64; 3]> {
    let owners = m
        .face_of_triangle
        .iter()
        .enumerate()
        .flat_map(|(t, &f)| (0..3).map(move |c| (f, m.indices[t * 3 + c] as usize)));
    centres(&m.positions, owners)
}

fn edge_centres(m: &Mesh) -> Vec<[i64; 3]> {
    let owners = m
        .edge_of_line
        .iter()
        .enumerate()
        .flat_map(|(l, &e)| (0..2).map(move |c| (e, m.line_indices[l * 2 + c] as usize)));
    centres(&m.line_positions, owners)
}

fn same_ids(make: fn(&mut TruckKernel) -> Body, what: &str) {
    let mut k = TruckKernel::new();
    let saved = {
        let body = make(&mut k);
        k.save_body(body).expect("save")
    };
    // Both sides are loaded, and that matters: the solid the boolean returned
    // holds its faces in whatever order the allocator gave, which may happen
    // to agree with the scramble — and this test then passed with the
    // tiebreak removed. The saved file is in canonical order, and the scramble
    // *reverses* it, so any two faces the sort cannot tell apart are
    // guaranteed to arrive in opposite orders.
    let plain = k.load_body(&saved).expect("load");
    let scrambled = k.load_body(&scramble(&saved)).expect("load the scramble");

    let q = Quality::display_default();
    let (a, b) = (
        k.tessellate(plain, q).expect("mesh"),
        k.tessellate(scrambled, q).expect("mesh the scramble"),
    );
    assert_eq!(
        face_centres(&a),
        face_centres(&b),
        "{what}: a face id names a different face once the topology's order changes"
    );
    assert_eq!(
        edge_centres(&a),
        edge_centres(&b),
        "{what}: an edge id names a different edge once the topology's order changes"
    );
}

#[test]
fn two_faces_on_one_plane_keep_their_ids() {
    same_ids(slot, "the slot");
}

#[test]
fn the_drilled_plate_keeps_its_ids() {
    same_ids(cut, "the drilled plate");
}
