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

What to read:

- [STACK.md](STACK.md) — the shape, and every choice with what forced it.
- [AGENTS.md](AGENTS.md) — conventions, and the rules that are not style.
- [SESSIONS/](SESSIONS/) — the record. `*.md` is open, `*.completed.md` is finished.
- [`kernel/src/lib.rs`](kernel/src/lib.rs) — the contract, and the two properties of it that
  everything above depends on.
- [`kernel-occt/native/w3d_occt.h`](kernel-occt/native/w3d_occt.h) — the C ABI, which is the
  specification of what an OpenCASCADE build must export.
