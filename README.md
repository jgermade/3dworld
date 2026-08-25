# 3dworld

A B-rep modeller in the mould of [Plasticity](https://www.plasticity.xyz/): NURBS surfaces and
exact solids, direct modelling rather than parametric history, running in the browser on
WebAssembly and on the desktop from the same Rust core.

**GPL-3.0-or-later.**

The seam, the document and an OpenCASCADE backend are built and tested. There is no viewport yet.

```
make test       # check, clippy -D warnings, and the tests — no setup needed
make wasm       # the same code, built for wasm32-unknown-unknown
make test-occt  # the conformance suite against real geometry (needs OCCT)
```

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
archive. Take the header from upstream at the matching tag:

```sh
sudo curl -o /usr/include/opencascade/NCollection_AliasedArray.hxx \
  https://raw.githubusercontent.com/Open-Cascade-SAS/OCCT/V7_6_3/src/NCollection/NCollection_AliasedArray.hxx
```

What to read:

- [STACK.md](STACK.md) — the shape, and every choice with what forced it.
- [AGENTS.md](AGENTS.md) — conventions, and the rules that are not style.
- [SESSIONS/](SESSIONS/) — the record. `*.md` is open, `*.completed.md` is finished. **The plan
  is `## The pending register` at the end of the open file**; the `Next` blocks above it are
  superseded and kept for the order things became pending in.
- [`kernel/src/lib.rs`](kernel/src/lib.rs) — the contract, and the two properties of it that
  everything above depends on.
- [`kernel-occt/native/w3d_occt.h`](kernel-occt/native/w3d_occt.h) — the C ABI, which is the
  specification of what an OpenCASCADE build must export.
