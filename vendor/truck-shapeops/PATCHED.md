# `truck-shapeops` 0.4.0, patched

A copy of [`truck-shapeops`](https://github.com/ricosjp/truck) **0.4.0** as published on crates.io
(checksum `fd6e83ea4d36d00229d38ab985a991e32f58c3ed2971083011ac66fb57e26d5c`, upstream commit
`c23d2c25c1cd83b615d35db72cc5a4432c693e17`, path `truck-shapeops`), wired into the workspace by
`[patch.crates-io]` in the root `Cargo.toml`. It is Apache-2.0, like upstream; `LICENSE` is the
licence text, which the published crate does not carry. The changes below are upstream's
licence's, not this repository's GPL, and each changed file says it was changed (Apache-2.0 §4(b)).

Removed from the published package, and nothing else: `.cargo-ok`, `.cargo_vcs_info.json` (its
contents are the commit above), `Cargo.lock`, `Cargo.toml.orig`.

## What differs, and why

**`src/transversal/polyline_construction/mod.rs`** — the fix. `construct_polylines` chains the
boolean's intersection segments through a `Graph` of `FxHashMap<PointIndex, Node>`, and picks the
first node with `iter().next()` and each next direction with `adjacency.iter().next()`.
`rustc-hash` multiplies by `0xf1357aea2e62a9c5` when `usize` is 64 bits and by `0x93d765dd` when
it is 32, so the same closed loop is entered at a different vertex on wasm32 than on x86-64; the
curve is parameterized from elsewhere and the tessellator divides it differently. The same
document cut to **6294** triangles on the desktop and **6290** in the browser. Diagnosed in
`RECORD/2026-09-21_22h10.the-boolean-disagrees-by-a-hash.completed.md`.

The change is the import — `BTreeMap`/`BTreeSet` over the same key instead of `FxHashMap`/
`FxHashSet` — and `PartialOrd, Ord` on `PointIndex`, which is `[i64; 3]` and so has a total order
that is the same on every target. Nothing else in the function moves. It now cuts to **6292** on
both, bit-identical: `make xarch` asserts it.

**`src/lib.rs`** — `#![cfg_attr(not(debug_assertions), deny(warnings))]` removed. Cargo passes
`--cap-lints` to a registry dependency and not to a path dependency, so the published crate's
`dead_code` warning on `Alternative` is fatal in a release build here and invisible on crates.io.
It changes no code.

## What is not claimed

**6292 is agreement, not correctness.** It is neither of the two old answers: an ordered map picks
a third starting vertex. The conformance suite passes against it, which is what a change to a
boolean has to be argued against, and it is one scene's evidence that the two architectures agree.
Whether either answer was the better curve is not a question this patch asks.

**Only the one map whose order reached the output was changed, and the rest were read.** Audited
on 2026-09-27, in `RECORD/2026-09-27_09h16.the-order-a-boolean-hands-back.completed.md`:

- `divide_face/` and `loops_store/` use their maps only through `entry`, `get`, `insert` and
  `remove` — caches and lookups, never iterated. Their order cannot reach anything.
- `faces_classification/` looks faces up in its map and iterates the `Shell` beside it, a `Vec`.
- `healing/` iterates one map (`param_vertices`, in `enumerate_vertices_on_divisor`) and sorts
  the result by parameter afterwards, so only an exact tie keeps the hash's order; its maps are
  keyed by `usize`, whose hash differs by target. It is not on the boolean's path at all — it is
  exported for imported shapes, and nothing in this repository calls it.

What *does* reach a result is not in this crate: `and` and `or` end in
`truck_topology::Shell::connected_components`, which walks an `FxHashMap` keyed by `&Face` —
by address — so the **order of a boolean's faces** changes between runs. It changes the order,
not the solid. `w3d-kernel-truck` no longer lets it show: `save_body` writes a canonical order,
faces are sorted with a geometric tiebreak, and edges are numbered by where they are.
`integrate_by_component` also decides a component's status from `boundary[0][0]`, the first
edge of an order that map sets; on a component whose boundary touches only one class, which is
every one a valid input makes, any edge gives the same answer.

## Dropping it

Report upstream, and remove this directory and the `[patch.crates-io]` entry the day a published
`truck-shapeops` iterates that graph in a target-independent order. `make xarch` is the check: the
six `cut.*` rows fail without the fix, and were shown to on 2026-09-26.
