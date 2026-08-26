#!/usr/bin/env python3
"""Runs the modeller in a window and checks that it drew a modeller.

The desktop counterpart of `make web-test`. It needs a display, so it uses
`xvfb-run` and a software rasteriser — which proves the window, the swapchain,
the event loop, the egui integration and the present path, and proves nothing
about a real GPU or a real compositor.

Three assertions, and the second is the one worth having: a screenshot with
many colours only says *something* rendered. The chrome and the viewport are
checked separately, because a run where egui drew and the scene did not looks
identical in a colour count.

`--step` runs a different scenario through the same assertions: one process
exports a STEP file, a **second process** imports it, and the second one's
frame is what gets checked. That is the only place the STEP path is exercised
by the program rather than by a test harness — the kernel tests prove a kernel
can read what a kernel wrote, and this proves the modeller can open what the
modeller saved. It needs a build with `--features occt`, because the fake
kernel refuses STEP and says so.
"""

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Matches the panel width in `chrome`, in logical pixels at scale 1.
PANEL = 240


def read_ppm(path):
    data = path.read_bytes()
    magic, dims, _maxval, pixels = data.split(b"\n", 3)
    if magic != b"P6":
        raise SystemExit(f"not a binary PPM: {magic!r}")
    width, height = (int(n) for n in dims.split())
    return width, height, pixels


def colours(pixels, width, height, x0, x1):
    seen = set()
    for y in range(0, height, 4):
        row = y * width * 3
        for x in range(x0, min(x1, width), 4):
            at = row + x * 3
            seen.add(pixels[at : at + 3])
    return seen


def self_test():
    """A checker that cannot fail is a checker that says yes.

    Two synthetic frames: one flat, one a gradient. The flat one must be
    rejected by both assertions and the gradient accepted, or the real run
    below means nothing.
    """
    width, height = 64, 32
    flat = bytes([40, 42, 46]) * (width * height)
    gradient = bytes(
        b for y in range(height) for x in range(width) for b in (x * 4 % 256, y * 8 % 256, 90)
    )
    wrong = 0
    if len(colours(flat, width, height, 0, width)) >= 4:
        print("  SELF-TEST FAILED: a flat frame was not seen as flat")
        wrong += 1
    if len(colours(gradient, width, height, 0, width)) < 8:
        print("  SELF-TEST FAILED: a gradient was seen as flat")
        wrong += 1
    if wrong:
        print("the checker is broken; its verdict below means nothing.")
    return wrong


def run(binary, args, timeout=180):
    """One run of the modeller, in a window that closes itself."""
    result = subprocess.run(
        ["xvfb-run", "-a", "--server-args=-screen 0 1200x800x24", str(binary), *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    print(result.stdout.strip() or "(no adapter line)")
    if result.returncode != 0:
        print(result.stderr.strip()[:2000])
        raise SystemExit(f"the modeller exited {result.returncode}")
    return result


def main():
    if self_test():
        return 2
    binary = ROOT / "target" / "debug" / "w3d"
    if not binary.exists():
        raise SystemExit(f"{binary} does not exist — `cargo build -p w3d-app` first")
    step = "--step" in sys.argv[1:]

    with tempfile.TemporaryDirectory() as tmp:
        shot = Path(tmp) / "frame.ppm"
        if step:
            exported = Path(tmp) / "roundtrip.step"
            # One process writes it. No screenshot: what this run has to prove
            # is that a file appeared, and the frame that matters is the other
            # one's.
            run(binary, ["--demo", "--export-step", str(exported), "--frames", "3"])
            if not exported.exists():
                raise SystemExit("no STEP file was written")
            head = exported.read_bytes()[:13]
            if head != b"ISO-10303-21;":
                raise SystemExit(f"what was written does not begin as STEP: {head!r}")
            print(f"wrote {exported.stat().st_size} bytes of STEP")
            # And another reads it, in a process that shares nothing with the
            # first but the file.
            run(
                binary,
                ["--import-step", str(exported), "--frames", "30", "--screenshot", str(shot)],
            )
        else:
            run(binary, ["--demo", "--frames", "30", "--screenshot", str(shot)])
        if not shot.exists():
            raise SystemExit("no screenshot was written")

        width, height, pixels = read_ppm(shot)
        chrome = colours(pixels, width, height, 0, PANEL)
        viewport = colours(pixels, width, height, PANEL + 40, width)

        failures = []
        if len(chrome) < 4:
            failures.append(f"the chrome is {len(chrome)} flat colour(s): egui drew nothing")
        if len(viewport) < 8:
            failures.append(
                f"the viewport is {len(viewport)} flat colour(s): the scene drew nothing"
            )
        # A lit solid has a gradient across it; a solid fill does not. This is
        # what separates "cleared the screen" from "shaded a body".
        if len(viewport) < len(chrome):
            failures.append("the viewport has fewer colours than the side panel")

        print(f"{width}x{height} · chrome {len(chrome)} colours · viewport {len(viewport)}")
        for message in failures:
            print(f"FAIL  {message}")
        if failures:
            return 1
        if step:
            print("ok    a STEP file left one process and was drawn by another")
        else:
            print("ok    a window opened, egui drew, and the scene drew")
        return 0


if __name__ == "__main__":
    sys.exit(main())
