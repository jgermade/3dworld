# The whole of `make test`, and it is three different kinds of check.
CARGO ?= cargo
WASM_TARGET := wasm32-unknown-unknown

.PHONY: test check clippy fmt fmt-check wasm doc clean

## Everything CI runs, and everything a contributor should run before pushing.
## Needs no setup: the OpenCASCADE backend is not a default workspace member.
test: check clippy fmt-check licences notice-check
	$(CARGO) test
	@# The same tests again with `parallel` on, which is the only way one
	@# fingerprint can hold two builds to the same mesh. A feature nothing
	@# runs is a feature that compiles.
	$(CARGO) test -p w3d-kernel-truck --features parallel

## The implementation compiles, and so does every crate on its own.
check:
	$(CARGO) check --all-targets
	@# Feature combinations are their own build: `parallel` swaps the body of
	@# `tessellate`, so a `cargo check` of the default features says nothing
	@# about it.
	$(CARGO) check -p w3d-kernel-truck --features parallel --all-targets

## The OpenCASCADE backend, and the conformance suite run against real
## geometry. Needs OCCT headers and libraries:
##   apt install libocct-foundation-dev libocct-modeling-data-dev \
##               libocct-modeling-algorithms-dev libocct-data-exchange-dev
## Override discovery with OCCT_INCLUDE_DIR / OCCT_LIB_DIR.
.PHONY: test-occt
test-occt:
	$(CARGO) test -p w3d-kernel-occt -- --test-threads=1
	@# `make clippy` cannot reach this crate: it is outside default-members,
	@# which is what keeps `make test` free of setup. So it is linted here, by
	@# the one command that already needs OCCT installed — a crate nothing
	@# lints is a crate whose lints are all still there, and the first run of
	@# this found one.
	$(CARGO) clippy -p w3d-kernel-occt --all-targets -- -D warnings

## Fetches the headers a distribution failed to ship, at the revision in
## kernel-occt/native/UPSTREAM. Needed on Ubuntu Noble, whose
## libocct-foundation-dev is missing NCollection_AliasedArray.hxx — see the
## note there. Nothing is committed: the files are OCCT's, LGPL-2.1.
##
## Separate from the build on purpose. A build script that fetches from the
## network on its own is not a build anybody can reproduce.
OCCT_TAG ?= V7_6_3
OCCT_RAW := https://raw.githubusercontent.com/Open-Cascade-SAS/OCCT/$(OCCT_TAG)/src
VENDOR := kernel-occt/native/vendor-include

.PHONY: occt-headers
occt-headers:
	mkdir -p $(VENDOR)
	curl -sSfL -o $(VENDOR)/NCollection_AliasedArray.hxx \
	    $(OCCT_RAW)/NCollection/NCollection_AliasedArray.hxx
	@echo "fetched into $(VENDOR) at $(OCCT_TAG); these files are OCCT's, LGPL-2.1"

## Every crate this project links is compatible with GPL-3.0-or-later, which
## AGENTS.md has required since the licence was settled and nothing checked
## until there was a check. Reads both targets, because the dependency sets
## differ. Carries its own negative controls and runs them first.
.PHONY: licences notice notice-check benchmark-step
licences:
	python3 tools/licences.py

notice:
	python3 tools/notice.py

notice-check:
	python3 tools/notice.py --check

benchmark-step:
	python3 tools/benchmark_step.py

clippy:
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) clippy -p w3d-kernel-truck --features parallel --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

## The seam survives the target it exists for: `w3d-core` and the kernel
## contract must build for wasm with no host assumption having crept in. It is
## still only a type-check — nothing is instantiated, because there is no
## `web/` yet.
##
## The second command is not one. The WebGL2 fallback is one feature name away
## from not being in the build at all, and asking for the wrong one still
## compiles: `gles` is the *native* GL backend and does nothing on wasm32.
## `glow` in the dependency tree is the evidence that the fallback is there.
wasm:
	rustup target add $(WASM_TARGET)
	@# `w3d-app` is the *desktop* shell — winit and a native window — and does
	@# not build for wasm by design; the browser's shell is `w3d-web`. It is
	@# excluded by name rather than dropped from default-members, so that
	@# `make test` still runs the editor's tests.
	$(CARGO) check --target $(WASM_TARGET) --workspace \
	    --exclude w3d-app --exclude w3d-kernel-occt
	@$(CARGO) tree -p w3d-render --target $(WASM_TARGET) -e normal \
	    | grep -q glow \
	    || (echo "the wasm build has no WebGL2 fallback: glow is not in the \
tree. Check render/Cargo.toml's wgpu features."; exit 1)

## The browser build. `wasm-bindgen` is a separate tool, not a crate: install
## it with `cargo install wasm-bindgen-cli --version <the wasm-bindgen dep's
## version>`, and keep the two in step — a mismatch is a runtime error about
## an unknown import, not a build failure.
##
## Output goes to web/dist/, which is gitignored. Nothing built is committed.
WASM_OUT := web/dist

## What the threaded variant needs, in one place because it is long and because
## every flag in it is explained above `web-threaded` rather than in a comment
## nobody reads at the end of a line.
THREAD_RUSTFLAGS := -C target-feature=+atomics,+bulk-memory,+mutable-globals \
  -C link-arg=--shared-memory \
  -C link-arg=--import-memory \
  -C link-arg=--max-memory=4294967296 \
  -C link-arg=--export=__wasm_init_tls \
  -C link-arg=--export=__tls_base \
  -C link-arg=--export=__tls_size \
  -C link-arg=--export=__tls_align

.PHONY: web web-threaded web-both web-opt web-serve web-test app app-test
web:
	rustup target add $(WASM_TARGET)
	$(CARGO) build -p w3d-web --release --target $(WASM_TARGET)
	wasm-bindgen --target web --no-typescript --out-dir $(WASM_OUT) \
	    target/$(WASM_TARGET)/release/w3d_web.wasm
	@ls -l $(WASM_OUT)/w3d_web_bg.wasm | awk '{printf "wasm:     %.2f MiB\n", $$5/1048576}'
	@gzip -9 -c $(WASM_OUT)/w3d_web_bg.wasm | wc -c | awk '{printf "wasm.gz:  %.2f MiB\n", $$1/1048576}'
	@brotli -9 -c $(WASM_OUT)/w3d_web_bg.wasm 2>/dev/null | wc -c | awk '{printf "wasm.br:  %.2f MiB\n", $$1/1048576}' || true

## The second entry in the build matrix: the same crate, compiled to a module
## with a shared memory and atomic instructions, meshing a solid's faces across
## a rayon pool. Into web/dist/threaded/, which is where `loader.js` looks.
##
## Four things about this command are load-bearing and none of them is obvious:
##
##   - **Nightly, and not by preference.** `-C target-feature=+atomics` is an
##     unstable target feature and `-Z build-std` is unstable outright. The
##     standard library has to be rebuilt because a prebuilt `std` was compiled
##     without atomics, and mixing the two links but does not run.
##   - **`--shared-memory` and `--import-memory` are linker flags, not target
##     features.** Enabling atomics does not make the memory shared: without
##     the first, the build succeeds and produces a module with a private
##     memory, which is the failure `tools/wasm_threads.py` exists to catch —
##     it caught it here, on the first build that got this far. Without the
##     second, wasm-bindgen refuses, which is the kinder of the two.
##   - **The TLS symbols are exported by name.** `wasm-bindgen` needs
##     `__wasm_init_tls` to give each worker its own thread locals, and the
##     linker garbage-collects it: it is synthetic, nothing in the module calls
##     it, and only an explicit `--export` keeps it alive. Removing any of
##     these four gets "failed to find `__wasm_init_tls`" from a tool that has
##     already spent a minute linking.
##   - **`--split-linked-modules`** puts wasm-bindgen-rayon's worker bootstrap
##     in `snippets/` as a real file. Inlined, a worker would be started from a
##     blob: URL, which is a different origin and so not cross-origin isolated,
##     and the shared memory would be refused at the last possible moment.
##     That file then needs one line rewritten before a web server can serve
##     what it asks for; `tools/rayon_worker_entry.py` says why.
##
## Not part of `make web`, and not in `make test`: it needs a nightly
## toolchain and rebuilds `std`, which is a minute of work and a setup
## requirement. `make web-both` is the pair.
web-threaded:
	rustup target add --toolchain nightly $(WASM_TARGET)
	RUSTFLAGS="$(THREAD_RUSTFLAGS)" $(CARGO) +nightly build -p w3d-web --release \
	    --features threads --target $(WASM_TARGET) -Z build-std=std,panic_abort
	wasm-bindgen --target web --no-typescript --split-linked-modules \
	    --out-dir $(WASM_OUT)/threaded target/$(WASM_TARGET)/release/w3d_web.wasm
	@# rayon's workers resolve the main module as a *directory*, which only a
	@# bundler can serve. Left alone it is a page that hangs rather than one
	@# that fails — see the file.
	python3 tools/rayon_worker_entry.py $(WASM_OUT)/threaded w3d_web.js
	@# The build said yes; this asks the artifact. See the file for why the
	@# two are different questions.
	python3 tools/wasm_threads.py $(WASM_OUT)/threaded/w3d_web_bg.wasm
	@ls -l $(WASM_OUT)/threaded/w3d_web_bg.wasm \
	    | awk '{printf "wasm.threaded:    %.2f MiB\n", $$5/1048576}'
	@gzip -9 -c $(WASM_OUT)/threaded/w3d_web_bg.wasm | wc -c \
	    | awk '{printf "wasm.threaded.gz: %.2f MiB\n", $$1/1048576}'

## Both entries of the matrix, which is what the loader's dispatch needs before
## it has two things to dispatch to.
web-both: web web-threaded

web-opt: web
	@which wasm-opt >/dev/null 2>&1 && ( \
	    wasm-opt -O3 $(WASM_OUT)/w3d_web_bg.wasm -o $(WASM_OUT)/w3d_web_bg.opt.wasm && \
	    ls -l $(WASM_OUT)/w3d_web_bg.opt.wasm | awk '{printf "wasm.opt:    %.2f MiB\n", $$5/1048576}' && \
	    gzip -9 -c $(WASM_OUT)/w3d_web_bg.opt.wasm | wc -c | awk '{printf "wasm.opt.gz: %.2f MiB\n", $$1/1048576}' && \
	    (brotli -9 -c $(WASM_OUT)/w3d_web_bg.opt.wasm 2>/dev/null | wc -c | awk '{printf "wasm.opt.br: %.2f MiB\n", $$1/1048576}' || true) \
	) || echo "wasm-opt not installed on host; reported raw & compressed wasm metrics above"

## Serves web/ with COOP/COEP. `--no-isolation` omits them, which is the case
## worth seeing: the loader must degrade visibly rather than fail obscurely.
web-serve: web
	python3 web/serve.py

## The modeller, in a real window. Needs `xvfb-run` and a rasteriser:
##   apt install xvfb mesa-vulkan-drivers libxkbcommon-x11-0
## `--features occt` swaps the fake kernel for OpenCASCADE.
app:
	$(CARGO) build -p w3d-app

app-test: app
	python3 tools/app_smoke.py

## The STEP path end to end, through the program rather than through a test
## harness: one process exports, a second imports, and the second one's frame
## is asserted to contain a scene. Needs OCCT *and* a display, which is why it
## is neither `make test` nor `make app-test`.
.PHONY: app-test-step
app-test-step:
	$(CARGO) build -p w3d-app --features occt
	python3 tools/app_smoke.py --step

## STEP, against something that is not us. Everything else about STEP in this
## repository is OpenCASCADE agreeing with OpenCASCADE.
##
## Two directions, and neither of them is a round trip:
##   - a file we wrote, read by a parser that shares no code with OCCT
##     (`pip install steputils`), which resolves every reference and counts the
##     faces by surface type — so the hole in the plate is asserted to be in
##     the *file*;
##   - files we did not write, from Pro/ENGINEER, Siemens NX and STEP Tools,
##     imported through the real reader. One of them must be refused: it is a
##     surface model, which is a legitimate STEP file and not a thing a
##     modeller for solids can hold.
##
## Needs OCCT, steputils, and `make step-samples` having been run.
STEP_OUT := .tmp/ours.step

.PHONY: step-check step-samples
step-check:
	@mkdir -p $(dir $(STEP_OUT))
	$(CARGO) run -q -p w3d-kernel-occt --example export_step -- $(STEP_OUT)
	python3 tools/step_check.py $(STEP_OUT) --solids 2 \
	    --surfaces PLANE=6,CYLINDRICAL_SURFACE=1,SPHERICAL_SURFACE=1
	python3 tools/step_check.py --describe `python3 tools/step_samples.py --list import` \
	    `python3 tools/step_samples.py --list refuse`
	$(CARGO) run -q -p w3d-kernel-occt --example import_step -- \
	    `python3 tools/step_samples.py --list import`
	$(CARGO) run -q -p w3d-kernel-occt --example import_step -- --must-refuse \
	    `python3 tools/step_samples.py --list refuse`

## Fetches those files into a gitignored samples/, pinned by SHA-256. Not run
## from any build: a build that reaches the network is not one anybody can
## reproduce. Same rule as `make occt-headers`.
step-samples:
	python3 tools/step_samples.py --fetch

## Another *program* opens a file this one wrote, and weighs what comes out
## against arithmetic — a plate of 40x40x10 with a 12 mm hole is
## 16000 - pi*6^2*10 mm3 because that is what a cylinder is, and no CAD kernel
## has a vote on it. That is the check that turns "it imported" into "it
## imported the right thing, at the right size, in the right unit".
##
## FreeCAD's kernel is OpenCASCADE, so this is another application's *import
## path* — its XDE layer, its units, its document — and not a second opinion on
## the geometry. The difference matters and is written down in the script.
##
## Needs FreeCAD (Ubuntu 22.04: apt install freecad; it is absent from 24.04).
## `FREECAD_STEP=path` weighs a file that already exists instead of writing one,
## which is how CI does it, on a machine with no OCCT and no Rust.
FREECAD_STEP ?= $(STEP_OUT)

.PHONY: freecad-check
freecad-check:
	@if [ ! -f "$(FREECAD_STEP)" ]; then 	    mkdir -p $(dir $(STEP_OUT)); 	    $(CARGO) run -q -p w3d-kernel-occt --example export_step -- $(STEP_OUT); 	fi
	python3 tools/freecad_volume.py $(FREECAD_STEP)

## Drives the built page in headless Chromium and asserts it drew and picked.
## This is the only check in the repository that runs the viewport in a real
## browser; everything else is native.
## The browser. `web-both` rather than `web`, so the threaded variant is there
## to be dispatched to: without it `browser.mjs` still runs and still asserts,
## but what it asserts is that the build is missing.
web-test: web-both
	node web/test/browser.mjs

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
