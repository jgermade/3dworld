//! The viewport.
//!
//! `wgpu` over WebGPU, Vulkan, Metal and DX12, with WebGL2 as the fallback
//! that the loader may land on. Three things in here are decisions rather than
//! plumbing, and each is documented where it lives:
//!
//! - **What the adapter can do is read, not assumed** ([`gpu::Capabilities`]).
//!   WebGL2 has no compute shaders, so everything this project has planned for
//!   the GPU beyond drawing — culling, BVH build, silhouettes — degrades to
//!   the CPU there. That is a fact to announce, not to discover.
//! - **Model space is `f64` and narrows once** ([`camera`]), at the matrix
//!   handed to the queue.
//! - **A click is answered by rendering ids, not by intersecting rays**
//!   ([`Renderer::pick`]). It shares its vertex stage with the picture, which
//!   is what makes it agree with the picture at a silhouette.
//!
//! This crate knows nothing about the document. Objects carry a caller-chosen
//! `u32` id and the caller maps it back — so `w3d-core` stays a dev-dependency
//! here, and the viewport can draw something that is not a document node.

pub mod camera;
pub mod gpu;
pub mod grid;
pub mod scene;

pub use camera::Camera;
pub use gpu::{Capabilities, Gpu, GpuError};
pub use grid::Grid;
pub use scene::{GpuMesh, MeshError};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What [`offscreen_targets`] makes, and what a test renders into. A surface
/// picks its own — see [`Renderer::new`].
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Two `u32`: the object, and the face within it.
pub const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;

/// What a pick returns when the ray missed everything.
///
/// `u32::MAX` rather than 0, so that a caller's id of 0 is a legal object.
/// The alternative — reserving 0 — pushes an off-by-one into every caller.
pub const NOTHING: u32 = u32::MAX;

/// One drawable body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub color: [f32; 4],
    pub selected: bool,
    pub selected_face: Option<u32>,
    pub hovered_face: Option<u32>,
    pub hovered_edge: bool,
    pub selected_edge: bool,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            color: [0.72, 0.74, 0.78, 1.0],
            selected: false,
            selected_face: None,
            hovered_face: None,
            hovered_edge: false,
            selected_edge: false,
        }
    }
}

/// A device, its queue, and the size being drawn into.
///
/// Bundled rather than passed loose, and one bug class is why: the aspect
/// ratio used to be an argument next to the target textures, so a caller could
/// hand in an aspect that disagreed with the size it was rendering — which
/// looks like a wrong camera and is a wrong call site.
#[derive(Clone, Copy)]
pub struct Viewport<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub width: u32,
    pub height: u32,
}

impl Viewport<'_> {
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        f64::from(self.width) / f64::from(self.height)
    }
}

pub struct Object<'a> {
    pub mesh: &'a GpuMesh,
    /// The caller's own identity for this thing. Comes back out of
    /// [`Renderer::pick`] unchanged.
    pub id: u32,
    pub material: Material,
}

/// What was under a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub object: u32,
    pub face: u32,
}

impl Pick {
    pub const MISS: Self = Self {
        object: NOTHING,
        face: NOTHING,
    };

    pub fn hit(self) -> Option<(u32, u32)> {
        (self.object != NOTHING).then_some((self.object, self.face))
    }
}

/// A submitted pick, waiting on the device.
///
/// Carries no lifetime, so it can sit in an application's state between
/// frames — which is the whole point of it existing.
pub struct PickPending {
    /// `None` for a pick outside the viewport, which is a miss decided without
    /// touching the GPU.
    readback: Option<wgpu::Buffer>,
    ready: Arc<AtomicBool>,
}

impl PickPending {
    /// `None` while the readback is still in flight.
    ///
    /// Call it once a frame. On native it nudges the device; in a browser the
    /// map callback arrives on the event loop and this only reads the flag.
    /// Calling it after it has answered once returns `None` — the buffer is
    /// consumed, and a receipt is good for one answer.
    pub fn collect(&self, device: &wgpu::Device) -> Option<Pick> {
        let Some(readback) = &self.readback else {
            // Nothing was submitted, so there is nothing to wait for.
            return self
                .ready
                .swap(false, Ordering::Acquire)
                .then_some(Pick::MISS);
        };
        let _ = device.poll(wgpu::PollType::Poll);
        if !self.ready.swap(false, Ordering::Acquire) {
            return None;
        }
        let slice = readback.slice(..);
        let Ok(data) = slice.get_mapped_range() else {
            return Some(Pick::MISS);
        };
        let read =
            |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let pick = Pick {
            object: read(0),
            face: read(4),
        };
        drop(data);
        readback.unmap();
        Some(pick)
    }
}

/// Globals: a 4x4 and an eye position, padded to what `uniform` requires.
const GLOBALS_SIZE: u64 = 80;
/// `vec4` colour plus eight `u32`.
const OBJECT_SIZE: u64 = 48;

pub struct Renderer {
    shade: wgpu::RenderPipeline,
    pick: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    grid: Grid,
    pub show_grid: bool,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    /// One buffer for every object in a frame, addressed by dynamic offset.
    /// Grown, never shrunk; a viewport's object count is stable.
    objects: wgpu::Buffer,
    objects_group: wgpu::BindGroup,
    object_stride: u64,
    object_capacity: u64,
    /// The alignment the adapter demands between two dynamic offsets. Read
    /// from the device rather than assumed to be 256, which is the usual value
    /// and not a guarantee.
    alignment: u64,
}

impl Renderer {
    /// `color_format` is the format of whatever will be drawn into, and it is
    /// an argument because it is not ours to choose: a canvas or a window hands
    /// back the format its compositor wants — usually `Bgra8Unorm` — and a
    /// pipeline built for a different one is a validation error at the first
    /// frame, not at construction.
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("w3d shade"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shade.wgsl").into()),
        });

        let globals_layout = uniform_layout(device, "w3d globals", GLOBALS_SIZE, false);
        let object_layout = uniform_layout(device, "w3d object", OBJECT_SIZE, true);

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("w3d globals"),
            size: GLOBALS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("w3d globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });

        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let object_stride = alignment.max(OBJECT_SIZE);
        let object_capacity = 64;
        let objects = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("w3d objects"),
            size: object_stride * object_capacity,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let objects_group = object_bind_group(device, &object_layout, &objects);

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("w3d"),
            bind_group_layouts: &[Some(&globals_layout), Some(&object_layout)],
            immediate_size: 0,
        });

        let shade = pipeline(device, &layout, &shader, "fs_shade", color_format);
        let pick = pipeline(device, &layout, &shader, "fs_pick", ID_FORMAT);
        let lines = line_pipeline(device, &layout, &shader, color_format);
        let grid = Grid::new(device, &globals_layout, color_format);

        Self {
            shade,
            pick,
            lines,
            grid,
            show_grid: true,
            globals,
            globals_group,
            object_layout,
            objects,
            objects_group,
            object_stride,
            object_capacity,
            alignment,
        }
    }

    /// Draws into `color` with `depth`, both owned by the caller — a surface
    /// texture on the desktop and in the browser, an offscreen texture in a
    /// test or a thumbnail.
    pub fn draw(
        &mut self,
        vp: &Viewport<'_>,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        camera: &Camera,
        objects: &[Object<'_>],
    ) {
        self.write_globals(vp.queue, camera, vp.aspect());
        self.write_objects(vp.device, vp.queue, objects);

        let mut encoder = vp
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("w3d") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("w3d shade"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.055,
                            g: 0.059,
                            b: 0.067,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(depth_attachment(depth)),
                ..Default::default()
            });
            self.draw_into(&mut pass, objects);
        }
        vp.queue.submit([encoder.finish()]);
    }

    /// Writes this frame's uniforms. Call before [`Renderer::draw_into`].
    ///
    /// Split out because the chrome shares the scene's render pass rather than
    /// compositing over it — that is the whole reason `egui` was chosen over a
    /// DOM framework, and it means somebody else owns the pass. Uniforms must
    /// be written before that pass begins, because a render pass borrows the
    /// encoder and nothing may touch the queue while it is open.
    pub fn prepare(&mut self, vp: &Viewport<'_>, camera: &Camera, objects: &[Object<'_>]) {
        self.write_globals(vp.queue, camera, vp.aspect());
        self.write_objects(vp.device, vp.queue, objects);
    }

    /// Records the scene into a pass the caller opened.
    ///
    /// The caller is responsible for the colour and depth attachments, and for
    /// having called [`Renderer::prepare`] with the same objects.
    pub fn draw_into(&self, pass: &mut wgpu::RenderPass<'_>, objects: &[Object<'_>]) {
        if self.show_grid {
            self.grid.draw(pass, &self.globals_group);
        }
        pass.set_pipeline(&self.shade);
        self.record(pass, objects);
        pass.set_pipeline(&self.lines);
        self.record_lines(pass, objects);
    }

    /// What is under one pixel, by rendering ids into it.
    ///
    /// The whole framebuffer is set up and the scissor is one pixel wide, so
    /// rasterisation is bit-for-bit what the picture did — same vertex stage,
    /// same projection, same depth test — and only the wanted texel is
    /// written. That is what makes a pick at a silhouette agree with what the
    /// user can see.
    ///
    /// It is a full render for one pixel, which is the cost of exactness. A
    /// pick matrix that shrinks the frustum to the pixel would rasterise less
    /// and is the obvious optimisation; it is not written, because a click is
    /// not a hot path and a wrong pick is much worse than a slow one.
    ///
    /// **Blocking**, and only honestly so on native: it waits on the device.
    /// A browser has no such wait — WebGPU's map completion arrives on the JS
    /// event loop, so a blocking poll there returns without the buffer being
    /// mapped and this method answers `MISS` for every click. Anything with an
    /// event loop wants [`Renderer::pick_begin`] and [`PickPending::collect`].
    pub fn pick(
        &mut self,
        vp: &Viewport<'_>,
        camera: &Camera,
        x: u32,
        y: u32,
        objects: &[Object<'_>],
    ) -> Pick {
        let pending = self.pick_begin(vp, camera, x, y, objects);
        let _ = vp.device.poll(wgpu::PollType::wait_indefinitely());
        pending.collect(vp.device).unwrap_or(Pick::MISS)
    }

    /// Submits the pick and hands back a receipt.
    ///
    /// Split from [`Renderer::pick`] because a readback is not something a
    /// browser can be made to do synchronously, and pretending otherwise
    /// produces a modeller where nothing is ever selected. A caller polls
    /// [`PickPending::collect`] on each frame; it is one atomic load until the
    /// answer is there.
    pub fn pick_begin(
        &mut self,
        vp: &Viewport<'_>,
        camera: &Camera,
        x: u32,
        y: u32,
        objects: &[Object<'_>],
    ) -> PickPending {
        let (width, height) = (vp.width, vp.height);
        if x >= width || y >= height || width == 0 || height == 0 {
            return PickPending {
                readback: None,
                ready: Arc::new(AtomicBool::new(true)),
            };
        }
        let device = vp.device;
        self.write_globals(vp.queue, camera, vp.aspect());
        self.write_objects(device, vp.queue, objects);

        let ids = texture(device, "w3d ids", width, height, ID_FORMAT, true);
        let depth = texture(device, "w3d pick depth", width, height, DEPTH_FORMAT, false);
        let ids_view = ids.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());

        // 8 bytes of payload in a row that must be 256-byte aligned. The
        // padding is the API's, not ours.
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("w3d pick"),
            size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("w3d pick"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("w3d pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &ids_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The miss value. Cleared over the whole attachment,
                        // not just the scissor, which is what makes a pick
                        // that hits nothing return `NOTHING` rather than
                        // whatever the last pick left there.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(NOTHING),
                            g: f64::from(NOTHING),
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                ..Default::default()
            });
            pass.set_scissor_rect(x, y, 1, 1);
            pass.set_pipeline(&self.pick);
            self.record(&mut pass, objects);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &ids,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        vp.queue.submit([encoder.finish()]);

        let ready = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ready);
        readback.slice(..).map_async(wgpu::MapMode::Read, move |_| {
            // The result is discarded and the flag set either way: a failed
            // map is a miss, and `collect` reads a `None` range as one.
            flag.store(true, Ordering::Release);
        });

        PickPending {
            readback: Some(readback),
            ready,
        }
    }

    fn record(&self, pass: &mut wgpu::RenderPass<'_>, objects: &[Object<'_>]) {
        pass.set_bind_group(0, &self.globals_group, &[]);
        for (i, object) in objects.iter().enumerate() {
            let offset = (i as u64 * self.object_stride) as u32;
            pass.set_bind_group(1, &self.objects_group, &[offset]);
            object.mesh.draw(pass);
        }
    }

    fn record_lines(&self, pass: &mut wgpu::RenderPass<'_>, objects: &[Object<'_>]) {
        pass.set_bind_group(0, &self.globals_group, &[]);
        for (i, object) in objects.iter().enumerate() {
            let offset = (i as u64 * self.object_stride) as u32;
            pass.set_bind_group(1, &self.objects_group, &[offset]);
            object.mesh.draw_lines(pass);
        }
    }

    fn write_globals(&self, queue: &wgpu::Queue, camera: &Camera, aspect: f64) {
        let mut bytes = Vec::with_capacity(GLOBALS_SIZE as usize);
        for col in camera.view_projection(aspect) {
            for c in col {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        let eye = camera.eye();
        for c in eye.to_f32() {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&0f32.to_le_bytes());
        queue.write_buffer(&self.globals, 0, &bytes);
    }

    fn write_objects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        objects: &[Object<'_>],
    ) {
        self.reserve(device, objects.len() as u64);
        let mut bytes = vec![0u8; (self.object_stride * objects.len().max(1) as u64) as usize];
        for (i, object) in objects.iter().enumerate() {
            let at = (i as u64 * self.object_stride) as usize;
            let mut w = at;
            for c in object.material.color {
                bytes[w..w + 4].copy_from_slice(&c.to_le_bytes());
                w += 4;
            }
            bytes[w..w + 4].copy_from_slice(&object.id.to_le_bytes());
            w += 4;
            bytes[w..w + 4].copy_from_slice(&u32::from(object.material.selected).to_le_bytes());
            w += 4;
            let sel_face = object.material.selected_face.unwrap_or(u32::MAX);
            bytes[w..w + 4].copy_from_slice(&sel_face.to_le_bytes());
            w += 4;
            let hov_face = object.material.hovered_face.unwrap_or(u32::MAX);
            bytes[w..w + 4].copy_from_slice(&hov_face.to_le_bytes());
            w += 4;
            bytes[w..w + 4].copy_from_slice(&u32::from(object.material.hovered_edge).to_le_bytes());
            w += 4;
            bytes[w..w + 4]
                .copy_from_slice(&u32::from(object.material.selected_edge).to_le_bytes());
            w += 4;
            bytes[w..w + 4].copy_from_slice(&0u32.to_le_bytes()); // _pad0
        }
        if !objects.is_empty() {
            queue.write_buffer(&self.objects, 0, &bytes);
        }
    }

    fn reserve(&mut self, device: &wgpu::Device, count: u64) {
        if count <= self.object_capacity {
            return;
        }
        self.object_capacity = count.next_power_of_two();
        self.objects = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("w3d objects"),
            size: self.object_stride * self.object_capacity,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.objects_group = object_bind_group(device, &self.object_layout, &self.objects);
    }

    /// What one object costs in the uniform buffer. Reported because it is the
    /// adapter's alignment rather than ours, and it is 256 on most hardware
    /// for 32 bytes of payload.
    pub fn object_stride(&self) -> u64 {
        self.object_stride
    }

    pub fn uniform_alignment(&self) -> u64 {
        self.alignment
    }
}

/// A colour and depth pair sized for a viewport. Offscreen; a surface provides
/// its own colour texture and only needs the depth.
pub fn offscreen_targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::Texture) {
    (
        texture(device, "w3d color", width, height, COLOR_FORMAT, true),
        texture(device, "w3d depth", width, height, DEPTH_FORMAT, false),
    )
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    copyable: bool,
) -> wgpu::Texture {
    let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
    if copyable {
        usage |= wgpu::TextureUsages::COPY_SRC;
    }
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn depth_attachment(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

fn uniform_layout(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    dynamic: bool,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        }],
    })
}

fn object_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("w3d objects"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: wgpu::BufferSize::new(OBJECT_SIZE),
            }),
        }],
    })
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fragment: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(scene::VERTEX_LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Nothing is culled, and that is a decision. A solid whose normals
            // are inverted is a defect worth *seeing*; culling it away shows a
            // hole and blames the boolean instead.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn line_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("w3d lines"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_line"),
            compilation_options: Default::default(),
            buffers: &[Some(scene::LINE_VERTEX_LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_line"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
