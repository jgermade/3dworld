#!/usr/bin/env python3
"""Opens a STEP file this program wrote in FreeCAD, and weighs what comes out.

Two things this is, and one it is not.

**It is another application's import path.** FreeCAD reads STEP through its own
XDE layer, applies its own unit handling and builds its own document. Files
that a low-level reader accepts and an application then rejects or mangles are
a real category, and nothing else here would catch one.

**It is arithmetic, not a second opinion.** The volumes below are what the
shapes are worth by hand — a plate of 40x40x10 with a 12 mm hole through it is
16000 - pi*6^2*10 mm^3 because that is what a cylinder is. Neither number comes
from a CAD kernel, so agreeing with them is not two kernels agreeing with each
other. It is the check that turns "it imported" into "it imported the right
thing, at the right size, in the right unit".

**It is not an independent geometry kernel.** FreeCAD's is OpenCASCADE, the
same one that wrote the file. A program built on Parasolid or ACIS saying this
is the evidence that is still missing, and the register says so.

Run it with FreeCAD's own interpreter, or with a python3 that can find its
modules:

    freecadcmd tools/freecad_volume.py -- FILE
    python3 tools/freecad_volume.py FILE
"""

import glob
import math
import sys

# What `kernel-occt/examples/export_step.rs` writes, in the order it writes it,
# and what each is worth. Change one and this stops matching, which is the
# intended amount of coupling: the file and its expected contents are one fact.
EXPECTED = [
    ("drilled plate 40x40x10, hole d=12", 40 * 40 * 10 - math.pi * 6**2 * 10),
    ("sphere r=8", 4 / 3 * math.pi * 8**3),
]

# Volumes come from exact integration over analytic surfaces, not from a mesh,
# so this is tight on purpose. A tolerance loose enough to hide a millimetre is
# a tolerance that would have passed a file in centimetres.
RELATIVE = 1.0e-6


def find_freecad():
    """Import FreeCAD, from wherever this distribution decided to put it.

    Running under `freecadcmd` it is already importable. Under a system
    python3 it is not on the path, and the directory has been three different
    things across Debian and Ubuntu releases — so this looks, and says what it
    looked at when it fails, rather than leaving somebody an ImportError.
    """
    try:
        import FreeCAD  # noqa: F401

        return
    except ImportError:
        pass

    candidates = sorted(
        set(
            glob.glob("/usr/lib/freecad*/lib*")
            + glob.glob("/usr/lib/freecad*/Mod")
            + glob.glob("/usr/share/freecad*/lib*")
            + glob.glob("/opt/freecad*/lib*")
        )
    )
    for path in candidates:
        sys.path.append(path)
    try:
        import FreeCAD  # noqa: F401
    except ImportError as e:
        raise SystemExit(
            f"FreeCAD is not importable: {e}\nLooked in: {candidates or 'nothing matched'}\n"
            "Install it (Ubuntu 22.04: apt install freecad) or run this under freecadcmd."
        )


def main(path):
    find_freecad()
    import FreeCAD
    import Part

    print(f"FreeCAD {FreeCAD.Version()[0]}.{FreeCAD.Version()[1]}.{FreeCAD.Version()[2]}")

    shape = Part.Shape()
    shape.read(path)
    solids = shape.Solids
    failures = []

    if len(solids) != len(EXPECTED):
        failures.append(f"{len(solids)} solids, expected {len(EXPECTED)}")

    # Sorted by volume, not by the order they arrive in: STEP does not promise
    # an order and this check is not about one.
    measured = sorted((s.Volume, s) for s in solids)
    for (what, want), (got, solid) in zip(sorted(EXPECTED, key=lambda e: e[1]), measured):
        if not solid.isValid():
            failures.append(f"{what}: FreeCAD says the solid is not valid")
        if not solid.isClosed():
            failures.append(f"{what}: the solid is not closed, so it encloses nothing")
        off = abs(got - want) / want
        verdict = "ok  " if off <= RELATIVE else "FAIL"
        print(f"{verdict}  {what}: {got:.6f} mm3, arithmetic says {want:.6f} ({off:.2e})")
        if off > RELATIVE:
            failures.append(f"{what}: {got:.6f} against {want:.6f}, off by {off:.2e}")

    for message in failures:
        print(f"FAIL  {message}")
    if failures:
        return 1
    print("ok    another program opened it and measured what the arithmetic says")
    return 0


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--"]
    if len(args) != 1:
        raise SystemExit(__doc__)
    sys.exit(main(args[0]))
