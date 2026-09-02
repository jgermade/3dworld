#!/usr/bin/env python3
"""Asserts that a .wasm module really has a shared memory.

The threaded variant is a build, not a flag, and every part of that build can
succeed while producing a module that cannot be threaded at all. `RUSTFLAGS`
reaching cargo is not checked by cargo; a `-C target-feature` typo is a
warning, not an error; and `wasm-bindgen` will happily process a module with a
plain memory and emit an `initThreadPool` that hands out workers which then
cannot share anything. Every one of those failures ships a page that boots,
says "threaded", and runs on one core.

So the artifact is read instead of the build being trusted. This is the same
argument as the `glow` grep in `make wasm` — asking the build system what it
did is not evidence about what came out of it.

A wasm memory is shared when the `shared` bit is set in its limits, whether the
memory is defined in the module or imported from JS. wasm-bindgen's threaded
output imports it, but which of the two it is is not this file's business to
insist on: what matters is that the module's memory is shared.

Usage: wasm_threads.py <file.wasm>
"""

import sys


def leb128(data, i):
    """An unsigned LEB128 at `i`, and the index after it."""
    result = 0
    shift = 0
    while True:
        byte = data[i]
        i += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, i
        shift += 7


def limits(data, i):
    """A limits record: `(shared, index after it)`.

    Flags are a bitfield: 0x01 is "has a maximum", 0x02 is "shared". A shared
    memory is required to declare a maximum, so 0x03 is what a threaded build
    produces and 0x02 alone is malformed — read rather than assumed, because
    reading it wrong is how this file would report success on a module it
    misparsed.
    """
    flags = data[i]
    i += 1
    _min, i = leb128(data, i)
    if flags & 0x01:
        _max, i = leb128(data, i)
    return bool(flags & 0x02), i


def name(data, i):
    length, i = leb128(data, i)
    return data[i : i + length].decode("utf-8", "replace"), i + length


def memories(data):
    """Every memory the module declares, as `(where, shared)`."""
    if data[:4] != b"\x00asm":
        raise ValueError("not a wasm module")
    found = []
    i = 8  # magic and version
    while i < len(data):
        section_id = data[i]
        i += 1
        size, i = leb128(data, i)
        end = i + size
        if section_id == 2:  # imports
            count, j = leb128(data, i)
            for _ in range(count):
                module, j = name(data, j)
                field, j = name(data, j)
                kind = data[j]
                j += 1
                if kind == 0x00:  # a function's type index
                    _, j = leb128(data, j)
                elif kind == 0x01:  # a table
                    j += 1
                    _, j = limits(data, j)
                elif kind == 0x02:  # a memory
                    shared, j = limits(data, j)
                    found.append((f"imported {module}.{field}", shared))
                elif kind == 0x03:  # a global
                    j += 2
                else:
                    raise ValueError(f"unknown import kind {kind}")
        elif section_id == 5:  # memories defined here
            count, j = leb128(data, i)
            for n in range(count):
                shared, j = limits(data, j)
                found.append((f"defined #{n}", shared))
        i = end
    return found


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    path = argv[1]
    with open(path, "rb") as handle:
        found = memories(handle.read())

    if not found:
        print(f"{path}: no memory at all — this is not a module wasm-bindgen produced")
        return 1
    for where, shared in found:
        print(f"  memory {where}: {'shared' if shared else 'NOT SHARED'}")
    if not any(shared for _, shared in found):
        print(
            f"{path}: no shared memory. The build did not get "
            "`-C target-feature=+atomics`, so `initThreadPool` would hand out "
            "workers that cannot see each other's heap."
        )
        return 1
    print(f"{path}: shared memory present")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
