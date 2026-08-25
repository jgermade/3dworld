# 3dworld

A B-rep modeller in the mould of [Plasticity](https://www.plasticity.xyz/): NURBS surfaces and
exact solids, direct modelling rather than parametric history, running in the browser on
WebAssembly and on the desktop from the same Rust core.

The seam and the document are built and tested; no real kernel is chosen yet.

```
make test     # check, clippy -D warnings, and the workspace tests
make wasm     # the same code, built for wasm32-unknown-unknown
```

What to read:

- [STACK.md](STACK.md) — the shape, and every choice with what forced it.
- [AGENTS.md](AGENTS.md) — conventions, and the rules that are not style.
- [SESSIONS/](SESSIONS/) — the record. `*.md` is open, `*.completed.md` is finished.
- [`kernel/src/lib.rs`](kernel/src/lib.rs) — the contract, and the two properties of it that
  everything above depends on.
