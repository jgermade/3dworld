#!/usr/bin/env python3
"""Fetches STEP files written by other people's programs.

`make test-occt` proves this kernel can read what this kernel wrote. That is
the weakest claim in the repository, and these files are how it stops being the
only one: an assembly from Pro/ENGINEER in AP203, a part from Siemens NX
through ST-Developer, and a surface model from STEP Tools that this modeller
must **refuse** — a negative control found in the wild rather than invented.

Separate from any build, and never run from one: a build that reaches the
network on its own is not a build anybody can reproduce. Same rule as
`make occt-headers`, and the same reason.

Each file is pinned by SHA-256 rather than by a branch, because a branch is a
promise nobody made. A file whose hash does not match is not kept — the one
upstream changed, and that is a thing to look at rather than a hash to edit.

    python3 tools/step_samples.py --fetch
    python3 tools/step_samples.py --list import
"""

import hashlib
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "tools" / "step-samples.txt"
INTO = ROOT / "samples" / "step"


def manifest():
    for line in MANIFEST.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        sha, verdict, name, url = line.split()
        yield sha, verdict, name, url


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch():
    INTO.mkdir(parents=True, exist_ok=True)
    failures = 0
    for sha, _verdict, name, url in manifest():
        target = INTO / name
        if target.exists() and digest(target) == sha:
            print(f"have  {name}")
            continue
        print(f"get   {name}")
        result = subprocess.run(
            ["curl", "-sSfL", "--max-time", "120", url, "-o", str(target)],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(f"FAIL  {name}: {result.stderr.strip()[:200]}")
            failures += 1
            continue
        got = digest(target)
        if got != sha:
            # Not kept and not accepted: the file upstream is not the file this
            # manifest was written against, and a check against the wrong input
            # is worse than no check.
            target.unlink()
            print(f"FAIL  {name}: sha256 {got}, expected {sha}")
            failures += 1
    if failures:
        return 1
    for _sha, verdict, name, _url in manifest():
        print(f"      {name} — must {verdict}")
    # Who actually wrote each one is a question for a parser, and there is one
    # in `step_check.py --describe`. A header field pulled out with string
    # surgery here got it wrong on two of these three files, which is a fair
    # summary of why part 21 has a grammar.
    print(f"ok    {INTO.relative_to(ROOT)}: fetched, and none of it is committed")
    return 0


def listing(verdict):
    missing = [n for _s, v, n, _u in manifest() if v == verdict and not (INTO / n).exists()]
    if missing:
        raise SystemExit(
            f"missing sample(s): {', '.join(missing)}\nRun `make step-samples` first."
        )
    for _sha, v, name, _url in manifest():
        if v == verdict:
            print(INTO / name)
    return 0


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] == "--fetch":
        sys.exit(fetch())
    if len(sys.argv) == 3 and sys.argv[1] == "--list":
        sys.exit(listing(sys.argv[2]))
    raise SystemExit(__doc__)
