//! The viewport, rendered offscreen and read back.
//!
//! Every test here needs a real adapter, and a container without one is a
//! normal situation rather than a failure — so they *skip*, loudly, with the
//! reason printed. A skipped test that looks passed is how a renderer stays
//! broken for a month; `cargo test -- --nocapture` shows which happened.
//!
//! What runs here is software rasterisation (lavapipe, in the container this
//! was written in). That proves the pipeline, the layouts, the shader and the
//! readback are right. It proves nothing about a real GPU's timing, and
//! nothing at all about WebGL2.

use w3d_core::Document;
use w3d_kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_fake::FakeKernel;
use w3d_render::{Camera, Gpu, GpuMesh, Material, Object, Renderer, Viewport};

const W: u32 = 256;
const H: u32 = 192;

struct Harness {
    gpu: Gpu,
    renderer: Renderer,
}

fn viewport(gpu: &Gpu) -> Viewport<'_> {
    Viewport {
        device: &gpu.device,
        queue: &gpu.queue,
        width: W,
        height: H,
    }
}

fn harness() -> Option<Harness> {
    let gpu = pollster::block_on(async {
        let instance = Gpu::instance().await;
        Gpu::open(&instance, None).await
    });
    match gpu {
        Ok(gpu) => {
            println!("adapter: {}", gpu.capabilities);
            if let Some(warning) = gpu.capabilities.degradation() {
                println!("degraded: {warning}");
            }
            let renderer = Renderer::new(&gpu.device);
            Some(Harness { gpu, renderer })
        }
        Err(e) => {
            println!("SKIPPED: {e}");
            None
        }
    }
}

/// A plate with a hole through it, on the fake kernel — so this file needs no
/// OCCT. The fake tessellates a bounding box, which is a weak answer about
/// geometry and a complete one about *upload, draw and readback*, which is
/// what is under test.
fn document() -> Document<FakeKernel> {
    let mut doc = Document::new(FakeKernel::default());
    let plate = doc.add_box("plate", Vec3::new(40.0, 40.0, 10.0)).unwrap();
    let drill = doc.add_cylinder("drill", 6.0, 20.0).unwrap();
    doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .unwrap();
    doc.boolean(BooleanOp::Difference, plate, drill).unwrap();
    doc
}

fn read_back(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let row = (W * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        let at = (y * row) as usize;
        out.extend_from_slice(&data[at..at + (W * 4) as usize]);
    }
    drop(data);
    buffer.unmap();
    out
}

#[test]
fn what_the_adapter_can_do_is_reported_not_assumed() {
    let Some(h) = harness() else { return };
    let caps = &h.gpu.capabilities;

    // Not an assertion about *this* machine — the point is that the answer is
    // read from the adapter. What is asserted is the invariant the loader
    // depends on: no compute means a degradation message exists to show.
    assert_eq!(caps.compute, caps.degradation().is_none());
    assert!(caps.max_buffer_size > 0);
    assert!(caps.max_texture_dimension_2d >= 2048);
    assert!(!caps.adapter.is_empty());
}

#[test]
fn a_document_reaches_the_framebuffer() {
    let Some(mut h) = harness() else { return };
    let mut doc = document();

    let (color, depth) = w3d_render::offscreen_targets(&h.gpu.device, W, H);
    let (color_view, depth_view) = (
        color.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );

    let ids: Vec<_> = doc.nodes().map(|(id, _)| id).collect();
    let mut meshes = Vec::new();
    for id in &ids {
        let mesh = doc.mesh(*id).unwrap().clone();
        meshes.push(
            GpuMesh::upload(
                &h.gpu.device,
                h.gpu.capabilities.max_buffer_size,
                "body",
                &mesh,
            )
            .unwrap(),
        );
    }
    // Both backends tessellate face by face, so the indexed path is the one
    // that should be taken. If this ever flips, the mesh got three times
    // bigger and somebody should know why.
    assert!(meshes.iter().all(|m| !m.deindexed));
    assert!(meshes.iter().all(|m| m.triangles > 0));

    let objects: Vec<_> = meshes
        .iter()
        .zip(&ids)
        .map(|(mesh, id)| Object {
            mesh,
            id: id.index(),
            material: Material::default(),
        })
        .collect();

    let mut camera = Camera::default();
    camera.fit(&doc.visible_bounds());

    let vp = viewport(&h.gpu);
    h.renderer
        .draw(&vp, &color_view, &depth_view, &camera, &objects);

    let pixels = read_back(&h.gpu, &color);
    // The background is *sampled*, not asserted: the clear colour is linear
    // and the target is `Rgba8UnormSrgb`, so what lands in memory is the sRGB
    // encoding of it — 0.055 linear reads back as 65, not 14. A hard-coded
    // expectation here was the first thing this test got wrong.
    let background = [pixels[0], pixels[1], pixels[2]];
    let lit = pixels
        .chunks_exact(4)
        .filter(|p| {
            p[0].abs_diff(background[0]) > 6
                || p[1].abs_diff(background[1]) > 6
                || p[2].abs_diff(background[2]) > 6
        })
        .count();

    // A solid framed by `fit` covers a good part of the viewport. The loose
    // bound is deliberate: this asserts that something was drawn and shaded,
    // not that a particular rasteriser produced a particular picture.
    let total = (W * H) as usize;
    assert!(
        lit > total / 20,
        "only {lit} of {total} pixels differ from the background — nothing was drawn"
    );
    assert!(
        lit < total,
        "every pixel differs from the background — the clear did not happen"
    );
    assert!(
        background.iter().all(|&c| c > 40 && c < 90),
        "the sampled background {background:?} is not the cleared colour"
    );
}

#[test]
fn a_click_in_the_middle_names_the_body_under_it() {
    let Some(mut h) = harness() else { return };
    let mut doc = document();
    let (id, _) = doc.nodes().next().unwrap();
    let mesh = doc.mesh(id).unwrap().clone();
    let gpu_mesh = GpuMesh::upload(
        &h.gpu.device,
        h.gpu.capabilities.max_buffer_size,
        "body",
        &mesh,
    )
    .unwrap();
    let objects = [Object {
        mesh: &gpu_mesh,
        id: 7,
        material: Material::default(),
    }];

    let mut camera = Camera::default();
    camera.fit(&doc.visible_bounds());

    let vp = viewport(&h.gpu);
    let hit = h.renderer.pick(&vp, &camera, W / 2, H / 2, &objects);
    let (object, face) = hit
        .hit()
        .expect("the centre of a framed solid is the solid");
    assert_eq!(object, 7, "the caller's own id comes back unchanged");
    assert!(
        face < mesh.face_of_triangle.iter().copied().max().unwrap() + 1,
        "face {face} is not one of this mesh's faces"
    );

    // A corner of the viewport is background, and background is a miss rather
    // than object 0 — which is why `NOTHING` is `u32::MAX` and not zero.
    let miss = h.renderer.pick(&vp, &camera, 1, 1, &objects);
    assert_eq!(miss, w3d_render::Pick::MISS);
    assert!(miss.hit().is_none());
}

#[test]
fn picking_outside_the_viewport_is_a_miss_not_a_panic() {
    let Some(mut h) = harness() else { return };
    let camera = Camera::default();
    let vp = viewport(&h.gpu);
    let hit = h.renderer.pick(&vp, &camera, W, 0, &[]);
    assert_eq!(hit, w3d_render::Pick::MISS);
}
