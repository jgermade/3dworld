# The whole of `make test`, and it is three different kinds of check.
CARGO ?= cargo
WASM_TARGET := wasm32-unknown-unknown

.PHONY: test check clippy fmt fmt-check wasm doc clean

## Everything CI runs, and everything a contributor should run before pushing.
## Needs no setup: the OpenCASCADE backend is not a default workspace member.
test: check clippy fmt-check
	$(CARGO) test

## The implementation compiles, and so does every crate on its own.
check:
	$(CARGO) check --all-targets

## The OpenCASCADE backend, and the conformance suite run against real
## geometry. Needs OCCT headers and libraries:
##   apt install libocct-foundation-dev libocct-modeling-data-dev \
##               libocct-modeling-algorithms-dev
## Override discovery with OCCT_INCLUDE_DIR / OCCT_LIB_DIR.
.PHONY: test-occt
test-occt:
	$(CARGO) test -p w3d-kernel-occt

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

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

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
	$(CARGO) check --target $(WASM_TARGET)
	@$(CARGO) tree -p w3d-render --target $(WASM_TARGET) -e normal \
	    | grep -q glow \
	    || (echo "the wasm build has no WebGL2 fallback: glow is not in the \
tree. Check render/Cargo.toml's wgpu features."; exit 1)

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
