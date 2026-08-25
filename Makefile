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

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

## The seam survives the target it exists for. `w3d-core` and the kernel
## contract must build for wasm with no host assumptions; nothing here needs a
## browser, because nothing above the seam does.
wasm:
	rustup target add $(WASM_TARGET)
	$(CARGO) check --target $(WASM_TARGET)

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
