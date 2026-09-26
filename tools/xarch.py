#!/usr/bin/env python3
"""Runs one measurement on two architectures and requires them to agree.

Register item 4 was that the desktop and the browser cut the same plate into
different solids — 6294 triangles against 6290. On 2026-09-21 that was traced
to an iteration order inside `truck-shapeops`: the intersection polyline is
chained through an `FxHashMap`, `rustc-hash` multiplies by a different constant
when `usize` is 32 bits wide, and the same closed loop is therefore entered at a
different vertex. Since 2026-09-26 the workspace carries that crate patched —
`vendor/truck-shapeops/PATCHED.md` — and the cut is 6292 triangles on both.

**So every row is asserted, bit for bit**: the operands as saved solids, their
bounds, their topology and their meshes at four sags; the cut's topology, its
mesh, and every number in the saved cut; and each build deterministic in
itself, run twice in two processes. Before the patch this script could only
hold the half before the boolean and report the rest.

The determinism half is not ceremony. The first version of this measurement
hashed the *saved cut*'s bytes, which vary from run to run on one machine —
`TruckKernel` writes its vertices in the iteration order of a pointer-keyed map
— and that produced one confident wrong reading before anybody checked. A
cross-architecture claim resting on a number that is not stable within an
architecture is not a claim.

What it needs: a wasm32 target (`rustup target add`, as `make wasm` does) and a
node. Deliberately not wasm-bindgen's CLI — see `tools/xarch.js`.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = "wasm32-unknown-unknown"
WASM = ROOT / "target" / TARGET / "release" / "w3d_xarch.wasm"
NATIVE = ROOT / "target" / "release" / "xarch"

def run(cmd, **kwargs):
    return subprocess.run(cmd, cwd=ROOT, check=True, text=True,
                          capture_output=True, **kwargs).stdout


def parse(text, native):
    """Rows as (index, rule, bits, value). The native side carries names; the
    wasm side cannot hand a string across, so the two are aligned by index and
    the rule is compared as a guard against a drift nobody noticed."""
    rows = []
    for line in text.strip().splitlines():
        fields = line.split("\t")
        if native:
            index, name, rule, bits, value = fields
        else:
            index, rule, bits, value = fields
            name = None
        rows.append((int(index), name, rule, bits, float(value)))
    return rows


def measure():
    print("building, native and wasm32", flush=True)
    run(["rustup", "target", "add", TARGET])
    run(["cargo", "build", "--release", "-p", "w3d-xarch", "--bin", "xarch"])
    run(["cargo", "build", "--release", "-p", "w3d-xarch", "--lib", "--target", TARGET])

    print("running each build twice, in two processes", flush=True)
    native = [parse(run([str(NATIVE)]), True) for _ in range(2)]
    wasm = [parse(run(["node", "tools/xarch.js", str(WASM)]), False) for _ in range(2)]
    return native, wasm


def determinism(label, runs):
    """Two runs of one build, in two processes. A build that is not stable in
    itself cannot say anything about another one."""
    first, second = runs
    drift = [a[1] or f"row {a[0]}" for a, b in zip(first, second) if a[3] != b[3]]
    return [f"{label} is not deterministic: {', '.join(drift)}"] if drift else []


def compare(rows_native, rows_wasm):
    """The table, and what it refuses. Returns (lines, failures)."""
    if len(rows_native) != len(rows_wasm):
        raise SystemExit(f"{len(rows_native)} rows native, {len(rows_wasm)} in wasm32 — "
                         "the two builds are not the same measurement")

    lines, failures = [], []
    width = max(len(r[1] or "") for r in rows_native)
    for (i, name, rule, bits_n, value_n), (j, _, rule_w, bits_w, value_w) in zip(
        rows_native, rows_wasm
    ):
        if (i, rule) != (j, rule_w):
            raise SystemExit(f"row {i} is {rule} natively and {rule_w} in wasm32 — "
                             "the two builds have drifted apart")
        same = bits_n == bits_w
        if rule == "exact":
            verdict = "same" if same else "DIFFER"
            if not same:
                failures.append(f"{name} must agree and does not: {value_n} against {value_w}")
        elif rule == "report":
            verdict = "same" if same else "differs, reported"
        else:
            raise SystemExit(f"row {i} carries a rule this script does not know: {rule}")
        lines.append(f"{name or '':{width}}  {value_n:>16.10g}  {value_w:>16.10g}  {verdict}")
    return lines, failures


def negative_controls():
    """Two faults, and the table has to catch each. They run *first*, and
    they run on every invocation: a comparison that cannot fail is a
    comparison that says nothing when it passes, and the way this script
    silently stops asserting is a rule name that stops matching."""
    row = lambda i, name, rule, bits, value: (i, name, rule, bits, value)
    native = [
        row(0, "a.exact", "exact", "aaaa", 1.0),
        row(1, "b.report", "report", "bbbb", 7.0),
    ]
    controls = [
        ("an exact row that differs",
         [row(0, "a.exact", "exact", "zzzz", 2.0), native[1]], 1),
        ("a report row that differs, which must NOT fail",
         [native[0], row(1, "b.report", "report", "zzzz", 9.0)], 0),
    ]
    for description, wasm, wanted in controls:
        _, failures = compare(native, wasm)
        if len(failures) != wanted:
            raise SystemExit(f"negative control failed — {description}: "
                             f"wanted {wanted} failure(s), got {len(failures)}")
    if determinism("control", [native, native]):
        raise SystemExit("negative control failed — two identical runs reported drift")
    if not determinism("control", [native, [row(0, "a.exact", "exact", "zzzz", 2.0)] + native[1:]]):
        raise SystemExit("negative control failed — a drifting run was not noticed")
    print(f"{len(controls) + 2} negative controls pass: the table can say no")


def main():
    negative_controls()
    native, wasm = measure()

    failures = determinism("native", native) + determinism("wasm32", wasm)
    lines, mismatches = compare(native[0], wasm[0])
    failures += mismatches

    print()
    print(f"{'':{max(len(r[1]) for r in native[0])}}  {'x86-64':>16}  {'wasm32':>16}")
    for line in lines:
        print(line)

    print()
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}")
        return 1
    exact = sum(1 for r in native[0] if r[2] == "exact")
    print(f"The two builds agree, bit for bit, on all {exact} asserted rows — "
          "the boolean's result included.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
