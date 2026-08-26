#!/usr/bin/env python3
"""Reads a STEP file with something that is not OpenCASCADE.

`make test-occt` proves that a kernel can read back what a kernel wrote, and
that is the weakest interesting claim there is: one library agreeing with
itself. This is the other half — a reader that shares no line of code with
OCCT opens the file and is asked what is in it.

**It is not a full second implementation and does not pretend to be.**
`steputils` is a pure-Python ISO 10303-21 parser: it gives back instances,
their types and their parameters, and nothing about geometry. Every geometric
question below — do the references resolve, is each solid reachable from a
shape representation, how many faces of each surface type are there — is this
script's own reading of part 21, deliberately, because a parser that also
interpreted the geometry would be one opinion arriving twice.

What it cannot do is tell you whether Fusion or SolidWorks will open the file.
Nothing available here can; that stays owed, and the register says so.

    pip install steputils
    python3 tools/step_check.py FILE --solids 2 \\
        --surfaces PLANE=6,CYLINDRICAL_SURFACE=1,SPHERICAL_SURFACE=1
"""

import sys
from pathlib import Path

try:
    from steputils import p21
except ImportError:
    raise SystemExit(
        "steputils is not installed, and it is the whole point of this check:\n"
        "  pip install steputils\n"
        "It is a dev-time tool, MIT, and nothing distributed links to it."
    )


class Rejected(Exception):
    """The file is not what it claims to be, with a reason."""


def instances(step):
    """Every instance in the data sections, by reference."""
    found = {}
    for section in step.data:
        found.update(section.instances)
    return found


def entities_of(instance):
    """A simple instance has one entity; a complex one has several.

    `#430 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );` is one
    instance and three entities, and a checker that only understood the simple
    form would silently see no units at all.
    """
    if isinstance(instance, p21.ComplexEntityInstance):
        return list(instance.entities)
    return [instance.entity]


def references(value):
    """Every `#n` reachable inside a parameter, at any depth."""
    if p21.is_reference(value):
        yield str(value)
    elif isinstance(value, (p21.ParameterList, list, tuple)) and not isinstance(value, str):
        for item in value:
            yield from references(item)


def outgoing(instance):
    for entity in entities_of(instance):
        for param in entity.params:
            yield from references(param)


def inventory(by_ref):
    counts = {}
    for instance in by_ref.values():
        for entity in entities_of(instance):
            counts[entity.name] = counts.get(entity.name, 0) + 1
    return counts


# ---- the checks -----------------------------------------------------------


def check_references_resolve(by_ref):
    """Every `#n` names an instance that is in the file.

    The one check OCCT reading its own output can never fail: a truncated
    file, a lost instance, a writer that renumbered halfway. It is also the
    check most likely to be vacuous, which is why the self-test below builds a
    file with a dangling reference and requires this to reject it.
    """
    for ref, instance in by_ref.items():
        for target in outgoing(instance):
            if target not in by_ref:
                raise Rejected(f"{ref} refers to {target}, which is not in the file")


def check_header(step, originator):
    schema = step.header.get("FILE_SCHEMA")
    if schema is None:
        raise Rejected("no FILE_SCHEMA: nothing can know what this file is")
    text = str(schema.params)
    if "AUTOMOTIVE_DESIGN" not in text and "AP214" not in text:
        raise Rejected(f"the schema is not AP214: {text}")
    name = step.header.get("FILE_NAME")
    if name is None:
        raise Rejected("no FILE_NAME")
    if originator and originator not in str(name.params):
        raise Rejected(
            f"nothing in FILE_NAME names {originator!r}, so a file that opens "
            f"badly somewhere names no program to ask: {name.params}"
        )


def check_solids(by_ref, expected):
    """Solids exist, are closed, and hang off a shape representation.

    A `MANIFOLD_SOLID_BREP` that nothing points at is in the file and not in
    the model, and a receiving program is entitled to ignore it — which is
    exactly the failure that looks fine in a byte count and in a round-trip
    through the library that wrote it.
    """
    counts = inventory(by_ref)
    solids = counts.get("MANIFOLD_SOLID_BREP", 0)
    if solids != expected:
        raise Rejected(f"{solids} solids in the file, expected {expected}")
    shells = counts.get("CLOSED_SHELL", 0)
    if shells < solids:
        raise Rejected(f"{solids} solids over {shells} closed shells")

    roots = [
        ref
        for ref, instance in by_ref.items()
        if any(
            e.name in ("ADVANCED_BREP_SHAPE_REPRESENTATION", "MANIFOLD_SURFACE_SHAPE_REPRESENTATION")
            for e in entities_of(instance)
        )
    ]
    if not roots:
        raise Rejected("no shape representation: the solids are in the file, not in the model")

    seen, queue = set(), list(roots)
    while queue:
        ref = queue.pop()
        if ref in seen:
            continue
        seen.add(ref)
        queue.extend(outgoing(by_ref[ref]))

    for ref, instance in by_ref.items():
        if any(e.name == "MANIFOLD_SOLID_BREP" for e in entities_of(instance)) and ref not in seen:
            raise Rejected(f"the solid {ref} is not reachable from any shape representation")


def check_surfaces(by_ref, expected):
    """The geometry is the geometry, counted by an outsider.

    `PLANE=6,CYLINDRICAL_SURFACE=1` for a drilled plate is the assertion that
    the *hole is in the file*, and it is one nothing inside this repository can
    make about its own output.
    """
    counts = inventory(by_ref)
    for name, want in expected.items():
        got = counts.get(name, 0)
        if got != want:
            raise Rejected(f"{got} {name}, expected {want}")


def check_length_unit(by_ref):
    units = set()
    for instance in by_ref.values():
        names = {e.name for e in entities_of(instance)}
        if "LENGTH_UNIT" not in names:
            continue
        for entity in entities_of(instance):
            if entity.name == "SI_UNIT":
                units.add(tuple(str(p) for p in entity.params))
    if not units:
        raise Rejected("no length unit: the file does not say what its numbers mean")
    if units != {(".MILLI.", ".METRE.")}:
        raise Rejected(f"the length unit is not millimetres: {sorted(units)}")


def read(text):
    """Parse, and turn every way a parser can object into one exception.

    `steputils` raises its own errors, and a lexer given prose raises whatever
    a lexer given prose raises. All of it means the same thing here — this is
    not a STEP file — and funnelling it into `Rejected` is what lets every
    caller below catch one type and mean it.
    """
    try:
        step = p21.loads(text)
    except Exception as e:  # noqa: BLE001 — see the docstring
        raise Rejected(f"did not parse: {type(e).__name__}: {e}") from e
    by_ref = instances(step)
    if not by_ref:
        raise Rejected("no instances in the data section")
    check_references_resolve(by_ref)
    return step, by_ref


# ---- controls -------------------------------------------------------------


HEAD = (
    "ISO-10303-21;\nHEADER;\n"
    "FILE_DESCRIPTION((''),'2;1');\n"
    "FILE_NAME('t','2026-01-01T00:00:00',(''),(''),'','3dworld','');\n"
    "FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n"
)
TAIL = "ENDSEC;\nEND-ISO-10303-21;\n"


def self_test():
    """A checker that cannot fail is a checker that says yes.

    Four controls, and the third is the one that matters: the reference walk
    has to *reject* a file whose references do not resolve, or every real run
    below is a green light nobody earned.
    """
    wrong = 0

    def expect_rejected(what, text):
        nonlocal wrong
        try:
            read(text)
        except Rejected:
            return
        print(f"  SELF-TEST FAILED: {what} was accepted")
        wrong += 1

    expect_rejected("prose", "this is not a STEP file, and never was one")
    expect_rejected("a truncated file", HEAD + "#1 = CARTESIAN_POINT('',(0.,0.,")
    expect_rejected(
        "a dangling reference",
        HEAD + "#1 = VERTEX_POINT('',#2);\n" + TAIL,
    )
    try:
        read(HEAD + "#1 = VERTEX_POINT('',#2);\n#2 = CARTESIAN_POINT('',(0.,0.,0.));\n" + TAIL)
    except Rejected as e:
        print(f"  SELF-TEST FAILED: a well-formed file was rejected: {e}")
        wrong += 1

    if wrong:
        print("the checker is broken; its verdict below means nothing.")
    return wrong


def describe(path):
    """What a foreign file is, according to a parser that is not OCCT.

    Run over the fetched samples before they are imported, so that the check
    that follows is against a file somebody can see the provenance of — and so
    that the reference walk is pointed at files this repository did not write,
    where it is a real question rather than a formality.
    """
    try:
        step, by_ref = read(path.read_text(encoding="ascii", errors="replace"))
    except Rejected as e:
        print(f"FAIL  {path.name}: {e}")
        return 1
    name = step.header.get("FILE_NAME")
    producer = "unknown"
    if name is not None and len(name.params) > 5:
        producer = str(name.params[5]) or str(name.params[4])
    schema = step.header.get("FILE_SCHEMA")
    counts = inventory(by_ref)
    print(
        f"{path.name}: {len(by_ref)} instances · "
        f"{counts.get('MANIFOLD_SOLID_BREP', 0)} solids · "
        f"written by {producer.strip() or 'unknown'} · "
        f"{str(schema.params).strip('()') if schema else 'no schema'}"
    )
    return 0


def parse_surfaces(spec):
    if not spec:
        return {}
    out = {}
    for pair in spec.split(","):
        name, _, count = pair.partition("=")
        out[name.strip()] = int(count)
    return out


def main(argv):
    if self_test():
        return 2

    args = list(argv)
    if args[0] == "--describe":
        files = [Path(p) for p in args[1:]]
        if not files:
            raise SystemExit("--describe needs at least one file")
        return max(describe(f) for f in files)

    path = Path(args.pop(0))
    solids, surfaces, originator = None, {}, "3dworld"
    while args:
        flag = args.pop(0)
        if flag == "--solids":
            solids = int(args.pop(0))
        elif flag == "--surfaces":
            surfaces = parse_surfaces(args.pop(0))
        elif flag == "--originator":
            originator = args.pop(0)
        else:
            raise SystemExit(f"unknown argument: {flag}")

    # Strict ASCII, and that is a check rather than an inconvenience: part 21
    # says so, and a file of ours with a stray byte in it is a file somebody
    # else's reader is entitled to reject.
    try:
        text = path.read_text(encoding="ascii", errors="strict")
    except UnicodeDecodeError as e:
        print(f"FAIL  {path.name} is not ASCII, which part 21 requires: {e}")
        return 1

    # One more control, on the file actually being checked rather than on a
    # synthetic one: half of it must not pass. A checker that accepts half a
    # file has not read the half it was given either.
    try:
        read(text[: len(text) // 2])
        print("  SELF-TEST FAILED: half of this file was accepted as whole")
        return 2
    except Rejected:
        pass

    try:
        step, by_ref = read(text)
        check_header(step, originator)
        if solids is not None:
            check_solids(by_ref, solids)
        check_surfaces(by_ref, surfaces)
        check_length_unit(by_ref)
    except Rejected as e:
        print(f"FAIL  {path.name}: {e}")
        return 1

    counts = inventory(by_ref)
    interesting = sorted(
        (n, c) for n, c in counts.items() if n.endswith(("_SURFACE", "SOLID_BREP", "PLANE"))
    )
    print(f"{path.name}: {len(by_ref)} instances · " + " · ".join(f"{n} {c}" for n, c in interesting))
    print("ok    a parser that is not OpenCASCADE read it, and the solids are in the model")
    return 0


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    sys.exit(main(sys.argv[1:]))
