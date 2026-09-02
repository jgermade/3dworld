//! The browser half of the loader.
//!
//! JS decides *which* variant to instantiate — that is `loader.js`, and it is
//! the part that probes threads and cross-origin isolation. This crate is what
//! the chosen variant runs: a canvas becomes a wgpu surface, the adapter is
//! asked what it can do, and the answer goes back to JS so the page can say so
//! out loud instead of degrading quietly.
//!
//! It is deliberately not the modeller. The scene is a fixed document on
//! `TruckKernel`, so the triangles on the screen are a real tessellation of a
//! real B-rep — but a *fixed* one, and `app/` is what replaces it. The surface,
//! the format negotiation and the pick loop stay.
//!
//! # The two variants
//!
//! This crate is built twice. Without `threads` it is a plain `wasm32` module.
//! With it, the module has a shared memory and atomic instructions, exports
//! `initThreadPool`, and tessellates a solid's faces across a rayon pool.
//! They are different artifacts in different directories, because a module
//! compiled for atomics will not instantiate on an engine without them and a
//! page that is not cross-origin isolated cannot give it a shared memory.
//! `loader.js` picks; `report().threads` says which one is actually running,
//! as a number, because "threaded" claimed by the file it came from is not
//! evidence that a pool ever started.

// Everything below needs a browser. On the host this crate is empty, which is
// what lets it be a default workspace member without `make test` growing a
// toolchain requirement.
#![cfg(target_arch = "wasm32")]

use js_sys::{Object, Reflect};
use w3d_core::Document;
use w3d_core::kernel::{BooleanOp, Mat4, Vec3};
use w3d_kernel_truck::TruckKernel;
use w3d_render::{Camera, Gpu, GpuMesh, Material, Object as DrawObject, PickPending, Renderer};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// The pool, and JS has to start it: a wasm module cannot spawn its own
/// workers, so `initThreadPool(n)` is called from `loader.js` after the module
/// is instantiated and before anything asks rayon to do work.
///
/// Re-exported rather than wrapped. A wrapper would have to reproduce the
/// `#[wasm_bindgen]` signature, and getting it subtly wrong is a promise that
/// resolves before the workers are up.
#[cfg(feature = "threads")]
pub use wasm_bindgen_rayon::init_thread_pool;

/// How many threads rayon actually has, which is 1 when there is no pool.
///
/// A number rather than a boolean, and read from rayon rather than from the
/// variant that was loaded. A threaded module whose `initThreadPool` was never
/// awaited reports 1 here and would report "threaded" anywhere else — that gap
/// is the whole reason this is not a `cfg!`.
fn thread_count() -> u32 {
    #[cfg(feature = "threads")]
    {
        rayon::current_num_threads() as u32
    }
    #[cfg(not(feature = "threads"))]
    {
        1
    }
}

/// One node's mesh on the GPU, and the id the pick will answer with.
struct Body {
    mesh: GpuMesh,
    id: u32,
}

#[wasm_bindgen]
pub struct Viewer {
    gpu: Gpu,
    renderer: Renderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::Texture,
    bodies: Vec<Body>,
    camera: Camera,
    pending: Option<PickPending>,
    selected: Option<u32>,
    /// Wall-clock milliseconds spent tessellating the scene at startup.
    ///
    /// The only number in this repository measured on the platform it is a
    /// claim about. It is one scene on one machine and it is not a benchmark —
    /// but a threaded build that reports the same figure as a single-threaded
    /// one is a pool that is not being used, and nothing else here would say so.
    tessellate_ms: f64,
}

/// Opens a device against `canvas` and builds the scene.
///
/// Fails rather than panics when there is no adapter: on the web that is a
/// message a user has to see — "this browser has neither WebGPU nor WebGL2" —
/// and a panic in wasm is a blank page and a stack trace in a console nobody
/// has open.
/// `force_webgl` skips WebGPU entirely. The loader passes it on a second
/// attempt, after a first one produced a canvas of one flat colour — see
/// [`w3d_render::Gpu::instance_for`] for why that check has to exist at all.
#[wasm_bindgen]
pub async fn start(canvas: HtmlCanvasElement, force_webgl: bool) -> Result<Viewer, JsError> {
    let (width, height) = (canvas.width().max(1), canvas.height().max(1));

    let backends = if force_webgl {
        wgpu::Backends::GL
    } else {
        wgpu::Backends::all()
    };
    let instance = Gpu::instance_for(backends).await;
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| JsError::new(&format!("no surface for this canvas: {e}")))?;
    let gpu = Gpu::open(&instance, Some(&surface))
        .await
        .map_err(|e| JsError::new(&e.to_string()))?;

    // The compositor's choice, not ours. Taking `formats[0]` is taking what the
    // surface says it prefers; hard-coding one is how a page works in Chrome
    // and is a validation error in Safari.
    let caps = surface.get_capabilities(&gpu.adapter);
    let format = caps
        .formats
        .first()
        .copied()
        .ok_or_else(|| JsError::new("the surface supports no format at all"))?;

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        color_space: wgpu::SurfaceColorSpace::Auto,
        view_formats: vec![],
    };
    surface.configure(&gpu.device, &config);

    let renderer = Renderer::new(&gpu.device, format);
    let depth = depth_texture(&gpu.device, width, height);

    let mut doc = scene();
    let ids: Vec<_> = doc.nodes().map(|(id, _)| id).collect();

    // Tessellation is timed on its own, with the GPU upload outside the clock:
    // the upload is driver work and would drown the thing being measured.
    let started = js_sys::Date::now();
    let mut meshes = Vec::with_capacity(ids.len());
    for id in &ids {
        meshes.push(
            doc.mesh(*id)
                .map_err(|e| JsError::new(&e.to_string()))?
                .clone(),
        );
    }
    let tessellate_ms = js_sys::Date::now() - started;

    let mut bodies = Vec::with_capacity(ids.len());
    for (id, mesh) in ids.iter().zip(&meshes) {
        let mesh = GpuMesh::upload(&gpu.device, gpu.capabilities.max_buffer_size, "body", mesh)
            .map_err(|e| JsError::new(&e.to_string()))?;
        bodies.push(Body {
            mesh,
            id: id.index(),
        });
    }

    let mut camera = Camera::default();
    camera.fit(&doc.visible_bounds());

    Ok(Viewer {
        gpu,
        renderer,
        surface,
        config,
        depth,
        bodies,
        camera,
        pending: None,
        selected: None,
        tessellate_ms,
    })
}

#[wasm_bindgen]
impl Viewer {
    /// What the adapter turned out to be, for the page to display.
    ///
    /// `degradation` is a string or null, and a loader that ignores it is a
    /// loader that lets WebGL2 look like WebGPU until somebody profiles it.
    pub fn report(&self) -> Result<Object, JsError> {
        let caps = &self.gpu.capabilities;
        let out = Object::new();
        set(&out, "backend", &format!("{}", caps.backend).into())?;
        set(&out, "adapter", &caps.adapter.as_str().into())?;
        set(&out, "compute", &caps.compute.into())?;
        set(
            &out,
            "vertexStorageBuffers",
            &caps.vertex_storage_buffers.into(),
        )?;
        set(&out, "maxBufferSize", &(caps.max_buffer_size as f64).into())?;
        set(
            &out,
            "degradation",
            &match caps.degradation() {
                Some(text) => text.into(),
                None => JsValue::NULL,
            },
        )?;
        set(&out, "triangles", &self.triangles().into())?;
        // Read from rayon, not from which file was loaded — see `thread_count`.
        set(&out, "threads", &thread_count().into())?;
        set(&out, "tessellateMs", &self.tessellate_ms.into())?;
        set(
            &out,
            "deindexed",
            &self.bodies.iter().any(|b| b.mesh.deindexed).into(),
        )?;
        Ok(out)
    }

    pub fn triangles(&self) -> u32 {
        self.bodies.iter().map(|b| b.mesh.triangles).sum()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.depth = depth_texture(&self.gpu.device, width, height);
    }

    pub fn orbit(&mut self, d_yaw: f64, d_pitch: f64) {
        self.camera.orbit(d_yaw, d_pitch);
    }

    pub fn dolly(&mut self, factor: f64) {
        self.camera.dolly(factor);
    }

    /// One frame. Returns false when the surface needs reconfiguring, which is
    /// the browser's way of saying the canvas changed size behind our back.
    pub fn render(&mut self) -> bool {
        use wgpu::CurrentSurfaceTexture as Got;
        // `Suboptimal` is still a frame and is drawn: refusing it makes a
        // resize flash instead of showing one slightly stale frame.
        let frame = match self.surface.get_current_texture() {
            Got::Success(frame) | Got::Suboptimal(frame) => frame,
            _ => {
                self.surface.configure(&self.gpu.device, &self.config);
                return false;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let depth = self.depth.create_view(&Default::default());
        // Borrowed field by field rather than through `&self`, so that the
        // renderer can be borrowed mutably alongside the bodies it draws.
        let vp = viewport(&self.gpu, &self.config);
        let objects = objects(&self.bodies, self.selected);
        self.renderer
            .draw(&vp, &view, &depth, &self.camera, &objects);
        drop(objects);
        self.gpu.queue.present(frame);
        true
    }

    /// Submits a pick. The answer arrives later — see `pickCollect`.
    ///
    /// There is no synchronous version on the web and that is not a limitation
    /// of this crate: a GPU readback completes on the JS event loop, so a
    /// blocking wait is a wait that can never finish.
    #[wasm_bindgen(js_name = pickBegin)]
    pub fn pick_begin(&mut self, x: u32, y: u32) {
        let vp = viewport(&self.gpu, &self.config);
        let objects = objects(&self.bodies, self.selected);
        self.pending = Some(self.renderer.pick_begin(&vp, &self.camera, x, y, &objects));
    }

    /// `null` while the readback is in flight, then `{object, face}` — with
    /// `object` null for a click on the background.
    #[wasm_bindgen(js_name = pickCollect)]
    pub fn pick_collect(&mut self) -> Result<JsValue, JsError> {
        let Some(pending) = &self.pending else {
            return Ok(JsValue::NULL);
        };
        let Some(pick) = pending.collect(&self.gpu.device) else {
            return Ok(JsValue::NULL);
        };
        self.pending = None;

        let out = Object::new();
        match pick.hit() {
            Some((object, face)) => {
                self.selected = Some(object);
                set(&out, "object", &object.into())?;
                set(&out, "face", &face.into())?;
            }
            None => {
                self.selected = None;
                set(&out, "object", &JsValue::NULL)?;
                set(&out, "face", &JsValue::NULL)?;
            }
        }
        Ok(out.into())
    }
}

/// Free functions rather than methods, and the borrow checker is the reason:
/// a method takes `&self`, which then collides with the `&mut self.renderer`
/// every draw needs. Borrowing the fields separately is the whole fix.
fn viewport<'a>(gpu: &'a Gpu, config: &wgpu::SurfaceConfiguration) -> w3d_render::Viewport<'a> {
    w3d_render::Viewport {
        device: &gpu.device,
        queue: &gpu.queue,
        width: config.width,
        height: config.height,
    }
}

fn objects(bodies: &[Body], selected: Option<u32>) -> Vec<DrawObject<'_>> {
    bodies
        .iter()
        .map(|b| DrawObject {
            mesh: &b.mesh,
            id: b.id,
            material: Material {
                selected: selected == Some(b.id),
                ..Material::default()
            },
        })
        .collect()
}

fn depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("w3d depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: w3d_render::DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

/// A plate with a hole, on the fake kernel — the same document the headless
/// tests draw, so that a difference between here and there is a difference in
/// the browser rather than in the scene.
/// Pure Rust B-rep geometry scene rendered via TruckKernel in WebAssembly.
fn scene() -> Document<TruckKernel> {
    let mut doc = Document::new(TruckKernel::default());
    let plate = doc
        .add_box("plate", Vec3::new(40.0, 40.0, 10.0))
        .expect("box");
    let drill = doc.add_cylinder("drill", 6.0, 20.0).expect("cylinder");
    let _ = doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)));
    let _ = doc.boolean(BooleanOp::Difference, plate, drill);
    doc
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsError> {
    Reflect::set(target, &key.into(), value)
        .map(|_| ())
        .map_err(|_| JsError::new("could not build the report object"))
}
