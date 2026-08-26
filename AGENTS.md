# Conventions

## Architecture

Check [STACK.md](STACK.md).

## The record

Work is recorded in [`RECORD/`](./RECORD/), one file per **body of work** — not one per session.
A file is opened when a piece of work starts, named `YYYY-MM-DD_HHhMM.<kebab-case-summary>.md` for
the moment it was opened, and written in English. Several sessions may extend one file; one session
may open more than one.

Two rules, and they pull in different directions on purpose:

- **No prior approval is needed.** Work proceeds — nothing waits on a plan being signed off.
- **But it must end up written down.** A record file is not a changelog of commits; it is the
  account of what was decided, what was learned, and what is *not* true yet — the parts a commit
  message cannot hold:

| Section | What it is for |
| --- | --- |
| `Where this repo actually was` | The state before, so a later reader can tell what changed from what was already broken. |
| `Walkthrough · as built` | What landed, not what was planned. |
| `Bugs found by building, not by reading` | Defects the *process* surfaced, with their cause. These are the ones that get re-introduced otherwise. |
| `Verified, and not` | Two explicit lists. The second matters more: name the caveat and the risk taken. |
| `Loose ends, deliberately left` | What was not done, and what it would cost. |
| `Next` | What remains. Without it a record file is a diary; with it, the folder is a plan. |

A geometry kernel makes one section carry more than its share. **`Verified, and not` is where a
numerical claim goes to be qualified**: "the boolean is correct" is not a finding; "the boolean is
correct on the twelve fixtures in `tests/fixtures/bool/`, all of them non-degenerate, and untested
against coincident faces" is. Robustness bugs in this domain are found years later by a user, not
minutes later by CI, and the only defence is that nobody was ever told the case was covered.

### Closing a file

Append a final `## Walkthrough · as completed` — what landed across the whole body of work, not a
summary of what was planned — and rename the file from `.md` to `.completed.md`. The folder then
answers at a glance the two questions that matter: `*.md` is open, `*.completed.md` is finished and
why. Nothing is deleted, and nothing moves out of the folder.

A file closes when nothing is pending *in it*, which is not the same as everything being done: its
own work has landed and whatever it left owed has moved into the live register, which the
walkthrough must name. Work being abandoned rather than finished is said so out loud — an abandoned
item closed quietly is indistinguishable from a forgotten one.

### The plan is the last register, not the last `Next`

A `Next` block records what was pending *when that session ended*, and a file with four of them is
a plan a reader has to reconstruct. When they accumulate, close them with a single
`## The pending register` naming which registers and `Next` blocks it supersedes, and gather into
it what they never held — the work owed in `Loose ends, deliberately left` and in the second half
of `Verified, and not`.

**There is exactly one live register at any time**, in the open file — `*.md`, of which there
should normally be one. When a new file takes it over, the old file says so and points at the new
one; everything above stays standing, because the order in which things became pending is part of
the record. **The register may also be a file of its own**, and once a body of work is finished
that is the tidier place for it: the closed file stops carrying a plan that has outgrown it, and
the next session opens against a document that is nothing but what is left.
[`2026-08-25_21h59.what-is-not-built-yet.md`](./RECORD/2026-08-25_21h59.what-is-not-built-yet.md)
is that file today; it has no `Walkthrough · as built` and will never grow one — work that acts on
it opens its own file and says which items it took.

**It is also the one file that is maintained rather than appended.** A record is where the wrong
turns are most of the value, so it is only ever added to; a register is a *plan*, and a plan that
only grows stops being one. Items leave it as they are done, and a `## Taken` table says which file
took each so the reasoning stays one link away.

### Always append, never rewrite

Everything except the register. Extensions are `## Extension · <date> · <summary>`, corrections
`### Correction · …`. When a decision supersedes an earlier one, say so out loud in the new text
and leave the old text standing — a record file edited to look right is worth nothing. The wasm64
reversal in the 2026-08-25 file is the worked example: the first claim, that 64-bit memory "is not
viable yet", was wrong on the facts and right on the conclusion, and both halves of that are worth
more than the tidy version.

**This covers what looks like housekeeping.** A closed file is right about its own moment, so a
path, a name or a count inside one is never hand-corrected to match the present. Tidying is how a
record stops being one; if the drift matters, it earns a `### Correction · …` that leaves the
original standing.

This rule governs `RECORD/` only. Documents that describe how things *are* — this file,
`README.md`, `STACK.md` — are kept correct by editing them.

## What this repository is

A B-rep modeller in the mould of Plasticity: NURBS surfaces and exact solids, driven for direct
modelling rather than parametric history, running in the browser on WebAssembly and on the desktop
from the same core.

What exists is the top half. The rest is the decided shape, and code that lands either matches it
or amends this file in the same commit.

```
kernel/        w3d-kernel       the seam: the trait, the value types, the       ✅ built
                                conformance suite every backend must pass
kernel-fake/   w3d-kernel-fake  a backend that satisfies the contract without   ✅ built
                                doing geometry
core/          w3d-core         document · history/undo · selection ·           ✅ built
                                tessellation cache — generic over the kernel
format/        w3d-format       the .w3d file: a zip, a manifest, and one       ✅ built
                                geometry blob per body — see FORMAT.md
kernel-occt/   w3d-kernel-occt  the OpenCASCADE backend: a C ABI, and the       ✅ native
                                Rust side of it                                 ⬜ wasm
render/        w3d-render       wgpu: capability detection, mesh upload,        ✅ built
                                camera, and ID-buffer picking                   ✅ WebGL2
                                                                                ⬜ WebGPU
web/           w3d-web          the loader: probe, dispatch, COOP/COEP, and     ✅ built
                                a canvas that draws and picks                   ⬜ threaded
app/           w3d-app          the modeller: editor, scene, and a winit +      ✅ desktop
                                egui shell that draws in one pass               ⬜ web
kernel-native/                  a kernel of our own, or truck                   ⬜
```

Crates are prefixed `w3d-` because `3dworld` is not a valid Rust identifier and the name is not
settled anyway. Directory names are the ones above; do not rename either half unilaterally.

The seam is `kernel::GeometryKernel`. It is not a convenience trait: it is what lets the whole of
`core/`, `render/` and `app/` be written, reviewed and tested before the kernel decision is final,
and what makes that decision reversible afterwards. **When the modeller needs a new geometric
capability, it is declared there first**, then implemented behind whichever backend, then used.

## Rules that are not style

- **Nothing below `GeometryKernel` leaks above it.** No `TopoDS_Shape`, no OCCT handle, no
  `Standard_Real` in `core/`. The moment a caller pattern-matches on a backend type, the seam has
  stopped being a seam and the kernel decision has stopped being reversible. Every escape is a
  method on the trait or it does not happen.
- **Arenas and `u32` indices, never pointers, for anything crossing a thread or a heap.** Not a
  preference: heavy work runs in workers with *their own* linear memory (see STACK.md), so a
  pointer is meaningless one line later and an index is valid in any heap. It also halves the size
  of every B-rep node against 64-bit pointers, and is what undo and serialisation want anyway.
- **Arena slots are never reused.** Undo restores a node into the slot it came from, so a free list
  lets a later insert take a slot an undo still needs, and the undo then silently does nothing.
  This was written, shipped and reverted within one session; the cost is that an arena grows with
  the nodes a session ever created. Do not reintroduce a free list to reclaim it — compaction with
  history cleared is the honest fix, and it is not written.
- **Bodies are immutable; every operation returns a new one.** Undo is then keeping the old handle
  rather than inverting an operation, which is the only undo a B-rep kernel can implement
  honestly — nothing can invert a boolean. `conformance` enforces it, because a backend that
  consumes its operands makes history impossible and would be found out much later otherwise.
- **A tolerance is an argument, never a constant.** Every predicate that compares takes its
  tolerance from the document, and no file outside `kernel/` invents an epsilon. Scattered
  hard-coded epsilons are the standard way a CAD kernel becomes unfixable.
- **Relaxed SIMD is forbidden in kernel predicates.** It is welcome in tessellation, transforms and
  anything feeding the GPU. In an orientation test or an intersection it is poison: results are
  *permitted* to differ between machines, so one document would produce different topology on
  different hardware and the regression fixtures would stop meaning anything. The rule is
  per-crate, not per-call — `kernel/` is built without it.
- **The kernel does not move to the GPU.** WGSL has no `f64` and no exact predicates. Booleans,
  intersections and anything whose correctness is numerical stay on the CPU, permanently, and no
  benchmark is an argument against this.
- **wasm32 only.** wasm64 costs between 10% and over 100% on bounds checks, is Tier 3 in Rust, and
  buys an address space this architecture does not need. Revisit only when *both* the hardware
  bounds-check support lands and a real document is measured against the 4 GB ceiling — and record
  the measurement, not the intuition.
- **`unsafe` lives in exactly one crate, and that crate is the FFI boundary.** The workspace
  *forbids* it, and `forbid` cannot be relaxed by an `#[allow]` in a file — so `w3d-kernel-occt`
  opts out of the shared lint in its own `Cargo.toml`, a visible act in a reviewed file rather than
  an attribute buried in a module. A second crate wanting `unsafe` is a design conversation.
- **The C ABI header is the specification, not a convenience.** Every symbol
  `kernel-occt/native/w3d_occt.h` declares is one an Emscripten build must keep alive through
  `EMSCRIPTEN_KEEPALIVE`: an entry point for each trait method that crosses, plus the context and
  the buffer frees that C requires and Rust does not. Declare there first, then implement in C++,
  then use from Rust — and never widen it to "expose a bit more of OCCT".
- **A capability probe cannot certify a driver.** What the machine can do is read from the adapter,
  never inferred from `cfg!` — but that is the first of three ways this goes wrong, and the other
  two cost more. A backend that is not compiled in cannot be reported at all: wgpu's `gles` feature
  is the *native* GL backend and does nothing on wasm32, so asking for it instead of `webgl`
  compiles, type-checks and ships with no fallback — which is why `make wasm` greps the tree for
  `glow`. And a driver can answer every question correctly and still draw nothing: headless
  Chromium reports WebGPU, hands back an adapter with compute shaders and a gigabyte of buffer, and
  rasterises a black canvas with no error anywhere. **The last word therefore belongs to whoever
  can look at the result** — `web/`'s loader renders a frame, counts the colours on the canvas, and
  falls back to WebGL2 from evidence rather than from a feature flag. A degradation nobody can name
  is a bug report about performance six months later.
- **Dependencies are declared per target, `default-features = false`, backend list spelled out.**
  The enabled backend list *is* the platform decision in STACK.md, and a default-features build
  hides it in a lockfile. The licence rule below is not the only reason to read a feature table.

## Testing

`make test` is the whole of it, and it is three different kinds of check.

| Command | What it proves |
| --- | --- |
| `make check` | Every crate compiles, tests and all. |
| `make clippy` | `-D warnings`, no allow-list. A lint that has to be silenced gets an argument in a record file. |
| `make licences` | Every crate on **both** targets is compatible with GPL-3.0-or-later, against the allowlist in `tools/licences.py`; an unlisted licence fails the build. It runs its own negative controls first, because a checker that cannot fail is a checker that says yes. |
| `cargo test --workspace` | Document, history, selection and the arena's identity rules — against `w3d-kernel-fake`, with no OCCT, no browser and no `.wasm` anywhere. |
| `w3d_kernel::conformance` | One suite, against *every* backend, `FakeKernel` included. A backend is only a backend if it passes, and this is what keeps the kernel decision reversible. |
| `w3d_format` round-trip | A saved document keeps its nodes, names, visibility, tolerance, quality and shared bodies, and refuses a file written by another kernel — both directions. The container is asserted to be a zip a standard tool can open. |
| `make wasm` | The default members build for `wasm32-unknown-unknown`, **and the WebGL2 backend is really in the tree** — the second half because asking wgpu for `gles` instead of `webgl` compiles cleanly and ships no fallback at all. |
| `make test-occt` | The conformance suite against **real geometry**, the document driven by OpenCASCADE, real geometry through the real viewport with a click that names a face, and STEP: bytes asserted to be a part-21 file *by inspection*, read back by a kernel that never saw the geometry. It also runs clippy over this crate, which `make clippy` cannot reach — being outside `default-members` is what keeps `make test` free of setup, and it is also what kept the FFI crate unlinted until somebody looked. Outside `make test` because it needs OCCT installed. |
| `make app-test` | The modeller **in a real window**: `xvfb-run`, thirty frames, and a screenshot in which chrome and viewport are checked *separately* — a run where egui drew and the scene did not looks identical in a colour count. Carries its own negative controls. Needs `xvfb`, a rasteriser, `libxkbcommon-x11-0`. |
| `make step-check` | STEP against something that is not us, in both directions: a file this program wrote, read by a pure-Python part-21 parser that shares no code with OCCT — every reference resolved, faces counted by surface type, so *the hole in the plate is asserted to be in the file* — and files from Pro/ENGINEER, Siemens NX and STEP Tools imported through the real reader, one of which must be refused because it is a surface model. Needs `make step-samples` first, which fetches them pinned by SHA-256 and commits nothing. **What it does not do is prove another program can open ours**; nothing that could is installable, and the register says so. |
| `make freecad-check` | Another *program* opens a file this one wrote and weighs each solid against **arithmetic** — 16000 − π·6²·10 mm³ for the plate, because that is what a cylinder is. It is what separates "it imported" from "it imported the right thing, at the right size, in the right unit". It is **not** a second geometry kernel: FreeCAD's is OpenCASCADE, so what this covers is another application's import path — XDE, units, its document model. Needs FreeCAD, which is in Ubuntu 22.04 and absent from 24.04. |
| `make app-test-step` | A STEP file written by one process and drawn by another, both of them the modeller. The kernel tests prove a kernel can read what a kernel wrote; this is the only place the claim is about the *program*. Needs OCCT **and** a display, so it is neither `make test` nor `make app-test`. |
| `make web-test` | The viewport **in a real browser**: WebGPU offered, WebGL2 forced, and a run with no COOP/COEP that must degrade visibly. The only check here that is not native. Needs `npm install` in `web/test/`, so it is outside `make test`. |
| `make web` | Not a check. Builds `web/dist/` — needs `wasm-bindgen-cli` at the `wasm-bindgen` dependency's version; a mismatch is a runtime error about an unknown import, not a build failure. |
| `make occt-headers` | Not a check. Fetches headers a distribution failed to ship, at the revision in `kernel-occt/native/UPSTREAM` — needed on Ubuntu Noble. Deliberately not run from `build.rs`: a build that reaches the network on its own is not reproducible. |

**Where these run.** [`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs every row above
except the browser on each push — the whole point of CI here is the checks that need something
installed, since `make test` is the one anybody can run anyway. The browser is
[nightly](.github/workflows/nightly.yml) and on demand. Only `actions/*` are used: a third-party
action is a dependency with a licence and a supply chain, and widening that list is a decision that
gets an argument in a record file rather than an edit.

**What is not covered is not listed here.** A green run is not evidence the viewport works, and the
gaps — no GPU and no browser in CI, WebGPU never having rendered a pixel, no degenerate fixtures,
no Emscripten build of OCCT — are named in the live register with what each would cost. This table
says what passing means; the register says what it does not, and only one of the two should have to
be kept in step with reality.

Three habits behind the table:

- **Keep the window out of what can be tested without one.** `w3d-app` is split into an editor with
  no window and no GPU in it — commands, selection, the rule that a drag is not a click — and a
  shell that is winit, egui and a surface. A winit event loop cannot be driven from `cargo test`
  and a state machine can, so everything that could be wrong and could be caught belongs on the
  first side. The shell is thin by construction, and `make app-test` covers what is left.
- **Prefer extending `w3d-kernel-fake` over mocking `core`.** A test that stubs the document proves
  nothing; one that stubs the kernel proves the whole modeller.
- **A check that belongs to *the contract* goes in `kernel/src/conformance.rs`**, not in a test
  file. Everything there has to be true of any correct kernel — which rules out most interesting
  assertions, and is the discipline that makes the ones left over mean something.

## The file format

`FORMAT.md` is the specification, and it is what makes the format open — not the container.
`format/` is one implementation of it, and a reader written from that page alone must be able to
read what this one writes.

One rule matters more than the rest, and it is the seam wearing a different hat: **the geometry
blobs are the writing kernel's own bytes, and the manifest records which kernel wrote them.** A
build whose kernel does not match must refuse the file by name. It must not convert, and it must
not open the document with the geometry missing.

> A native file that silently half-converts is the worst outcome a format can have. A file that
> will not open is a problem you can see.

Moving geometry *between* kernels is what STEP is for: a different operation, with a different name
in the interface, lossy in ways a user should be asked to accept. It is
`GeometryKernel::export_step` and `import_step` — **named for STEP, not for "an exchange format"**,
because what a caller has to know is what *this* format costs, and an abstraction over a single
case hides the paragraph that says so. Three rules came with them and are held by the conformance
suite:

- **Both directions, or neither.** A backend may honestly not do STEP and answers `Unsupported`
  from both. One direction only is a door that opens outwards, or a user's work going in and not
  coming out.
- **`Unsupported` means the build cannot do STEP.** Bytes that are not a STEP file are `Failed`.
  Two different sentences, and they send a user to two different places.
- **The file states its unit, because the document has none.** A number in a document is just a
  number; exporting is where somebody decides what it meant. Millimetres, both ways.

The checks for all three are conditional and **none of them skips** — every branch asserts
something, because a skipped check reads as a passed one on the backend it was written for.

- **`GeometryKernel::geometry_format` is a promise about bytes on somebody's disk.** Changing what
  `save_body` produces means changing that string, and a backend that does not is breaking every
  file anyone has saved.
- **The round-trip is a conformance check**, not a backend test. "Saving and loading a body keeps
  its topology and its bounds" is true of any correct kernel, so it lives in
  `kernel/src/conformance.rs` with everything else that is.

## Licensing

**This repository is GPL-3.0-or-later**, and the reasoning is in the 2026-08-25 record file under
`Extension · the licence and the first backend`. The short version:

- **OpenCASCADE is LGPL-2.1-*only*.** Its §3 carries the permission to apply "the ordinary GNU
  General Public License" to a copy, of any version — the clause anticipates versions later than 2
  explicitly. That is the route by which an LGPL-2.1-only kernel lives inside a GPL-3 work, and the
  whole of why this licence was available to choose.
- **The `OCCT-exception-1.0` is not what it is usually said to be**, and this repository does not
  rely on it. It covers exactly one thing: header material — inlines and templates — incorporated
  into object code, so LGPL does not leak into proprietary callers through C++ headers. It does
  **not** waive LGPL-2.1 §6, the right to relink against a modified library. A wasm build is static
  linking, so a closed distribution would have owed §6 a real answer. GPL-3 owes it nothing,
  because the source ships.
- **Modifications to OCCT are OCCT's**, whatever this repository is licensed as. Emscripten port
  patches live in their own series and are published under LGPL-2.1.
- **Parasolid**, which is what actually makes Plasticity good, is a commercial Siemens licence with
  no WebAssembly distribution. It is not an option here; the gap it leaves is the whole difficulty
  of this project, and should be named as such rather than wished away.

What this forecloses, deliberately: a closed binary sold under a perpetual licence, which is
Plasticity's own model. Relicensing later would mean removing OCCT.

Serving the `.wasm` to a browser is **distribution** — GPL-3, not AGPL, and the SaaS distinction
neither saves us nor is needed. Ship a link to the corresponding source alongside the build.

Do not add a dependency whose licence is incompatible with GPL-3.0-or-later. **`make licences`
enforces this** — it is part of `make test`, it reads both targets because the dependency sets
differ, and it fails on any licence not on the allowlist. Widening that list is a decision that
gets an argument in a record file, not an edit.

Two consequences of the tree as it stands, worth knowing before they surprise someone:

- **Two crates are Apache-2.0-only** (`codespan-reporting`, `spirv`, both reached through `naga`).
  Apache-2.0 is compatible with GPL-**3** and not with GPL-2, so the licence chosen for the
  kernel's sake is also the one the renderer's dependencies require. There is no version of this
  project that is GPL-2.
- **`cargo metadata` cannot see everything**, and the omissions are the interesting ones: OCCT
  itself, the header `make occt-headers` fetches, Playwright, and the browsers and drivers a test
  run needs. `tools/licences.py` lists them in its report rather than leaving them out.

## Temporary files and scripts

Use `.tmp/` in the project root; scripts in `.tmp/scripts/`. Both are gitignored.
