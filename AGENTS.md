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

kernel/occt/                    wrapper over an Emscripten build of OpenCASCADE  ⬜
kernel/native/                  a kernel of our own, or truck                    ⬜
render/                         wgpu — WebGPU, WebGL2 fallback with no compute   ⬜
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
| `make wasm` | The workspace builds for `wasm32-unknown-unknown`. Nothing above the seam may acquire a host assumption without this failing. |

Still owed, and named so that the gap is not mistaken for coverage:

- **Fixture regression** — named solids in, golden topology out. The only defence against a
  robustness fix breaking three cases to fix one. Needs a real kernel to be worth writing.
- **`wasm-pack test --headless`** — that the thing actually instantiates under COOP/COEP with
  threads. Needs `web/` to exist.

**Prefer extending `w3d-kernel-fake` over mocking `core`.** A test that stubs the document proves
nothing; one that stubs the kernel proves the whole modeller.

And when a check belongs to *the contract* rather than to one backend, it goes in
`kernel/src/conformance.rs`, not in a test file. Everything there has to be true of any correct
kernel — which rules out most interesting assertions, and is the discipline that makes the ones
left over mean something.

## Licensing

**Undecided, and it is blocking a real choice, not a formality.** The kernel decision and the
licence decision are the same decision:

- **OpenCASCADE** is LGPL-2.1 with an exception. Usable commercially, but read the exception
  before, not after, `kernel/occt/` exists.
- **truck** is MIT/Apache-2.0 and imposes nothing.
- **Parasolid**, which is what actually makes Plasticity good, is a commercial Siemens licence
  and is not distributed as a WebAssembly build. It is not an option here; the gap it leaves is
  the whole difficulty of this project and should be named as such rather than wished away.

Do not add a dependency until this is settled, and record the settlement in a session file.

## Temporary files and scripts

Use `.tmp/` in the project root; scripts in `.tmp/scripts/`. Both are gitignored.
