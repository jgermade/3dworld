#!/usr/bin/env python3
"""Weighs a built .wasm module, and asks it whether it was optimised.

What `make web` used to print was three lines of `ls` and `wc`, and the third
said `wasm.br: 0.00 MiB` on any machine without `brotli` — the `|| true` guarded
the pipe, not the count, so a tool that was not there reported a size of zero.
A number that is sometimes a measurement and sometimes an absence dressed as
one is worse than no number. Here an absent compressor says so.

It also reads the module's sections, because that is where the size is and
where the optimisation shows. wasm-bindgen keeps the `name` section — function
names, for stack traces — and `wasm-opt` drops it unless asked not to; on
2026-09-26 it was 1.02 MiB of a 4.58 MiB module. So a module that still has one
did not go through `wasm-opt`, and that is checked rather than trusted: the
Makefile saying it ran the tool is not evidence about the file, the same
argument as `tools/wasm_threads.py`. `--unoptimised` is for the build that
says, out loud, that it skipped the step (`WASM_OPT=none`).

Nothing is asserted about the size itself. There is no golden number for a
payload, and one would fail the day somebody adds a feature on purpose.

Usage: wasm_payload.py [--unoptimised] <file.wasm>
"""

import gzip
import shutil
import subprocess
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


SECTION_NAMES = {
    1: "type", 2: "import", 3: "function", 4: "table", 5: "memory", 6: "global",
    7: "export", 8: "start", 9: "element", 10: "code", 11: "data", 12: "datacount",
}


def sections(data):
    """(name, size) for every section, custom ones as `custom:<name>`."""
    if data[:4] != b"\0asm":
        raise SystemExit("not a wasm module")
    i = 8
    out = []
    while i < len(data):
        sid = data[i]
        size, body = leb128(data, i + 1)
        if sid == 0:
            length, at = leb128(data, body)
            name = "custom:" + data[at:at + length].decode("utf-8", "replace")
        else:
            name = SECTION_NAMES.get(sid, str(sid))
        out.append((name, size))
        i = body + size
    return out


def mib(n):
    return f"{n / 1048576:.2f} MiB"


def main():
    args = sys.argv[1:]
    unoptimised = "--unoptimised" in args
    paths = [a for a in args if not a.startswith("--")]
    if len(paths) != 1:
        raise SystemExit(__doc__)
    path = paths[0]
    data = open(path, "rb").read()

    print(f"{path}")
    print(f"  raw:     {mib(len(data))}")
    print(f"  gzip -9: {mib(len(gzip.compress(data, 9)))}")
    if shutil.which("brotli"):
        br = subprocess.run(["brotli", "-q", "11", "-c", path], capture_output=True, check=True)
        print(f"  brotli:  {mib(len(br.stdout))}")
    else:
        print("  brotli:  not measured — no `brotli` on this machine")

    found = sections(data)
    big = sorted(found, key=lambda s: -s[1])[:4]
    print("  largest sections: " + ", ".join(f"{n} {mib(s)}" for n, s in big))

    named = any(n == "custom:name" for n, _ in found)
    if unoptimised:
        print("  NOT optimised (WASM_OPT=none) — this is not the module a deployment serves")
    elif named:
        raise SystemExit(
            f"{path} still has a `name` section, so it did not go through wasm-opt. "
            "Run `make binaryen`, or build with WASM_OPT=none to say so on purpose."
        )


if __name__ == "__main__":
    main()
