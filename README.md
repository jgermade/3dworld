# 3dworld

A B-rep modeller in the mould of [Plasticity](https://www.plasticity.xyz/): NURBS surfaces and
exact solids, direct modelling rather than parametric history, running in the browser on
WebAssembly and on the desktop from the same Rust core.

**GPL-3.0-or-later.**

The seam, the document, an OpenCASCADE backend and the viewport are built and tested. Nothing
presents to a window or a canvas yet: `w3d-render` draws into textures, and the application shell
that owns a surface does not exist.

```
make test       # check, clippy -D warnings, and the tests — no setup needed
make wasm       # the same code, built for wasm32-unknown-unknown
make test-occt  # real geometry, and a click that names a face (needs OCCT)
```

`w3d-render`'s tests need a graphics adapter. Without one they **skip**, printing `SKIPPED:` and
the reason — so a green `make test` on a machine with no GPU is not evidence the viewport works.
Run `cargo test -p w3d-render -- --nocapture` to see which happened. A software rasteriser is
enough: `apt install mesa-vulkan-drivers` gets lavapipe.

## Building the OpenCASCADE backend

Only needed for `make test-occt`; everything else builds with a bare Rust toolchain.

```sh
apt install libocct-foundation-dev libocct-modeling-data-dev libocct-modeling-algorithms-dev
```

`OCCT_INCLUDE_DIR` and `OCCT_LIB_DIR` override discovery if your headers are elsewhere.

**On Ubuntu 24.04 (Noble) that package is broken** and the compiler error points nowhere useful.
`libocct-foundation-dev` 7.6.3+dfsg1-7.1build1 ships `Poly_ArrayOfNodes.hxx`, which includes
`NCollection_AliasedArray.hxx`, which it does not ship — so every translation unit that reaches
`Poly_Triangulation` fails, and that is all of modelling. There is no other version in the
archive.

```sh
make occt-headers
```

fetches the missing header from upstream at the tag in
[`kernel-occt/native/UPSTREAM`](kernel-occt/native/UPSTREAM), into a gitignored
`kernel-occt/native/vendor-include/` that the build puts *after* the system include path. Nothing
is committed — the file is OCCT's, LGPL-2.1. `build.rs` detects this exact case and says so,
rather than leaving you with forty lines of include trace.

What to read:

- [STACK.md](STACK.md) — the shape, and every choice with what forced it.
- [AGENTS.md](AGENTS.md) — conventions, and the rules that are not style.
- [SESSIONS/](SESSIONS/) — the record. `*.md` is open, `*.completed.md` is finished. **The plan is
  `## The pending register` at the end of the newest file**; there is exactly one live register
  across the folder, and every `Next` block above it is superseded and kept only for the order in
  which things became pending.
- [`kernel/src/lib.rs`](kernel/src/lib.rs) — the contract, and the two properties of it that
  everything above depends on.
- [`kernel-occt/native/w3d_occt.h`](kernel-occt/native/w3d_occt.h) — the C ABI, which is the
  specification of what an OpenCASCADE build must export.
