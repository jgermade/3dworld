#!/usr/bin/env python3
"""The browser's OpenCASCADE, held to the desktop's.

Runs `kernel-occt/examples/occt_wasm_reference.rs` natively and
`tools/occt_wasm_check.mjs` against web/dist/occt/, and compares the rows.

Two rules, and the difference between them is the point:

- **Exact**: topology, body counts, bounds printed to nine places, and which
  operations fail. Both halves are OCCT 7.6.3 built from the same source; a
  solid with a different number of faces is a different solid, and a fillet
  that fails on one and not the other is a different kernel.
- **Report**: meshes. The desktop's OCCT is Ubuntu's build against glibc's
  libm; the browser's is Emscripten's against musl's. A sine one ulp apart can
  move a triangle, so a mesh that differs is printed, not failed. It is
  `make xarch`'s `Report` rule, and for the same reason.

And one row only the wasm half has: an OCCT exception thrown and caught inside
the module. It must come back as the shim's `failed` with OCCT's own message.
A module built without working exceptions traps there, which is a failure of
this check rather than a row that happens to be missing.

Needs OCCT installed natively (as `make test-occt` does) and the module built
(`make occt-wasm`), and Node.
"""

import os
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def rows(text):
    out = {}
    for line in text.strip().splitlines():
        name, _, value = line.partition("\t")
        out[name] = value
    return out


def main():
    native = subprocess.run(
        ["cargo", "run", "-q", "--release", "-p", "w3d-kernel-occt", "--example",
         "occt_wasm_reference"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    wasm_run = subprocess.run(
        ["node", os.path.join(ROOT, "tools", "occt_wasm_check.mjs")],
        cwd=ROOT, capture_output=True, text=True,
    )
    if wasm_run.returncode != 0:
        sys.stderr.write(wasm_run.stdout + wasm_run.stderr)
        raise SystemExit("the wasm half did not finish — a trap or a missing module, not a row")
    sys.stderr.write(wasm_run.stderr)

    a, b = rows(native), rows(wasm_run.stdout)
    failures = []
    print(f"{'row':22} {'native':>28} {'wasm32':>28}")
    for name, value in a.items():
        other = b.get(name, "(missing)")
        report = name.endswith(".mesh")
        same = value == other
        verdict = "same" if same else ("differs (reported)" if report else "DIFFERS")
        print(f"{name:22} {value:>28} {other:>28}  {verdict}")
        if not same and not report:
            failures.append(name)

    kind, _, message = b.get("exception.degenerate_polygon", "(missing)").partition("\t")
    # OCCT's own words, not one of the shim's: the shim's messages are for the
    # failures it detects itself, and this one it can only catch.
    ok = kind == "failed" and "not done" in message
    print(f"{'exception in wasm':22} {kind:>28} {message[:40]:>28}  {'caught' if ok else 'NOT CAUGHT'}")
    if not ok:
        failures.append("exception.degenerate_polygon")

    if failures:
        raise SystemExit(f"{len(failures)} row(s) disagree: {', '.join(failures)}")
    print("\nThe browser's OpenCASCADE and the desktop's agree on every exact row.")


if __name__ == "__main__":
    main()
