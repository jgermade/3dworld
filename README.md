# 3dworld

A B-rep modeller in the mould of [Plasticity](https://www.plasticity.xyz/): NURBS surfaces and
exact solids, direct modelling rather than parametric history, running in the browser on
WebAssembly and on the desktop from the same Rust core.

**GPL-3.0-or-later.**

**There is a modeller.** `cargo run -p w3d-app --features occt -- --demo` opens a window with a
box, a cylinder and a real OpenCASCADE difference between them: drag to orbit, middle-drag to pan,
wheel to zoom, click to select. The viewport also runs in a browser, on WebGL2, where a click
names a face.

Ctrl-S writes a `.w3d` — a zip you can open with `unzip`, specified in
[FORMAT.md](FORMAT.md) — and `--open` reads one back.

**Work leaves.** Ctrl-E writes a STEP file — AP214, millimetres — and `--import-step FILE` brings
one back in, a body per solid. It needs a kernel that does STEP: OpenCASCADE does, the fake kernel
says so and refuses. There is no file dialogue yet, so export writes beside the document and import
is a command-line option.

`make step-check` is what stops that from being OpenCASCADE agreeing with OpenCASCADE. A parser
with no OCCT in it (`pip install steputils`) resolves every reference in a file we wrote and counts
its faces by surface type — so the hole in the plate is asserted to be *in the file* — and real
files from **Pro/ENGINEER**, **Siemens NX** and **STEP Tools** go through the real reader,
including one that must be **refused**, because it is a surface model and this is a modeller for
solids. `make freecad-check` goes one step further: FreeCAD opens a file we wrote and reports the volume of
each solid, which is compared against **arithmetic** — a plate of 40×40×10 with a ⌀12 hole is
16000 − π·6²·10 mm³ because that is what a cylinder is. No CAD kernel has a vote on that number, so
agreeing with it is not two kernels agreeing with each other.

What it is *not* is a second geometry kernel: FreeCAD's is OpenCASCADE too. Another application's
import path — its XDE layer, its units, its document model — is a real thing to test and not the
thing still missing. A program built on Parasolid or ACIS saying the same would be, and none of
them runs in CI.

It is early. No edges drawn; no fillets; and the browser build has no real geometry behind it
until OCCT is built for Emscripten.

```
make test       # check, clippy -D warnings, licences, and the tests
make wasm       # the same code, built for wasm32-unknown-unknown
make test-occt  # real geometry, and a click that names a face (needs OCCT)
make web        # the browser bundle, into web/dist/ (needs wasm-bindgen-cli)
make web-threaded # the same, with threads: shared memory and a rayon pool (nightly)
make web-serve  # serve it with COOP/COEP; --no-isolation to watch it degrade
make web-test   # drive it in headless Chromium (needs npm install in web/test/)
make app-test   # open the modeller in a real window under Xvfb, and check it drew

make step-samples  # fetch STEP files other programs wrote, pinned by checksum
make step-check    # ours read by a parser that is not OCCT, and theirs by ours
make app-test-step # a STEP file out of one process and drawn by another
make freecad-check # FreeCAD opens ours and weighs it against arithmetic
```

The deployed page is on [GitHub Pages](https://jgermade.github.io/3dworld/), which serves static
files and cannot send a header. `web/coi-serviceworker.js` supplies `Cross-Origin-Opener-Policy`
and `Cross-Origin-Embedder-Policy` from a service worker instead, at the cost of one reload on a
first visit; the status line says whether isolation came from the host or from the worker.

What it buys is the threaded variant, which `make web-threaded` now builds: a module with a shared
memory and a rayon pool that meshes a solid's faces at once. The status line shows the pool's size
next to the variant's name, because a threaded build whose workers never started looks identical
from the outside otherwise.

[CI](.github/workflows/ci.yml) runs all of those on every push, because each one needs something
installed and a green run that skipped them would be worth nothing. The browser check is
[nightly](.github/workflows/nightly.yml) and on demand: it is the slowest by a distance and checks
a thing that changes rarely. **There is still no GPU in CI** — the window job runs on a software
rasteriser, which proves the pipeline and nothing about a driver.

The desktop shell needs a display and, on X11, `libxkbcommon-x11-0` — without it the process
panics inside `xkbcommon-dl` before a window exists. To run it headless:
`apt install xvfb libxkbcommon-x11-0 mesa-vulkan-drivers`.

`w3d-render`'s tests need a graphics adapter. Without one they **skip**, printing `SKIPPED:` and
the reason — so a green `make test` on a machine with no GPU is not evidence the viewport works.
Run `cargo test -p w3d-render -- --nocapture` to see which happened. A software rasteriser is
enough: `apt install mesa-vulkan-drivers` gets lavapipe.

**WebGPU has never rendered a pixel here.** The native tests run on Vulkan, and headless Chromium
reports WebGPU and then draws nothing — so the loader renders a frame, looks at the canvas, and
falls back to WebGL2 when it is blank. Everything the fast path claims is still an argument.

## Building the OpenCASCADE backend

Only needed for `make test-occt`; everything else builds with a bare Rust toolchain.

```sh
apt install libocct-foundation-dev libocct-modeling-data-dev libocct-modeling-algorithms-dev \
            libocct-data-exchange-dev
```

The last of those is STEP. Without it the build fails at the link, naming the toolkits.

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
- [FORMAT.md](FORMAT.md) — the `.w3d` file, specified well enough to implement from.
- [AGENTS.md](AGENTS.md) — conventions, and the rules that are not style.
- [RECORD/](RECORD/) — what was decided, learned and left owed, in the order it happened.
  `*.md` is open, `*.completed.md` is finished and why.
  **The plan is [`what-is-not-built-yet`](RECORD/2026-08-25_21h59.what-is-not-built-yet.md)**,
  the one open file; every register and `Next` block inside the completed ones is superseded and
  kept only for the order in which things became pending.
- [`kernel/src/lib.rs`](kernel/src/lib.rs) — the contract, and the two properties of it that
  everything above depends on.
- [`kernel-occt/native/w3d_occt.h`](kernel-occt/native/w3d_occt.h) — the C ABI, which is the
  specification of what an OpenCASCADE build must export.
