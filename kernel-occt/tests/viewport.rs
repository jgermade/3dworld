//! The whole loop, once: OpenCASCADE → the document → the GPU → a click that
//! names a face.
//!
//! Every other test in this repository proves one link. This is the only one
//! that proves they connect, and the thing it is really testing is
//! `Mesh::face_of_triangle` — the field the kernel contract carries *because*
//! ID-buffer picking is impossible without it. Until this file existed, that
//! justification was an argument.
//!
//! It skips, loudly, when there is no adapter. A container without a GPU is a
//! normal situation; a test that quietly passes in one is not.

use std::collections::BTreeSet;
use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Vec3};
use w3d_kernel_occt::OcctKernel;
use w3d_render::{Camera, Gpu, GpuMesh, Material, Object, Pick, Renderer, Viewport};

const W: u32 = 320;
const H: u32 = 240;
const GRID: u32 = 12;

fn gpu() -> Option<Gpu> {
    match pollster::block_on(async {
        let instance = Gpu::instance().await;
        Gpu::open(&instance, None).await
    }) {
        Ok(gpu) => {
            println!("adapter: {}", gpu.capabilities);
            Some(gpu)
        }
        Err(e) => {
            println!("SKIPPED: {e}");
            None
        }
    }
}

/// The drilled plate the topology test asserts: 40x40x10, a 12 mm hole through
/// it, seven faces.
fn drilled_plate() -> Document<OcctKernel> {
    let mut d = Document::new(OcctKernel::new());
    let plate = d.add_box("Plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = d.add_cylinder("Drill", 6.0, 30.0).unwrap();
    d.boolean(BooleanOp::Difference, plate, drill).unwrap();
    d
}

fn viewport(gpu: &Gpu) -> Viewport<'_> {
    Viewport {
        device: &gpu.device,
        queue: &gpu.queue,
        width: W,
        height: H,
    }
}

/// Every pick over a grid, and what each returned. Misses included — where the
/// background is, is part of the answer.
fn sweep(renderer: &mut Renderer, gpu: &Gpu, camera: &Camera, objects: &[Object<'_>]) -> Vec<Pick> {
    let vp = viewport(gpu);
    let mut out = Vec::new();
    for i in 1..GRID {
        for j in 1..GRID {
            out.push(renderer.pick(&vp, camera, W * i / GRID, H * j / GRID, objects));
        }
    }
    out
}

fn faces(picks: &[Pick]) -> BTreeSet<u32> {
    picks
        .iter()
        .filter_map(|p| p.hit())
        .map(|(_, f)| f)
        .collect()
}

#[test]
fn a_click_on_a_drilled_plate_names_the_face_under_the_cursor() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(&gpu.device, w3d_render::COLOR_FORMAT);
    let mut doc = drilled_plate();

    let (node, _) = doc.nodes().next().unwrap();
    let count = doc.topology(node).unwrap().faces;
    assert_eq!(count, 7, "six planar faces and the hole's cylindrical one");

    let mesh = doc.mesh(node).unwrap().clone();
    let gpu_mesh = GpuMesh::upload(
        &gpu.device,
        gpu.capabilities.max_buffer_size,
        "plate",
        &mesh,
    )
    .unwrap();
    // OCCT triangulates face by face with each face's own nodes, so no vertex
    // is shared between two faces and the indexed path holds. This is the
    // assumption `scene::per_vertex_faces` checks rather than trusts; here is
    // where a real kernel confirms it.
    assert!(
        !gpu_mesh.deindexed,
        "OCCT shared a vertex between two faces — the de-indexed path is in use"
    );

    let objects = [Object {
        mesh: &gpu_mesh,
        id: node.index(),
        material: Material::default(),
    }];

    let mut camera = Camera::default();
    camera.fit(&doc.visible_bounds());
    let picks = sweep(&mut renderer, &gpu, &camera, &objects);

    let hits: Vec<_> = picks.iter().filter_map(|p| p.hit()).collect();
    assert!(
        !hits.is_empty(),
        "nothing was under any of the sampled pixels"
    );
    assert!(
        hits.iter().all(|&(object, _)| object == node.index()),
        "a pick returned an id nobody asked for"
    );
    assert!(
        hits.iter().all(|&(_, face)| face < count),
        "a pick returned a face this body does not have"
    );
    assert!(
        faces(&picks).len() > 1,
        "every pixel came back as one face — face ids are not reaching the \
         fragment shader"
    );

    // A corner is background, not face 0 of body 0. This is the whole reason
    // `NOTHING` is `u32::MAX` rather than zero.
    let outside = renderer.pick(&viewport(&gpu), &camera, 2, 2, &objects);
    assert!(outside.hit().is_none(), "the corner picked {outside:?}");
}

#[test]
fn every_face_of_a_drilled_plate_is_reachable_by_a_click() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(&gpu.device, w3d_render::COLOR_FORMAT);
    let mut doc = drilled_plate();
    let (node, _) = doc.nodes().next().unwrap();
    let mesh = doc.mesh(node).unwrap().clone();
    let gpu_mesh = GpuMesh::upload(
        &gpu.device,
        gpu.capabilities.max_buffer_size,
        "plate",
        &mesh,
    )
    .unwrap();
    let objects = [Object {
        mesh: &gpu_mesh,
        id: node.index(),
        material: Material::default(),
    }];
    let bounds = doc.visible_bounds();

    // Four yaws at two elevations. The elevations are what matter, and the
    // reason is worth writing down: looking *exactly* down the axis of a
    // through hole, the hole is background — there is nothing behind it — so
    // the one face the boolean created is the one face a straight-on view can
    // never click. The first version of this test asserted the opposite and
    // was wrong about the geometry, not about the renderer.
    let mut seen = BTreeSet::new();
    let quarter = core::f64::consts::FRAC_PI_2;
    for elevation in [core::f64::consts::FRAC_PI_3, quarter / 3.0] {
        for turn in 0..4 {
            let mut camera = Camera {
                yaw: f64::from(turn) * quarter,
                pitch: elevation,
                ..Camera::default()
            };
            camera.fit(&bounds);
            seen.extend(faces(&sweep(&mut renderer, &gpu, &camera, &objects)));
        }
    }
    // And from below, for the face the top of the plate hides.
    let mut camera = Camera {
        yaw: 0.0,
        pitch: -core::f64::consts::FRAC_PI_3,
        ..Camera::default()
    };
    camera.fit(&bounds);
    seen.extend(faces(&sweep(&mut renderer, &gpu, &camera, &objects)));

    let count = doc.topology(node).unwrap().faces;
    assert_eq!(
        seen.len() as u32,
        count,
        "clicked {} of {count} faces: {seen:?}. A face nothing can click is a \
         face the modeller does not have.",
        seen.len()
    );
}
