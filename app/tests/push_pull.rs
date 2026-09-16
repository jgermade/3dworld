//! Push/pull against a kernel that actually builds solids.
//!
//! `w3d-core`'s own suite runs on the fake kernel, which keeps a boolean
//! symbolic: it can say the edit has the right *shape* — one undo step, a new
//! body, the bounds on one side — and cannot say the solid is right. This is
//! the other half, and it lives here because `w3d-app` is the one crate that
//! depends on both the document and a real backend.
//!
//! It runs against whichever backend the build has, and `--features occt` is a
//! different run of the same assertions rather than a different test: the two
//! backends disagree about *how* a pull is built — OpenCASCADE joins the
//! coplanar sides `truck` will not — and must not disagree about the solid.
#![cfg(any(feature = "truck", feature = "occt"))]

use w3d_core::Document;
use w3d_core::kernel::{Mesh, Vec3};

#[cfg(feature = "occt")]
use w3d_kernel_occt::OcctKernel as Kernel;
#[cfg(all(feature = "truck", not(feature = "occt")))]
use w3d_kernel_truck::TruckKernel as Kernel;

/// The volume the mesh encloses, by the divergence theorem. Positive for a
/// solid wound the way the kernel contract says.
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

fn face_pointing(d: &mut Document<Kernel>, id: w3d_core::NodeId, axis: Vec3) -> u32 {
    let mesh = d.mesh(id).unwrap().clone();
    let mut ids: Vec<u32> = mesh.face_of_triangle.clone();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .find(|&f| {
            mesh.face_metrics(f)
                .is_some_and(|m| m.normal.dot(axis) > 0.99)
        })
        .expect("no face of this solid points that way")
}

#[test]
fn pulling_the_top_of_a_box_makes_a_taller_box() {
    let mut d = Document::new(Kernel::default());
    let id = d.add_box("Base", Vec3::new(10.0, 10.0, 10.0)).unwrap();
    let before = d.bounds(id).unwrap();
    let v0 = volume(d.mesh(id).unwrap());
    let top = face_pointing(&mut d, id, Vec3::Z);

    let done = d.push_pull_face(id, top, 4.0).unwrap();

    // The two backends reach the same solid by different routes, and each one
    // is pinned here rather than left to whichever is being built.
    // OpenCASCADE joins the prism's sides to the box's coplanar ones, so the
    // exact attempt is the one that answers; `truck` cannot, so it falls to the
    // slack attempt — which must still be under the tolerance the boolean
    // itself ran at, a thousandth of the solid.
    #[cfg(feature = "occt")]
    assert!(
        done.is_exact(),
        "OpenCASCADE should not need slack, and took {}",
        done.slack
    );
    #[cfg(all(feature = "truck", not(feature = "occt")))]
    assert!(
        done.slack > 0.0 && done.slack < 10.0 * 1.0e-3,
        "truck should take the slack path, under the boolean's own tolerance: {}",
        done.slack
    );

    let after = d.bounds(id).unwrap();
    let v1 = volume(d.mesh(id).unwrap());
    assert!(
        (after.max.z - (before.max.z + 4.0)).abs() < 1.0e-3,
        "{before:?} -> {after:?}"
    );
    assert!(
        (after.min.z - before.min.z).abs() < 1.0e-3,
        "the far side moved, so this was a translation and not a pull: {before:?} -> {after:?}"
    );
    assert!(
        (v1 - (v0 + 400.0)).abs() < 1.0,
        "a 10x10 face pulled 4 should add 400 of material, not {}",
        v1 - v0
    );
}

#[test]
fn pushing_the_top_of_a_box_takes_material_out_of_it() {
    let mut d = Document::new(Kernel::default());
    let id = d.add_box("Base", Vec3::new(10.0, 10.0, 10.0)).unwrap();
    let before = d.bounds(id).unwrap();
    let v0 = volume(d.mesh(id).unwrap());
    let top = face_pointing(&mut d, id, Vec3::Z);

    let done = d.push_pull_face(id, top, -3.0).unwrap();
    assert!(done.slack < 10.0 * 1.0e-3, "slack {}", done.slack);
    #[cfg(feature = "occt")]
    assert!(done.is_exact(), "slack {}", done.slack);

    let after = d.bounds(id).unwrap();
    let v1 = volume(d.mesh(id).unwrap());
    assert!(
        (after.max.z - (before.max.z - 3.0)).abs() < 1.0e-3,
        "{before:?} -> {after:?}"
    );
    assert!(
        (after.min.z - before.min.z).abs() < 1.0e-3,
        "{before:?} -> {after:?}"
    );
    assert!(
        (v1 - (v0 - 300.0)).abs() < 1.0,
        "a 10x10 face pushed 3 should remove 300 of material, not {}",
        v0 - v1
    );
}

/// Whether a face's own triangles disagree about which way they point.
///
/// Worked out from the triangles rather than from `face_metrics`, so that this
/// is an independent opinion about which face is curved: a cylinder's side
/// averages to no normal at all, since the outward directions cancel around the
/// axis, and asking the code under test to point out its own hard case proves
/// nothing.
fn is_curved(mesh: &Mesh, face_id: u32) -> bool {
    let p = |i: usize| {
        let v = mesh.positions[i];
        Vec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))
    };
    let normals: Vec<Vec3> = (0..mesh.triangle_count())
        .filter(|t| mesh.face_of_triangle.get(*t).copied() == Some(face_id))
        .filter_map(|t| {
            let a = p(mesh.indices[3 * t] as usize);
            let b = p(mesh.indices[3 * t + 1] as usize);
            let c = p(mesh.indices[3 * t + 2] as usize);
            (b - a).cross(c - a).normalize(1.0e-12)
        })
        .collect();
    let Some(&first) = normals.first() else {
        return false;
    };
    normals.iter().any(|n| n.dot(first) < 0.999)
}

#[test]
fn a_curved_face_is_refused_instead_of_moving_the_whole_part() {
    let mut d = Document::new(Kernel::default());
    let id = d.add_cylinder("Pin", 5.0, 20.0).unwrap();
    let before = d.bounds(id).unwrap();

    let mesh = d.mesh(id).unwrap().clone();
    let mut ids: Vec<u32> = mesh.face_of_triangle.clone();
    ids.sort_unstable();
    ids.dedup();
    let curved: Vec<u32> = ids.into_iter().filter(|&f| is_curved(&mesh, f)).collect();
    assert!(
        !curved.is_empty(),
        "a tessellated cylinder has at least one curved face"
    );

    for face in curved {
        let refused = d.push_pull_face(id, face, 5.0);
        assert!(
            refused.is_err(),
            "face #{face} is curved and was pulled as if it were flat"
        );
        assert_eq!(
            d.bounds(id).unwrap(),
            before,
            "the refusal of face #{face} still moved the part"
        );
    }
}

/// The modeller's own path: select a face, pull it, pull it again.
#[test]
fn a_second_pull_lands_on_the_face_the_first_one_left_behind() {
    use w3d_app::{Command, Editor};
    use w3d_render::Pick;

    let mut e = Editor::new(Kernel::default());
    e.run(Command::AddBox);
    let id = e.selection()[0];
    let before = e.document().bounds(id).unwrap();

    // Pick a face, whichever one the id buffer would have handed back.
    e.picked(
        Pick {
            object: id.index(),
            face: 1,
        },
        false,
    );
    let (_, first) = e.selected_face().expect("a face is selected");
    let normal = e.face_metrics(id, first).unwrap().normal;

    e.run(Command::PushPullFace(3.0));
    let once = e.document().bounds(id).unwrap();
    assert!(once != before, "{}", e.status());

    // The face id is not the same one — the solid was rebuilt — but the
    // selection has followed the geometry, so the next pull is on the same
    // face of the same part.
    let (_, second) = e
        .selected_face()
        .expect("the pulled face is still selected");
    let moved = e.face_metrics(id, second).unwrap();
    assert!(
        moved.normal.dot(normal) > 0.99,
        "the selection jumped to a face pointing somewhere else"
    );

    e.run(Command::PushPullFace(3.0));
    let twice = e.document().bounds(id).unwrap();
    assert!(twice != once, "the second pull did nothing: {}", e.status());
    let grew = (twice.max - once.max).length() + (twice.min - once.min).length();
    assert!((grew - 3.0).abs() < 0.05, "grew by {grew}");
}

/// What the edge handles issue, against a backend that can and one that cannot.
///
/// The blend handles were the part of the manipulators nothing had exercised:
/// `truck` answers `Unsupported` for both, so until OpenCASCADE was built here
/// the only thing under test was that the refusal reached the status line.
#[test]
fn the_blend_a_handle_asks_for_is_rounded_or_refused_by_name() {
    use w3d_app::{Command, Editor};

    let mut e = Editor::new(Kernel::default());
    e.run(Command::AddBox);
    let id = e.selection()[0];
    let before = e.document().bounds(id).unwrap();
    let v0 = volume(e.document_mut().mesh(id).unwrap());

    e.run(Command::FilletRadius(1.0));
    let status = e.status().to_string();

    if cfg!(feature = "occt") {
        assert!(status.contains("filleted"), "{status}");
        let v1 = volume(e.document_mut().mesh(id).unwrap());
        assert!(
            v1 < v0 && v1 > v0 * 0.9,
            "rounding the edges of a 20 mm box by 1 mm should shave a little off \
             {v0}, not take it to {v1}"
        );
        assert_eq!(
            e.document().bounds(id).unwrap(),
            before,
            "a fillet rounds the edges; it does not shrink the part"
        );
        assert_eq!(e.document_mut().undo(), Some("Fillet"));
        assert_eq!(e.document().bounds(id).unwrap(), before);
    } else {
        // The refusal is the backend's own sentence, and reaching the status
        // line is the whole of what the handle promises here.
        assert!(
            status.contains("blend") || status.contains("cannot"),
            "the backend's refusal did not reach the status line: {status}"
        );
        assert_eq!(e.document().bounds(id).unwrap(), before);
    }
}
