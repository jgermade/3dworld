# Stack

## The shape of it

```
   the browser tab                                   the desktop binary
        │                                                     │
        │  web/loader.ts — probe, then choose a variant       │  winit
        ▼                                                     ▼
┌─────────────────────────────────────────────────────────────────────┐
│  3dworld                                                            │
│                                                                     │
│   the modeller                     app/        egui or a TS shell ⬜ │
│     document · undo · selection    w3d-core                       ✅ │
│     tessellation cache             w3d-core                       ✅ │
│                                                                     │
│   ═════ w3d_kernel::GeometryKernel ═════  the seam, and the spec  ✅ │
│         + conformance: one suite, every backend                     │
│                                                                     │
│   w3d-kernel-fake  no geometry, full contract — drives the tests  ✅ │
│   kernel/occt/     OpenCASCADE, built by Emscripten               ⬜ │
│   kernel/native/   ours, or truck — swapped in behind the trait   ⬜ │
└─────────────────────────────────────────────────────────────────────┘
        │                                                     │
        ▼                                                     ▼
   wgpu → WebGPU  (WebGL2 fallback: no compute)          wgpu → Vulkan / Metal
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
| **`wgpu`, targeting WebGPU** | One API over WebGPU, Vulkan and Metal, which is what makes one source serve both columns above. The WebGL2 fallback is a fallback for *rendering*: it has no compute shaders, so anything built on compute degrades to the CPU rather than to a slower GPU. Budget for that, do not discover it. |
| **The kernel stays on the CPU** | WGSL has no `f64`. The GPU takes display tessellation and LOD, BVH build, culling, ID-buffer picking, silhouette and edge extraction, instance transforms. It does not take anything whose correctness is numerical. |
| **SIMD128 as the baseline, threads as the branch** | `+simd128` is universal since Safari 16.4, so it is not worth a variant. Threads are: they need `SharedArrayBuffer`, which needs COOP/COEP, which depends on the host's headers rather than the user's hardware. That is the one axis the loader really has to probe. |
| **Arena + `u32` index everywhere** | Forced by the sharding above — a pointer means nothing in another worker's heap. It also halves node size, and it is what undo, serialisation and stable entity IDs want independently. |

## Consequences a host has to know about

- **Cross-origin isolation is required for the threaded variant.** Without
  `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`,
  `SharedArrayBuffer` is unavailable. The loader must probe and fall back to the single-threaded
  variant with a named, visible degradation — not fail obscurely, and not pretend it is fine.
- **There is no CPUID inside wasm.** A module cannot ask what the machine can do; detection
  happens in JS before instantiation, via `WebAssembly.validate()` on probe modules
  (`wasm-feature-detect`). That means a build matrix and payload cost, which is why the matrix is
  kept to two entries.
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
| Rust | 1.94.1 stable, edition 2024; `unsafe_code = "forbid"` workspace-wide |
| Targets | `x86_64-unknown-linux-gnu` and `wasm32-unknown-unknown`, both checked; `+simd128` checked |
| Dependencies | **none.** The three crates depend on each other and on nothing else. |
| Threads | `wasm-bindgen-rayon`, `+atomics,+bulk-memory,+mutable-globals`, nightly for `build-std` |
| Graphics | `wgpu` — WebGPU where present, WebGL2 fallback |
| Kernel | undecided — see AGENTS.md § Licensing |
| Emscripten | pinned here once `kernel/occt/` exists |
