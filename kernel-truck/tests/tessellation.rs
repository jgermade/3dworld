//! What the `parallel` feature is and is not allowed to change.
//!
//! The feature exists so the browser's threaded variant has something to do
//! with its cores. A speed switch in a geometry kernel is only acceptable if
//! nothing above the seam can tell it was thrown, and "nothing" here has a
//! precise meaning: the same bytes, not merely the same shape. `face_of_triangle`
//! is face identity — what a selection and a per-face fillet are stored
//! against — so a mesh whose face numbering moved with the core count would
//! move a user's selection between two runs of the same build.
//!
//! The fingerprint below is therefore pinned to a literal rather than compared
//! between two runs in one process: a build with the feature and a build
//! without it are different programs, and the only way one test can hold both
//! to the same answer is for the answer to be written down. `make check` runs
//! this file twice, once each way.

use w3d_kernel::{GeometryKernel, Mat4, Mesh, Quality, Vec3};
use w3d_kernel_truck::TruckKernel;

/// Everything about a mesh that a thread count could plausibly disturb:
/// how much there is, where it is, and which face each triangle belongs to.
///
/// The position checksum is over raw `f32` bits, so it moves if any coordinate
/// changes at all — a tolerance here would hide exactly the drift it is meant
/// to catch. It is order-sensitive by construction: a merge that ran the faces
/// in a different order produces the same *set* of vertices and a different
/// checksum, which is the failure this test is for.
fn fingerprint(mesh: &Mesh) -> (usize, usize, usize, u64, u64) {
    let mut positions: u64 = 0;
    for (i, p) in mesh.positions.iter().enumerate() {
        for (j, v) in p.iter().enumerate() {
            positions = positions
                .rotate_left(7)
                .wrapping_add(v.to_bits() as u64)
                .wrapping_add((i * 3 + j) as u64);
        }
    }
    let mut faces: u64 = 0;
    for (i, f) in mesh.face_of_triangle.iter().enumerate() {
        faces = faces.rotate_left(5).wrapping_add(*f as u64 + i as u64);
    }
    (
        mesh.positions.len(),
        mesh.normals.len(),
        mesh.indices.len(),
        positions,
        faces,
    )
}

/// Two solids, chosen for what they make the merge do rather than for what
/// they look like.
///
/// A box is six planar faces of equal size — the case where an order bug is
/// invisible in a triangle count and shows up only in a checksum. A cylinder
/// is a curved lateral surface and two caps: different surface types, very
/// different vertex counts per face, and enough faces that the sort has
/// something to sort.
///
/// Deliberately *not* the drilled plate this backend's `boolean` appears to
/// produce. It does not: `BooleanOp::Difference` returns a copy of its first
/// operand here, so the "plate with a hole" is a plate. See the record file
/// for 2026-09-03 and the register item it opened — the fixture would have
/// been a box wearing a misleading name.
fn cases(k: &mut TruckKernel) -> [(&'static str, w3d_kernel::Body); 2] {
    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("box");
    let pin = k.create_cylinder(6.0, 20.0).expect("cylinder");
    let pin = k
        .transform(pin, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .expect("transform");
    [("box", plate), ("cylinder", pin)]
}

#[test]
fn tessellation_is_independent_of_the_thread_count() {
    let mut k = TruckKernel::new();
    for ((name, body), expected) in cases(&mut k).into_iter().zip(FINGERPRINTS) {
        let mesh = k
            .tessellate(body, Quality::display_default())
            .expect("tessellate");
        assert_eq!(
            fingerprint(&mesh),
            expected,
            "the {name} mesh changed. If this is a deliberate change to the \
             tessellator, re-record FINGERPRINTS and say so in a record file — \
             but if it moved because `parallel` was switched on or off, the \
             merge has stopped being order-preserving and every stored face id \
             is now wrong."
        );
    }
}

/// Recorded from the sequential path and asserted against both, in the order
/// [`cases`] returns. See the note at the top of the file on why these are
/// literals rather than two runs compared in one process.
const FINGERPRINTS: [(usize, usize, usize, u64, u64); 2] = [
    // box: six planar faces, four vertices each
    (24, 24, 36, 12909945243647881981, 1236065057125872),
    // cylinder: a curved lateral surface and two caps, 1266 triangles
    (1290, 1290, 3798, 15766414192103659986, 494696828649949194),
];

/// `w3d-web`'s threaded variant tessellates through `rayon`, which requires the
/// kernel to be shared across threads. Nothing in the seam demands this —
/// `OcctKernel` is neither `Send` nor `Sync` and is right not to be — so it is
/// a property of *this* backend, and one the browser build silently depends on.
/// Asserted here so that losing it is a failure in this crate rather than an
/// error inside a wasm build somebody runs once a night.
#[test]
fn the_kernel_can_be_shared_across_threads() {
    fn require<T: Send + Sync>() {}
    require::<TruckKernel>();
}
