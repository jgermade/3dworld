# Conventions

## Architecture

Check [STACK.md](STACK.md).

## Sessions

Work is recorded in [`SESSIONS/`](./SESSIONS/), one file per **body of work** — not one per
sitting. A file is opened when a piece of work starts, named
`YYYY-MM-DD_HHhMM.<kebab-case-summary>.md` for the moment it was opened, and written in English.
Several sessions may extend the same file; one session may open more than one.

Two rules, and they pull in different directions on purpose:

- **No prior approval is needed.** Work proceeds — no waiting on a plan being signed off.
- **But it must end up written down.** A session file is not a changelog of commits; it is the
  record of what was decided, what was learned, and what is *not* true yet. The parts that earn
  their keep are the ones a commit message cannot hold:

  | Section | What it is for |
  | --- | --- |
  | `Where this repo actually was` | The state of the world before, so a later reader can tell what changed from what was already broken. |
  | `Walkthrough · as built` | What landed, not what was planned. |
  | `Bugs found by building, not by reading` | Defects the *process* surfaced, with their cause. These are the ones that get re-introduced otherwise. |
  | `Verified, and not` | Two explicit lists. The second one matters more: name the caveat and the risk taken. |
  | `Loose ends, deliberately left` | What was not done, and what it would cost. |
  | `Next` | What remains. Without it a session file is a diary; with it, the folder is a plan. |

A geometry kernel makes one section carry more than its share. **`Verified, and not` is where a
numerical claim goes to be qualified**: "the boolean is correct" is not a finding, "the boolean is
correct on the twelve fixtures in `tests/fixtures/bool/`, all of them non-degenerate, and untested
against coincident faces" is. Robustness bugs in this domain are found years later by a user, not
minutes later by CI, and the only defence is that nobody was ever told the case was covered.

### Closing a file

When everything a file set out to do is done:

1. **Append a final `## Walkthrough · as completed`** — what actually landed across the whole
   body of work, not a summary of what was planned.
2. **Rename it from `.md` to `.completed.md`.**

So `SESSIONS/` read at a glance answers the two questions that matter: `*.md` is what is still
open, `*.completed.md` is what is finished and why. Nothing is deleted, and nothing moves out of
the folder.

A file is only closed when its `Next` is empty. If part of the work is being abandoned rather
than finished, say so in the walkthrough — an abandoned item closed quietly is indistinguishable
from a forgotten one.

### The plan is the last register, not the last `Next`

A `Next` block records what was pending *when that session ended*, and a file with four of them is
a plan a reader has to reconstruct. When a file's `Next` blocks have accumulated, close them with a
single `## The pending register` that says out loud which registers and `Next` blocks it
supersedes, and gather into it what the `Next` blocks never held — the owed work named in
`Loose ends, deliberately left` and in the second half of `Verified, and not`.

There is exactly one live register across the whole of `SESSIONS/` at any time. When a new file
takes it over, the old file gets a one-paragraph extension saying so and pointing at the new one.
Everything above stays standing: the order in which things became pending is part of the record.

### Always append, never rewrite

Extensions are `## Extension · <date> · <summary>`, corrections are `### Correction · …`. When a
decision supersedes an earlier one, say so out loud in the new text and leave the old text
standing — a session file edited to look right is worth nothing, because the wrong turns are most
of the value. The wasm64 reversal in the 2026-08-25 file is the worked example: the first claim
was that 64-bit memory "is not viable yet", which was wrong on the facts and right on the
conclusion, and both halves of that are worth more than the tidy version.

This rule governs `SESSIONS/` only. Documents that describe how things *are* — this file,
`README.md`, `STACK.md` — are kept correct by editing them.

## What this repository is

A B-rep modeller in the mould of Plasticity: NURBS surfaces and exact solids, driven for
direct modelling rather than parametric history, running in the browser on WebAssembly and on
the desktop from the same core.

What exists is the top half. The rest is the decided shape, and code that lands either matches
it or amends this file in the same commit.

```
kernel/        w3d-kernel       the seam: the trait, the value types, the        ✅ built
                                conformance suite every backend must pass
kernel-fake/   w3d-kernel-fake  a backend that satisfies the contract without    ✅ built
                                doing geometry
core/          w3d-core         document · history/undo · selection ·            ✅ built
                                tessellation cache — generic over the kernel

kernel-occt/   w3d-kernel-occt  the OpenCASCADE backend: a C ABI of thirteen    ✅ native
                                entry points, and the Rust side of it            ⬜ wasm

render/        w3d-render       wgpu: capability detection, mesh upload,        ✅ built
                                camera, and ID-buffer picking                    ⬜ WebGL2 untried

kernel-native/                  a kernel of our own, or truck                    ⬜
app/                            the modeller; native and web from one source     ⬜
web/                            the loader: feature probe, dispatch, COOP/COEP   ⬜
```

Crates are prefixed `w3d-` because `3dworld` is not a valid Rust identifier and the name is not
settled anyway. Directory names are the ones above; do not rename either half unilaterally.

The seam is `kernel::GeometryKernel`. It is not a convenience trait: it is what lets the whole of
`core/`, `render/` and `app/` be written, reviewed and tested before a decision on the kernel is
final, and it is what makes that decision reversible afterwards. **When the modeller needs a new
geometric capability, it is declared there first**, then implemented behind whichever backend, then
used.

## Rules that are not style

- **Nothing below `GeometryKernel` leaks above it.** No `TopoDS_Shape`, no OCCT handle, no
  `Standard_Real` in `core/`. The moment a caller pattern-matches on a backend type the seam has
  stopped being a seam and the kernel decision has stopped being reversible. Every escape is a
  method on the trait or it does not happen.
- **Arenas and `u32` indices, never pointers, for anything that crosses a thread or a heap.**
  This is not a preference. Heavy work runs in workers with *their own* linear memory (see
  STACK.md), so a pointer is meaningless one line later; an index is valid in any heap. It also
  halves the size of every B-rep node against 64-bit pointers, and it is what undo and
  serialisation want anyway.
- **Relaxed SIMD is forbidden in kernel predicates.** It is allowed — welcome, even — in
  tessellation, transforms and anything feeding the GPU. In an orientation test or an
  intersection it is poison: the results are permitted to differ between machines, so the same
  document would produce different topology on different hardware and the regression fixtures
  would stop meaning anything. The rule is per-crate, not per-call: `kernel/` is built without it.
- **The kernel does not move to the GPU.** WGSL has no `f64` and no exact predicates. Booleans,
  intersections and anything whose correctness is numerical stay on the CPU, permanently, and no
  benchmark is an argument against this.
- **wasm32 only.** wasm64 costs between 10% and over 100% on bounds checks, is Tier 3 in Rust,
  and buys an address space this architecture does not need. Revisit only when *both* the
  hardware bounds-check support lands and a real document is measured against the 4 GB ceiling
  — and record the measurement, not the intuition.
- **A tolerance is an argument, never a constant.** Every predicate that compares takes its
  tolerance from the document, and no file outside `kernel/` invents an epsilon. Scattered
  hard-coded epsilons are the standard way a CAD kernel becomes unfixable.
- **Bodies are immutable; every operation returns a new one.** Undo is then a matter of keeping
  the old handle rather than inverting an operation, which is the only undo a B-rep kernel can
  implement honestly — nothing can invert a boolean. `conformance` enforces it, because a backend
  that consumes its operands makes history impossible and would be found much later otherwise.
- **`unsafe` lives in exactly one crate, and that crate is the FFI boundary.** The workspace
  *forbids* it, and `forbid` cannot be relaxed by an `#[allow]` in a file — so `w3d-kernel-occt`
  opts out of the shared lint in its own `Cargo.toml`, which is a visible act in a reviewed file
  rather than an attribute buried in a module. A second crate wanting `unsafe` is a design
  conversation, not an edit.
- **The C ABI header is the specification, not a convenience.** Every symbol
  `kernel-occt/native/w3d_occt.h` declares is a symbol an Emscripten build has to keep alive
  through `EMSCRIPTEN_KEEPALIVE`. It carries thirteen entry points because the trait has thirteen
  methods. Declare there first, then implement in C++, then use from Rust — and never widen it to
  "expose a bit more of OCCT".
- **What the machine can do is read, not assumed — and a capability cannot report a backend that
  is not in the binary.** `Capabilities` is built from the adapter, never from `cfg!`, because a
  web build compiled with both backends can land on WebGPU or on WebGL2 and there is no way to
  know which until it has. That covers the runtime half. The build half is not something a probe
  can see: wgpu's `gles` feature is the *native* GL backend and does nothing on wasm32, so asking
  for it instead of `webgl` compiles, type-checks and ships with no fallback at all. `make wasm`
  greps the tree for `glow` for that reason. A degradation nobody can name is a bug report about
  performance six months later.
- **Dependencies are declared per target, with `default-features = false` and the backend list
  spelled out.** This workspace had none until `w3d-render`, and the first one is where the habit
  gets set: the enabled backend list *is* the platform decision in STACK.md, and a
  default-features build hides it in a lockfile. The licence rule below is not the only reason to
  read a dependency's feature table.
- **Arena slots are never reused.** Undo restores a node into the slot it came from, so a free
  list lets a later insert take a slot an undo still needs, and the undo then silently does
  nothing. This was written, shipped and reverted within one session; the cost is that an arena
  grows with the nodes a session ever created. Do not reintroduce a free list to reclaim it —
  compaction with history cleared is the honest fix, and it is not written.

## Testing

`make test` is the whole of it, and it is three different kinds of check:

| Command | What it proves |
| --- | --- |
| `make check` | Every crate compiles, tests and all. |
| `make clippy` | `-D warnings`. There is no allow-list; a lint that has to be silenced gets an argument in a session file. |
| `cargo test --workspace` | The document, history, selection and the arena's identity rules — driven against `w3d-kernel-fake`, with no OCCT, no browser and no `.wasm` anywhere. |
| `w3d_kernel::conformance` | One suite, run against *every* backend, `FakeKernel` included. A backend is only a backend if it passes it, and this is what keeps the kernel decision reversible. |
| `make wasm` | The default members build for `wasm32-unknown-unknown`, **and the WebGL2 backend is really in the tree**. Nothing above the seam may acquire a host assumption without the first half failing; the second half exists because asking wgpu for `gles` instead of `webgl` compiles cleanly and ships no fallback at all. |
| `make test-occt` | The same conformance suite against **real geometry**, plus the document driven by OpenCASCADE, plus the only end-to-end test there is: real geometry through the real viewport, and a click that names a face. Not part of `make test`, because it needs OCCT installed; `w3d-kernel-occt` is excluded from the workspace's `default-members` so that `make test` stays a no-setup command. |
| `make occt-headers` | Not a check. Fetches headers a distribution failed to ship, at the revision in `kernel-occt/native/UPSTREAM` — needed on Ubuntu Noble. Deliberately not run from `build.rs`: a build that reaches the network on its own is not a build anybody can reproduce. |

Still owed, and named so that the gap is not mistaken for coverage:

- **Fixture regression** — named solids in, golden topology out. The only defence against a
  robustness fix breaking three cases to fix one. `kernel-occt/tests/document.rs` is the first
  seed of it (a drilled plate is asserted at 7 faces, 15 edges, 10 vertices) but it is five cases,
  not a suite, and none of them is degenerate.
- **`wasm-pack test --headless`** — that the thing actually instantiates under COOP/COEP with
  threads. Needs `web/` to exist.
- **A GPU in CI.** `w3d-render`'s tests need an adapter and *skip*, printing `SKIPPED:`, when there
  is none — and `cargo test` then reports `ok`. They pass here against lavapipe, Mesa's software
  rasteriser. Until CI has one, a green run is not evidence the viewport works.
- **WebGL2 has never been run.** It is compiled and asserted present. Every claim about the
  fallback — `Rg32Uint` as a render target, the scissored pick, the downlevel limits — is an
  argument until a browser has executed it.
- **An OCCT build for wasm.** `kernel-occt` compiles and passes natively; no Emscripten build
  exists, and `-fwasm-exceptions` against `-fexceptions` has not been decided.

**Prefer extending `w3d-kernel-fake` over mocking `core`.** A test that stubs the document proves
nothing; one that stubs the kernel proves the whole modeller.

And when a check belongs to *the contract* rather than to one backend, it goes in
`kernel/src/conformance.rs`, not in a test file. Everything there has to be true of any correct
kernel — which rules out most interesting assertions, and is the discipline that makes the ones
left over mean something.

## Licensing

**This repository is GPL-3.0-or-later**, and the reasoning is in the 2026-08-25 session file
under `Extension · the licence and the first backend`. The short version:

- **OpenCASCADE is LGPL-2.1-*only*.** Its §3 carries the permission to apply "the ordinary GNU
  General Public License" to a copy, of any version — the clause anticipates versions later than
  2 explicitly. That is the route by which an LGPL-2.1-only kernel lives inside a GPL-3 work, and
  it is the whole of why this licence was available to choose.
- **The `OCCT-exception-1.0` is not what it is usually said to be**, and this repository does not
  rely on it. It covers exactly one thing: header material — inlines and templates — incorporated
  into object code, so that LGPL does not leak into proprietary callers through C++ headers. It
  does **not** waive LGPL-2.1 §6, the right to relink against a modified library. A wasm build is
  static linking, so a closed distribution would have owed §6 a real answer. GPL-3 owes it
  nothing, because the source ships.
- **Modifications to OCCT are OCCT's**, whatever this repository is licensed as. Emscripten port
  patches live in their own series and are published under LGPL-2.1.
- **Parasolid**, which is what actually makes Plasticity good, is a commercial Siemens licence
  with no WebAssembly distribution. It is not an option here; the gap it leaves is the whole
  difficulty of this project and should be named as such rather than wished away.

What this forecloses, deliberately: a closed binary sold under a perpetual licence, which is
Plasticity's own model. Relicensing later would mean removing OCCT.

Serving the `.wasm` to a browser is **distribution** — GPL-3, not AGPL, and the SaaS distinction
neither saves us nor is needed. Ship a link to the corresponding source alongside the build.

Do not add a dependency whose licence is incompatible with GPL-3.0-or-later.

## Temporary files and scripts

Use `.tmp/` in the project root; scripts in `.tmp/scripts/`. Both are gitignored.
