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

use js_sys::{Array, Object, Reflect, Uint8Array};
use w3d_core::Document;
use w3d_core::kernel::{Aabb, BooleanOp, Mat4, Vec3};
use w3d_kernel_truck::TruckKernel;
use w3d_render::{Camera, Gpu, GpuMesh, Material, Object as DrawObject, PickPending, Renderer};
use w3d_wire::{Addressing, Message, merge, split_by_face};
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

/// Where the mesh on screen was made.
///
/// Reported out to the page, and it has to be: a boot path that quietly fell
/// back to meshing on the main thread draws exactly the same picture as one
/// that did not, in exactly the same colours, with the same triangle count.
/// The only difference is that the thread which draws was blocked for the
/// whole of it — which is invisible in every check this repository has, and is
/// the entire point of the worker. See `report`.
#[derive(Clone, Copy, PartialEq)]
enum MeshedBy {
    /// `start` has returned and nothing has been installed yet. A viewer in
    /// this state draws an empty scene, which is a flat canvas — see the note
    /// in `loader.js` about why the graphics fallback cannot be asked to judge
    /// one.
    Nothing,
    Worker,
    MainThread,
}

impl MeshedBy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Nothing => "nothing yet",
            Self::Worker => "worker",
            Self::MainThread => "main thread",
        }
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
    /// Whether the scene's boolean succeeded, so the page can say whether what
    /// it draws was modelled or merely asked for. See `scene`.
    ///
    /// `None` where the source could not say — a document loaded from a file
    /// is a solid, and nothing in it records whether a boolean made it. That
    /// is reported as `null` rather than flattened to `false`, because the two
    /// are different sentences: "the cut did not happen" and "nobody here
    /// watched it happen".
    modelled: Option<bool>,
    /// Which side of the worker boundary produced what is on screen.
    meshed_by: MeshedBy,
    /// Where the *model* came from — a `.w3d` the page fetched, or the scene
    /// compiled into this module.
    ///
    /// Separate from `meshed_by` because they are independent questions, and
    /// conflating them would hide the interesting combination: a document that
    /// was meshed on the main thread because the worker would not start is a
    /// different failure from a worker that meshed the built-in scene because
    /// the document was missing. Both draw a plate with a hole in it.
    source: String,
}

/// The producer end of the worker boundary: tessellate the scene and hand back
/// the bytes, with no device anywhere in the call.
///
/// This is what runs **in a worker**, which is why it is a free function and
/// not a method on `Viewer`: a `Viewer` owns a wgpu surface, and a worker has
/// no canvas to put one on. Everything it touches — the document, the
/// tessellation, the split, the encoding — is `w3d-core`, `w3d-kernel-truck`
/// and `w3d-wire`, and none of those knows a GPU exists.
///
/// Returns one `Uint8Array` per chunk. JavaScript posts them with the buffers
/// in the transfer list, which is a *move*: the worker's heap gives them up.
/// See `web/worker.js`, and `WIRE.md` for what is in them.
///
/// `chunks_per_body` is a parameter because nothing knows what it should be.
/// The register says so too: no measurement anywhere in this repository says
/// what a good split is, and a constant here would be a guess with a number's
/// authority.
#[wasm_bindgen(js_name = tessellateScene)]
pub fn tessellate_scene(chunks_per_body: u32) -> Result<Object, JsError> {
    // Three phases rather than one wall-clock number, for the reason
    // `make measure` gives at the other end of this repository: modelling,
    // meshing and encoding fail differently, and a caller can only act on one
    // at a time. A single figure here would also be uncomparable with the
    // page's own `tessellateMs`, which covers the middle phase alone.
    let started = js_sys::Date::now();
    let (doc, modelled) = scene();
    let model_ms = js_sys::Date::now() - started;

    // `Some`, because this path *did* the boolean and watched it succeed or
    // decline. The document path below cannot say the same about a file.
    tessellate_into_chunks(doc, chunks_per_body, model_ms, Some(modelled))
}

/// The same, for a document that arrived as bytes instead of being built here.
///
/// **This is what makes the worker a modeller's boot path rather than a
/// demonstration.** `tessellate_scene` works only because the page's scene is
/// fixed and both sides can construct it; the moment a user opens a file, the
/// thread that draws holds bytes and the worker has to be *sent* them. That was
/// the whole of what register item 4 had left.
///
/// The bytes are a `.w3d` — the format this repository already specifies in
/// `FORMAT.md` — and not a second format invented for this boundary. That is
/// the point: a document crossing a worker boundary and a document crossing a
/// disk are the same problem, and `w3d-format` already solved it, zip and all,
/// with no dependency that does not build for wasm32. What crosses back is
/// still `WIRE.md` chunks, because a mesh and a document are not the same
/// thing and the asymmetry is real: bytes in are a model, bytes out are
/// triangles.
///
/// The `modelled` question has no answer here and is reported as one: a
/// document is a solid, and nothing in it records whether a boolean made it.
/// The file's own writer refuses to emit an uncut scene — see
/// `format/examples/scene_w3d.rs` — which is a guarantee about the file, not
/// something this function verified, and saying `true` here would be the page
/// vouching for a check it never ran.
#[wasm_bindgen(js_name = tessellateDocument)]
pub fn tessellate_document(bytes: &[u8], chunks_per_body: u32) -> Result<Object, JsError> {
    let started = js_sys::Date::now();
    let doc = w3d_format::load(TruckKernel::default(), bytes)
        .map_err(|e| JsError::new(&format!("the document would not open: {e}")))?;
    // Named `model_ms` for the phase it replaces, and it is not modelling: it
    // is a zip, a manifest and a BREP parse per body. On this scene it is the
    // 500 ms of boolean that is *no longer here*, which is the saving.
    let model_ms = js_sys::Date::now() - started;

    tessellate_into_chunks(doc, chunks_per_body, model_ms, None)
}

/// Mesh every body, split each into chunks, and report the phases.
///
/// Shared by both producers so that a document and the built-in scene cannot
/// drift into being encoded differently — which would make the comparison the
/// browser test draws between them meaningless.
fn tessellate_into_chunks(
    mut doc: Document<TruckKernel>,
    chunks_per_body: u32,
    model_ms: f64,
    modelled: Option<bool>,
) -> Result<Object, JsError> {
    let ids: Vec<_> = doc.nodes().map(|(id, _)| id).collect();
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

    let started = js_sys::Date::now();
    let out = Array::new();
    for (id, mesh) in ids.iter().zip(&meshes) {
        let chunks = split_by_face(
            mesh,
            chunks_per_body.max(1) as usize,
            Addressing::whole(id.index()),
        )
        .map_err(|e| JsError::new(&e.to_string()))?;
        for chunk in chunks {
            out.push(&Uint8Array::from(chunk.as_slice()));
        }
    }
    let encode_ms = js_sys::Date::now() - started;

    let result = Object::new();
    set(&result, "chunks", &out)?;
    set(&result, "bodies", &(ids.len() as u32).into())?;
    set(&result, "modelMs", &model_ms.into())?;
    set(&result, "tessellateMs", &tessellate_ms.into())?;
    set(&result, "encodeMs", &encode_ms.into())?;
    // Whether the boolean this scene asks for actually happened, or `null`
    // where the source cannot say. It has to cross with the bytes: the page
    // builds no document of its own any more, so a `modelled` left behind here
    // is a page that cannot tell whether what it draws was cut or merely
    // asked for.
    set(
        &result,
        "modelled",
        &match modelled {
            Some(yes) => yes.into(),
            None => JsValue::NULL,
        },
    )?;
    Ok(result)
}

/// Opens a device against `canvas`. **It does not build or mesh a scene.**
///
/// That changed on 2026-09-15, and it is what put the worker on the boot path:
/// this function used to model the document, tessellate every body and upload
/// the result before it returned, all of it on the thread that then had to
/// draw. Now it returns a viewer with no bodies at all, and the mesh arrives
/// through [`Viewer::install_wire`] from a worker that has been running since
/// before the adapter was asked for.
///
/// Two consequences worth knowing at the call site:
///
/// - **A viewer returned from here draws nothing**, so the loader's
///   "did WebGPU actually rasterise anything" check cannot be run against it
///   until a mesh is installed. `loader.js` orders those two for that reason.
/// - **The camera is not fitted here either**, because there is no document to
///   take bounds from. It is fitted when bodies arrive.
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

    Ok(Viewer {
        gpu,
        renderer,
        surface,
        config,
        depth,
        bodies: Vec::new(),
        camera: Camera::default(),
        pending: None,
        selected: None,
        tessellate_ms: 0.0,
        modelled: None,
        meshed_by: MeshedBy::Nothing,
        source: String::from("nothing yet"),
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
        set(
            &out,
            "acceleration",
            &format!("{}", caps.acceleration()).into(),
        )?;
        set(
            &out,
            "softwareRendering",
            &match caps.software_rendering() {
                Some(text) => text.into(),
                None => JsValue::NULL,
            },
        )?;
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
        // Which side of the worker boundary made what is on screen. A page
        // that fell back to meshing on the thread that draws looks *identical*
        // to one that did not — same triangles, same colours, same picture —
        // and the only observable difference is a stall nothing here can see.
        // So it is said out loud rather than assumed from the fact that a
        // worker was started.
        set(&out, "meshedBy", &self.meshed_by.as_str().into())?;
        set(&out, "source", &self.source.as_str().into())?;
        // Whether the plate on screen has a hole in it because a boolean cut
        // one, or is a plate and a pin sharing a space. See `scene`.
        set(
            &out,
            "modelled",
            &match self.modelled {
                Some(yes) => yes.into(),
                None => JsValue::NULL,
            },
        )?;
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

    /// The consumer end of the worker boundary, **and since 2026-09-15 the
    /// boot path**: install every body from chunks a worker produced.
    ///
    /// `chunks` is an array of `Uint8Array`, in whatever order they arrived —
    /// they are grouped by the node in their own headers and merged by chunk
    /// number, so the order this receives them in cannot change the result.
    /// See `WIRE.md`.
    ///
    /// **One copy happens here and it is not the transfer.** The transfer is
    /// free: `postMessage` moved the buffers rather than cloning them. What
    /// costs is getting the bytes from a JS `ArrayBuffer` into this module's
    /// linear memory, which `Uint8Array::to_vec` does — and which cannot be
    /// avoided without writing into an exported pointer, which needs `unsafe`,
    /// which this workspace forbids outside the FFI crate. It is one memcpy per
    /// chunk against a tessellation that took seconds.
    ///
    /// `facts` carries what the bytes cannot: `tessellateMs`, `modelled`,
    /// `fromWorker` and `source`. The first two have to be passed because this side no
    /// longer builds a document at startup, so there is nothing here that could
    /// measure the one or answer the other.
    ///
    /// **`fromWorker` is the caller's word and there is no way to check it**,
    /// which is worth stating rather than hiding: a `WIRE.md` message has no
    /// field for where it was made, deliberately — the format describes a mesh,
    /// not a provenance — so bytes tessellated on this thread and bytes posted
    /// from a worker are byte-for-byte indistinguishable on arrival. Only the
    /// caller knows which it has, and `report().meshedBy` is therefore exactly
    /// as trustworthy as `loader.js`. Missing or mistyped facts are an error
    /// rather than a default, because a `tessellateMs` that quietly became 0
    /// would read as a boot too fast to believe and nobody would believe it.
    ///
    /// **The camera is fitted here**, and from the arrived vertices, because
    /// `start` no longer has a document to take bounds from. Those are *mesh*
    /// bounds and they sit inside the B-rep's by the chordal error — a
    /// tessellated cylinder is fractionally narrower than the cylinder it
    /// approximates. For framing a view that is not a difference anyone can
    /// see; for anything that has to be exact it is the wrong number, which is
    /// why it is written down here rather than left to be discovered.
    ///
    /// Safe to call more than once with the same chunks, and the loader does
    /// exactly that: falling back from WebGPU to WebGL2 means a new canvas and
    /// a new device, and re-uploading bytes already in hand is much cheaper
    /// than tessellating the scene a second time — which is what the fallback
    /// used to cost.
    ///
    /// Returns how many bodies were installed.
    #[wasm_bindgen(js_name = installWire)]
    pub fn install_wire(&mut self, chunks: Array, facts: &Object) -> Result<u32, JsError> {
        let tessellate_ms = get(facts, "tessellateMs")?
            .as_f64()
            .ok_or_else(|| JsError::new("installWire: tessellateMs must be a number"))?;
        // `null` is a legal answer and means the source could not say — see
        // the field. Anything that is neither a boolean nor null is a caller
        // that has lost track of what it is installing.
        let modelled_js = get(facts, "modelled")?;
        let modelled =
            if modelled_js.is_null() || modelled_js.is_undefined() {
                None
            } else {
                Some(modelled_js.as_bool().ok_or_else(|| {
                    JsError::new("installWire: modelled must be a boolean or null")
                })?)
            };
        let from_worker = get(facts, "fromWorker")?
            .as_bool()
            .ok_or_else(|| JsError::new("installWire: fromWorker must be a boolean"))?;
        let source = get(facts, "source")?
            .as_string()
            .ok_or_else(|| JsError::new("installWire: source must be a string"))?;
        let arrived: Vec<Vec<u8>> = chunks
            .iter()
            .map(|v| {
                v.dyn_into::<Uint8Array>()
                    .map(|a| a.to_vec())
                    .map_err(|_| JsError::new("a chunk is not a Uint8Array"))
            })
            .collect::<Result<_, _>>()?;
        if arrived.is_empty() {
            return Err(JsError::new("no chunks arrived"));
        }

        // Grouped by node, in the order the nodes are first seen, so that the
        // bodies come out in a defined order rather than a hash map's.
        let mut nodes: Vec<u32> = Vec::new();
        let mut grouped: Vec<Vec<&[u8]>> = Vec::new();
        for bytes in &arrived {
            let node = Message::decode(bytes)
                .map_err(|e| JsError::new(&e.to_string()))?
                .addressing
                .node;
            match nodes.iter().position(|&n| n == node) {
                Some(at) => grouped[at].push(bytes),
                None => {
                    nodes.push(node);
                    grouped.push(vec![bytes]);
                }
            }
        }

        let mut bounds = Aabb::EMPTY;
        let mut bodies = Vec::with_capacity(grouped.len());
        for (node, chunks) in nodes.iter().zip(&grouped) {
            let merged = merge(chunks).map_err(|e| JsError::new(&e.to_string()))?;
            let message = Message::decode(&merged).map_err(|e| JsError::new(&e.to_string()))?;
            // Accumulated while the merged bytes are still alive, which is the
            // only window there is: `message` borrows `merged`.
            for v in message.vertices() {
                bounds.expand(position_of(v.position));
            }
            let mesh = GpuMesh::from_wire(
                &self.gpu.device,
                self.gpu.capabilities.max_buffer_size,
                // Not "from a worker": this path installs main-thread bytes
                // too, and a debug label that lies is worse than a vague one.
                "body from wire",
                &message,
            )
            .map_err(|e| JsError::new(&e.to_string()))?;
            bodies.push(Body { mesh, id: *node });
        }

        self.bodies = bodies;
        self.selected = None;
        self.pending = None;
        if !bounds.is_empty() {
            self.camera.fit(&bounds);
        }
        self.tessellate_ms = tessellate_ms;
        self.modelled = modelled;
        self.meshed_by = if from_worker {
            MeshedBy::Worker
        } else {
            MeshedBy::MainThread
        };
        self.source = source;
        Ok(self.bodies.len() as u32)
    }

    /// Mesh the scene on **this** thread — the fallback for when the worker
    /// could not be used at all.
    ///
    /// This is what `start` used to do, and it is deliberately still reachable.
    /// A module worker can fail for reasons that have nothing to do with the
    /// engine: a Content-Security-Policy without `worker-src`, a `worker.js`
    /// that did not get deployed, a file: URL, an engine out of memory. A
    /// modeller that shows *nothing* because a worker would not start is worse
    /// than one that stalls, so the slow path stays — and `report().meshedBy`
    /// says which of the two happened, so that it is visible rather than
    /// silent.
    ///
    /// Bounds are taken from the mesh here as well, rather than from the
    /// document that is right there, so that both paths frame the scene
    /// identically. A camera that depended on which of them ran would make
    /// every screenshot comparison between them meaningless.
    #[wasm_bindgen(js_name = tessellateHere)]
    pub fn tessellate_here(&mut self) -> Result<u32, JsError> {
        let (mut doc, modelled) = scene();
        let modelled = Some(modelled);
        let ids: Vec<_> = doc.nodes().map(|(id, _)| id).collect();

        // Timed with the upload outside the clock, for the reason the old
        // `start` gave: the upload is driver work and would drown the thing
        // being measured.
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

        let mut bounds = Aabb::EMPTY;
        let mut bodies = Vec::with_capacity(ids.len());
        for (id, mesh) in ids.iter().zip(&meshes) {
            for p in &mesh.positions {
                bounds.expand(position_of(*p));
            }
            let uploaded = GpuMesh::upload(
                &self.gpu.device,
                self.gpu.capabilities.max_buffer_size,
                "body",
                mesh,
            )
            .map_err(|e| JsError::new(&e.to_string()))?;
            bodies.push(Body {
                mesh: uploaded,
                id: id.index(),
            });
        }

        self.bodies = bodies;
        self.selected = None;
        self.pending = None;
        if !bounds.is_empty() {
            self.camera.fit(&bounds);
        }
        self.tessellate_ms = tessellate_ms;
        self.modelled = modelled;
        self.meshed_by = MeshedBy::MainThread;
        // This path builds the compiled-in scene and can build nothing else:
        // opening a document needs bytes, and if the worker could not run,
        // nothing here fetched any.
        self.source = String::from("built-in scene");
        Ok(self.bodies.len() as u32)
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

/// A packed position lifted to the document's own precision.
///
/// Both meshing paths accumulate bounds through this one function, so that
/// neither can drift into framing the scene differently from the other.
fn position_of(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
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

/// A plate with a hole, on `TruckKernel` — the same document the headless
/// tests draw, so that a difference between here and there is a difference in
/// the browser rather than in the scene.
///
/// The second half of the pair says whether it really is a plate with a hole.
/// Between 2026-08-27 and 2026-09-05 this function asked for the difference
/// and drew a plate: the backend's boolean returned a copy of its first
/// operand, and `let _ =` on the result meant the page could not tell. The
/// boolean is real now and can still decline — so the answer is carried out to
/// `report()` and asserted in the browser, rather than assumed here.
fn scene() -> (Document<TruckKernel>, bool) {
    let mut doc = Document::new(TruckKernel::default());
    let plate = doc
        .add_box("plate", Vec3::new(40.0, 40.0, 10.0))
        .expect("box");
    let drill = doc.add_cylinder("drill", 6.0, 20.0).expect("cylinder");
    let _ = doc.transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)));
    let modelled = doc.boolean(BooleanOp::Difference, plate, drill).is_ok();
    (doc, modelled)
}

fn get(target: &Object, key: &str) -> Result<JsValue, JsError> {
    Reflect::get(target, &key.into())
        .map_err(|_| JsError::new(&format!("could not read `{key}` from the facts object")))
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsError> {
    Reflect::set(target, &key.into(), value)
        .map(|_| ())
        .map_err(|_| JsError::new("could not build the report object"))
}
