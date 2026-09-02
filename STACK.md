# Stack

## The shape of it

```
   the browser tab                                   the desktop binary
        │                                                     │
        │  web/loader.js — probe, then look at the result     │  winit
        ▼                                                     ▼
┌─────────────────────────────────────────────────────────────────────┐
│  3dworld                                                            │
│                                                                     │
│   the modeller                     w3d-app     egui over wgpu     ✅ │
│     document · undo · selection    w3d-core                       ✅ │
│     the .w3d file · FORMAT.md      w3d-format                     ✅ │
│     tessellation cache             w3d-core                       ✅ │
│     viewport · camera · picking    w3d-render                     ✅ │
│     the loader, and a canvas       w3d-web                        ✅ │
│                                                                     │
│   ═════ w3d_kernel::GeometryKernel ═════  the seam, and the spec  ✅ │
│         + conformance: one suite, every backend                     │
│                                                                     │
│   w3d-kernel-fake  no geometry, full contract — drives the tests  ✅ │
│   w3d-kernel-occt  OpenCASCADE through a 13-entry C ABI           ✅ │
│     └ native build ✅   Emscripten build ⬜                          │
│   kernel-native/   ours, or truck — swapped in behind the trait   ⬜ │
└─────────────────────────────────────────────────────────────────────┘
        │                                                     │
        ▼                                                     ▼
   wgpu → WebGL2 ✅  WebGPU ⬜ never yet drawn            wgpu → Vulkan / Metal
   SIMD128 · shared memory + rayon · COOP/COEP           AVX-512 · real threads · no 4 GB
```

Everything above the double line is Rust with no dependency on any particular kernel, and is
tested against a fake one — no OCCT, no browser, no `.wasm`. Everything below is a large C++
build whose artifacts are published, never committed.

The left column and the right column are the same source. That is the point of the whole
arrangement: the desktop build is not a port, it is the same crate with a different backend for
the parts wasm constrains.

## Choices, and what forced them

| Choice | Why |
| --- | --- |
| **Rust, not Go** | No GC in an interactive modeller; real threads under wasm, which `GOOS=js` does not have; SIMD128; and C++ interop that a kernel wrapper cannot do without. Go additionally has no geometry ecosystem to speak of. The decisive one is that Rust gives the desktop build from the same source, and Go would have meant Wails or Electron on top. |
| **A `GeometryKernel` trait from the first commit** | The kernel is the whole difficulty and the decision cannot be made well up front. Behind a trait, `kernel/occt/` is a way to have a usable modeller in weeks and `kernel/native/` is a way to not be married to it. Without one, the choice is permanent by month two. |
| **OCCT first, ours later** | OpenCASCADE is mature B-rep and brings STEP/IGES for free. It is also 5–40 MB of wasm, an unpleasant C++ API, LGPL-with-exception, and fillets well short of Parasolid. It buys time, not the product. |
| **wasm32, not wasm64** | Memory64 is standardised (Wasm 3.0) and shipping in Chrome 133+ and Firefox 143+, so the objection is not availability. It is that 64-bit memory cannot use the 4 GB guard-page trick, so every access is bounds-checked: SpiderMonkey measured 10% to over 100%. A memory-bound kernel sits at the wrong end of that. Rust's target is Tier 3 besides. |
| **Sharding across wasm32 heaps, not one wasm64 heap** | The interactive document lives in one shared memory with a rayon pool over it. Bulk work — large booleans, mass tessellation, a huge STEP import — goes to workers with their *own* memory, each with its own 4 GB. Total addressable exceeds the ceiling without paying for 64-bit pointers anywhere. Exchange is by transferable `ArrayBuffer`, which is a move. |
| **`wgpu`, targeting WebGPU** | One API over WebGPU, Vulkan and Metal, which is what makes one source serve both columns above. The WebGL2 fallback is a fallback for *rendering*: it has no compute shaders, so anything built on compute degrades to the CPU rather than to a slower GPU. Budget for that, do not discover it. **Which of the two you get is decided by looking, not by asking** — a browser can report WebGPU, hand back a generous adapter and rasterise nothing, so the loader renders a frame and counts the colours before it believes the answer. |
| **`egui`, not a DOM framework** | In a modeller the UI is not the viewport, and that is what decides it. On the web a DOM shell composes fine around a `<canvas>`; on the desktop, any DOM framework is a webview, and compositing a native wgpu surface with a webview has only two answers — a transparent overlay whose mouse events fight the viewport's, or blitting frames into the webview at unusable latency. egui draws the chrome as GPU geometry in the same pass as the scene: one surface, one loop, nothing to composite. Blender, Fusion and Plasticity all land here. Dioxus was the strongest candidate against it and fails on exactly this; keep it in mind for auxiliary surfaces that composite with nothing. |
| **The kernel stays on the CPU** | WGSL has no `f64`. The GPU takes display tessellation and LOD, BVH build, culling, ID-buffer picking, silhouette and edge extraction, instance transforms. It does not take anything whose correctness is numerical. |
| **SIMD128 as the baseline, threads as the branch** | `+simd128` is universal since Safari 16.4, so it is not worth a variant. Threads are: they need `SharedArrayBuffer`, which needs COOP/COEP, which depends on the host's headers rather than the user's hardware. That is the one axis the loader really has to probe. |
| **A native `.w3d`, and STEP for everything else** | No existing format holds a graph of nodes with names, visibility and a document tolerance — Fusion's `.f3d` is undocumented and its geometry is proprietary ASM, and FreeCAD's `.FCStd` is a parametric feature tree we do not have. So the native file is ours and specified in `FORMAT.md`, and it is a zip so that `unzip -l` works. Interchange is **STEP**, which reaches Fusion, FreeCAD, SolidWorks and Onshape with one implementation; writing anyone else's native format would be a second implementation to reach somewhere STEP already goes. |
| **Arena + `u32` index everywhere** | Forced by the sharding above — a pointer means nothing in another worker's heap. It also halves node size, and it is what undo, serialisation and stable entity IDs want independently. |

## Consequences a host has to know about

- **Cross-origin isolation is required for the threaded variant.** Without
  `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`,
  `SharedArrayBuffer` is unavailable. The loader must probe and fall back to the single-threaded
  variant with a named, visible degradation — not fail obscurely, and not pretend it is fine.
- **A host that cannot send them is not the end of it.** GitHub Pages serves static files and has
  no way to add a response header, which is where this is deployed. `web/coi-serviceworker.js`
  supplies both headers from a service worker: once one controls the page, every response is
  re-issued through it and can carry headers the server never sent. It costs one reload on a first
  visit — a worker does not control the navigation that registered it — and it makes
  `require-corp` a constraint on the whole page, so any cross-origin subresource added later needs
  CORP or CORS from its own host or it is blocked. A host that *can* send the headers should send
  them; Cloudflare Pages and Netlify take a `_headers` file, and a real header beats a worker that
  can be off. The page says which of the two it got.
- **There is no CPUID inside wasm.** A module cannot ask what the machine can do; detection
  happens in JS before instantiation, via `WebAssembly.validate()` on probe modules —
  `web/loader.js` carries them inline rather than fetching `wasm-feature-detect` before it can
  decide anything. That means a build matrix and payload cost, which is why the matrix is kept to
  two entries. **Only one of the two is built today**: the loader dispatches to the threaded
  variant, finds it missing, and says so on the page.
- **A probe cannot certify a driver.** Validation answers what the *engine* accepts and
  `crossOriginIsolated` answers what the *page* was served, but neither can tell you whether the
  GPU will actually rasterise. Headless Chromium reports WebGPU, returns an adapter claiming
  compute shaders and a gigabyte of buffer, and draws a black canvas with no error. The loader
  therefore renders a frame, samples the canvas, and falls back to WebGL2 from evidence.
- **4 GB is a per-heap ceiling and a design constraint.** Tessellation lives in GPU buffers, not
  in linear memory. Inactive bodies go out of core. If a document is approaching the ceiling in
  the *interactive* heap, the answer is sharding, not wasm64.
- **Determinism is a product property.** The same document must produce the same topology on
  every machine, which is what forbids relaxed SIMD below the seam and what makes fixture
  regression meaningful at all.
- **The gap against Plasticity is Parasolid, and it is honest to say so.** Robust booleans and
  fillets that survive degenerate input are decades of work. Whatever ships first will be worse
  at exactly that, and the roadmap is about closing it, not about UI.

## Versions

| | |
| --- | --- |
| Rust | stable, edition 2024; `unsafe_code = "forbid"` workspace-wide. Built on **1.98**; the floor is egui 0.36's MSRV of **1.95**, which is what moved it off 1.94. |
| Targets | `x86_64-unknown-linux-gnu` and `wasm32-unknown-unknown`, both checked; `+simd128` checked |
| UI | `egui` 0.36 with `egui-wgpu` and `egui-winit`, `winit` 0.30. egui-wgpu 0.36 is built on wgpu 30, which is why the version had to be that one — egui 0.35 would have dragged in a second, incompatible wgpu. |
| Dependencies | **`wgpu` 30** (MIT OR Apache-2.0) in `w3d-render`, plus `wasm-bindgen`/`js-sys`/`web-sys` in `w3d-web` — all declared per target with `default-features = false`. The kernel, the fake and the document still depend on each other and on nothing else. Playwright is a devDependency of `web/test/` and is not in the crate graph. |
| Threads | `wasm-bindgen-rayon`, `+atomics,+bulk-memory,+mutable-globals`, nightly for `build-std`. **Not built.** The loader probes for them, reports them present, and runs the single-threaded variant because that is the only one that exists. |
| Graphics | `wgpu` 30 — WebGPU where present, WebGL2 fallback. Both compiled for wasm; only WebGPU-class backends have been *run*, and those under lavapipe. The wasm feature is `webgl`, **not** `gles`; `gles` is the native GL backend and silently does nothing on wasm32. |
| Kernel | OpenCASCADE (LGPL-2.1-only, taken to GPL-3 via its §3) behind `GeometryKernel` |
| Licence | GPL-3.0-or-later — see AGENTS.md § Licensing |
| OpenCASCADE | 7.6.3 (Ubuntu Noble), recorded in `kernel-occt/native/UPSTREAM`. That file names a version; it does not enforce one — the build takes whatever the system has. A real pin arrives with the Emscripten build. Noble's `libocct-foundation-dev` is missing a header: `make occt-headers`. |
| Emscripten | not yet — the OCCT wasm build does not exist, so the browser draws a `FakeKernel` bounding box |
| Browser build | `wasm-bindgen` 0.2.127 and a matching `wasm-bindgen-cli`; `make web` → 3.34 MiB of wasm, 1.12 MiB gzipped. No `wasm-opt`, no brotli. |
| Browser check | `make web-test` — Chromium via Playwright, three runs: WebGPU offered, WebGL2 forced, and no COOP/COEP |
