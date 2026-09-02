#!/usr/bin/env python3
"""Points wasm-bindgen-rayon's worker bootstrap at a URL a web server can serve.

`workerHelpers.js` — the file each rayon worker starts in — reaches back for the
main module with:

    const pkg = await import('../../..');

Relative to `dist/threaded/snippets/wasm-bindgen-rayon-<hash>/src/`, that is the
directory `dist/threaded/`. A bundler resolves a directory to its package entry;
a web server does not, and answers 404. The worker then never finishes booting
and never posts `wasm_bindgen_worker_ready`, so `initThreadPool` waits on a
promise that will not settle: the page hangs with no error, on the one path
where everything else has already succeeded. The upstream comment says the
technique "works well with bundlers today", and this build has no bundler.

So the import is rewritten to name the entry file. Three things make that safe
to do to generated output:

  - It fails loudly when the pattern is not there. A future wasm-bindgen that
    changes this file's layout must break the build, not silently restore the
    hang — a fix-up that quietly does nothing is worse than no fix-up.
  - It is idempotent, so re-running `make web-threaded` over an existing
    dist is not an error.
  - It touches one import specifier and nothing else.

Usage: rayon_worker_entry.py <dist dir> <entry file name>
"""

import glob
import os
import sys

BUNDLER_SPECIFIER = "import('../../..')"


def main(argv):
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    dist, entry = argv[1], argv[2]

    pattern = os.path.join(dist, "snippets", "wasm-bindgen-rayon-*", "src", "workerHelpers.js")
    found = sorted(glob.glob(pattern))
    if len(found) != 1:
        print(
            f"expected exactly one {pattern}, found {len(found)}. "
            "wasm-bindgen's snippet layout has changed, or --split-linked-modules "
            "was not passed and the bootstrap was inlined instead.",
            file=sys.stderr,
        )
        return 1

    path = found[0]
    with open(path, encoding="utf-8") as handle:
        source = handle.read()

    fixed = f"import('../../../{entry}')"
    if fixed in source:
        print(f"{path}: already points at {entry}")
        return 0

    count = source.count(BUNDLER_SPECIFIER)
    if count != 1:
        print(
            f"{path}: expected exactly one {BUNDLER_SPECIFIER}, found {count}. "
            "Do not guess — read the file: rayon's workers resolve the main "
            "module from here, and getting it wrong is a page that hangs with "
            "no error rather than one that fails.",
            file=sys.stderr,
        )
        return 1

    with open(path, "w", encoding="utf-8") as handle:
        handle.write(source.replace(BUNDLER_SPECIFIER, fixed))
    print(f"{path}: worker entry now {entry}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
