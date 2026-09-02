#!/usr/bin/env python3
"""Every crate this project links must be compatible with GPL-3.0-or-later.

`AGENTS.md § Licensing` has said so since the licence was settled, and nothing
checked it until this file existed. It is a check rather than an audit for the
same reason `make wasm` greps for `glow`: a rule nobody can run is a rule that
holds until the first person who does not know about it.

It reads `cargo metadata` once per *build* — see BUILDS. Two targets, because
the dependency sets differ and a crate that only appears on wasm is exactly the
one nobody looks at; and wasm a second time with the threaded variant's feature
on, because `--filter-platform` prunes an optional dependency that is off and a
crate nobody enabled is a crate nobody audited. It fails on any licence not on
the allowlist below. Adding a dependency with an unlisted
licence fails; the fix is to read the licence and either widen the list with an
argument in a session file, or not take the dependency.

Zero dependencies, because a licence checker that needs a dependency has a
problem it cannot see.

Out of scope, and named in the report rather than silently omitted: anything
cargo does not know about. See NON_CARGO below.
"""

import json
import re
import subprocess
import sys

# Each entry is a *build*, not a platform: the same target with different
# features is a different set of crates, and the one nobody looks at is the one
# that only exists behind a feature flag. `--filter-platform` prunes optional
# dependencies that are off, so a build that ships them has to be asked for by
# name — `wasm-bindgen-rayon` and `wasm_sync` reach a user's browser in the
# threaded variant and appeared in neither of the first two entries.
BUILDS = (
    ("x86_64-unknown-linux-gnu", ()),
    ("wasm32-unknown-unknown", ()),
    ("wasm32-unknown-unknown", ("--features", "w3d-web/threads")),
)

# Permissive licences that impose no condition GPL-3 cannot satisfy. Each entry
# is here because someone read it, not because it looked familiar.
ALLOWED = {
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MIT-0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Unlicense",
    "Zlib",
}

# Exceptions that only ever widen a permission.
ALLOWED_EXCEPTIONS = {"LLVM-exception"}

# Font licences, allowed **only for crates that are font data** — see
# FONT_DATA below. Scoped rather than added to ALLOWED because the argument
# for them is about fonts and would not survive being applied to code:
#
#   OFL-1.1 is a copyleft licence written so that fonts can be bundled with
#   software under any licence. Its conditions — keep the notice, do not sell
#   the font on its own, rename a modified font — are all satisfiable by a
#   GPL-3 work, and every distribution ships OFL fonts alongside GPL programs.
#
#   Ubuntu-font-1.0 is DFSG-free and in Debian main. The FSF has never ruled on
#   it, which is why it is not in ALLOWED, and the reason it is acceptable here
#   is narrower than "it is compatible": a font is *data this program displays*,
#   not code linked into it, so the font keeps its licence and the program keeps
#   GPL-3. That is aggregation, and it is what every GUI toolkit relies on.
#
# Both **require their notices to be preserved in a distribution**, which this
# project does not yet assemble. See the register's NOTICE item; these two make
# it a requirement rather than good manners.
ALLOWED_FONT_DATA = {"OFL-1.1", "Ubuntu-font-1.0"}

# Crates that are fonts rather than code. Kept as a list of names so that
# adding one is a visible act.
FONT_DATA = {"epaint_default_fonts"}

# Licences worth refusing with a *reason* rather than a shrug. Anything not
# here and not allowed still fails; these just fail informatively.
KNOWN_BAD = {
    "GPL-2.0": "GPL-2.0-only is incompatible with GPL-3: neither can take the other.",
    "GPL-2.0-only": "GPL-2.0-only is incompatible with GPL-3: neither can take the other.",
    "AGPL-3.0": "AGPL-3 would make the combined work AGPL, which is not what this project is.",
    "AGPL-3.0-only": "AGPL-3 would make the combined work AGPL, which is not what this project is.",
    "AGPL-3.0-or-later": "AGPL-3 would make the combined work AGPL, which is not what this project is.",
    "MPL-2.0": (
        "MPL-2.0 is usually GPL-compatible through its §3.3, but the "
        '"Incompatible With Secondary Licenses" variant is not, and the '
        "cargo metadata field cannot tell them apart. Read the file."
    ),
    "SSPL-1.0": "Not a free licence, and not compatible with anything here.",
    "BUSL-1.1": "Not a free licence.",
    "OpenSSL": "The OpenSSL licence's advertising clause is GPL-incompatible.",
}

# What cargo cannot see. Listed so the report is the whole picture rather than
# the part that was easy to automate.
NON_CARGO = [
    (
        "OpenCASCADE 7.6.3",
        "LGPL-2.1-only, with OCCT-exception-1.0",
        "linked by kernel-occt",
        "Taken to GPL-3 through LGPL-2.1 §3, which is the whole reason this "
        "licence was available to choose. See AGENTS.md § Licensing. The "
        "exception is not relied on and is not needed.",
    ),
    (
        "NCollection_AliasedArray.hxx",
        "LGPL-2.1-only (OCCT)",
        "fetched by `make occt-headers`, never committed",
        "A header Ubuntu Noble fails to ship. It is OCCT's file, it stays "
        "OCCT's, and native/vendor-include/ is gitignored.",
    ),
    (
        "steputils 0.1",
        "MIT",
        "dev-time only, used by `make step-check`; not in the crate graph",
        "A pure-Python ISO 10303-21 parser, and the point of it is that it "
        "shares no code with OpenCASCADE. Nothing it touches is distributed "
        "and nothing links to it.",
    ),
    (
        "STEP sample files (as1_pe_203, face_recognition_sample_part, splinecage)",
        "GPL-3.0-or-later, as distributed in tpaviot/pythonocc-demos",
        "fetched by `make step-samples`, never committed, never distributed",
        "Input to a check, not part of the program: files written by "
        "Pro/ENGINEER, Siemens NX and ST-Developer, which is the whole reason "
        "they are worth having. Pinned by SHA-256 in tools/step-samples.txt.",
    ),
    (
        "Playwright",
        "Apache-2.0",
        "devDependency of web/test/, not in the crate graph",
        "A test driver. Nothing it touches is distributed, and it is not "
        "linked into anything.",
    ),
    (
        "Hack, Ubuntu-Light, Noto Emoji, emoji-icon-font",
        "OFL-1.1, Ubuntu-font-1.0, MIT",
        "embedded in the binary by epaint_default_fonts, via egui",
        "Font *data*, not code: the fonts keep their licences and the program "
        "keeps GPL-3. Allowed only for crates named in FONT_DATA above. Their "
        "notices must ship with any distribution, which nothing yet assembles.",
    ),
    (
        "Chromium, Mesa/lavapipe",
        "BSD-3-Clause and others / MIT",
        "installed by a developer or CI to run tests",
        "Test-time only. Not linked, not shipped, not part of the work.",
    ),
]

TOKEN = re.compile(r"\(|\)|[^\s()]+")


def parse(expression):
    """An SPDX expression as nested lists: ('or'|'and', [terms...]) or a str.

    Cargo's `license` field is an SPDX expression, and `A OR B` is a *choice* —
    so a crate offering `MIT OR Apache-2.0` is fine even if only one of the two
    were acceptable. Treating the field as an opaque string is how a checker
    ends up rejecting half of crates.io.
    """
    # The pre-SPDX `MIT/Apache-2.0` form still appears and means OR.
    tokens = TOKEN.findall(expression.replace("/", " OR "))
    pos = 0

    def primary():
        nonlocal pos
        if tokens[pos] == "(":
            pos += 1
            node = expr()
            pos += 1  # ')'
            return node
        term = tokens[pos]
        pos += 1
        # `Apache-2.0 WITH LLVM-exception` is one term, not two.
        if pos + 1 < len(tokens) and tokens[pos].upper() == "WITH":
            term = f"{term} WITH {tokens[pos + 1]}"
            pos += 2
        return term

    def expr(level=0):
        nonlocal pos
        ops = ("OR", "AND")
        node = expr(level + 1) if level < len(ops) - 1 else primary()
        parts = [node]
        while pos < len(tokens) and tokens[pos].upper() == ops[level]:
            pos += 1
            parts.append(expr(level + 1) if level < len(ops) - 1 else primary())
        return parts[0] if len(parts) == 1 else (ops[level].lower(), parts)

    return expr()


def satisfiable(node, allowed=None):
    """Can this expression be satisfied entirely from `allowed`?"""
    allowed = ALLOWED if allowed is None else allowed
    if isinstance(node, str):
        if " WITH " in node:
            licence, exception = node.split(" WITH ")
            return licence in allowed and exception in ALLOWED_EXCEPTIONS
        return node in allowed
    op, parts = node
    results = (satisfiable(part, allowed) for part in parts)
    return any(results) if op == "or" else all(results)


def leaves(node):
    if isinstance(node, str):
        return [node]
    return [leaf for part in node[1] for leaf in leaves(part)]


def crates(target, extra=()):
    out = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            target,
            *extra,
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    for package in json.loads(out.stdout)["packages"]:
        # Workspace members are this project; they are GPL-3 by definition.
        if package["name"].startswith("w3d-"):
            continue
        yield package["name"], package["version"], package["license"]


# A checker that cannot fail is a checker that says yes. The SIMD probe in
# `web/loader.js` validated nothing for exactly this reason and would have
# reported "no SIMD128" on every browser forever, so this one carries its own
# negative controls and `make licences` runs them first.
SELF_TEST = [
    ("MIT", True, "the simplest allowed case"),
    ("MIT OR Apache-2.0", True, "a choice, and both are fine"),
    ("MIT/Apache-2.0", True, "the pre-SPDX separator still means OR"),
    ("MIT OR GPL-2.0-only", True, "a choice where one option is fine"),
    ("(MIT OR Apache-2.0) AND Unicode-3.0", True, "parentheses, and unicode-ident's real licence"),
    ("Apache-2.0 WITH LLVM-exception", True, "an exception that only widens"),
    ("Zlib OR Apache-2.0 OR MIT", True, "bytemuck's real licence"),
    ("GPL-2.0-only", False, "incompatible in both directions"),
    ("MIT AND GPL-2.0-only", False, "an AND cannot be escaped by choosing"),
    ("AGPL-3.0-or-later", False, "would relicense the combined work"),
    ("MPL-2.0", False, "needs a human to read which variant it is"),
    ("Apache-2.0 WITH Commons-Clause", False, "an unknown exception may narrow"),
    ("Sleepycat", False, "not on the list, so not assumed"),
    ("", False, "no licence at all is not permission"),
    ("(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0", False, "font licences are not allowed for code"),
]

# The same expression, allowed only because the crate is font data.
FONT_SELF_TEST = [
    ("(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0", True, "epaint_default_fonts"),
    ("GPL-2.0-only", False, "being a font does not excuse everything"),
]


def self_test():
    """Returns the number of controls that did not behave."""
    wrong = 0
    for expression, expected, why in SELF_TEST:
        got = satisfiable(parse(expression)) if expression else False
        if got != expected:
            wrong += 1
            print(f"  SELF-TEST FAILED: {expression!r} -> {got}, expected {expected} ({why})")
    for expression, expected, why in FONT_SELF_TEST:
        got = satisfiable(parse(expression), ALLOWED | ALLOWED_FONT_DATA)
        if got != expected:
            wrong += 1
            print(f"  SELF-TEST FAILED (font data): {expression!r} -> {got}, expected {expected} ({why})")
    if wrong:
        print(f"\n{wrong} of {len(SELF_TEST)} controls behaved wrongly. The checker is broken;")
        print("its verdict below means nothing until this passes.\n")
    return wrong


def main():
    if self_test():
        return 2

    failures = []
    everything = {}

    for target, extra in BUILDS:
        for name, version, licence in crates(target, extra):
            everything.setdefault((name, version), (licence, set()))[1].add(target)

    for (name, version), (licence, targets) in sorted(everything.items()):
        where = ", ".join(sorted(t.split("-")[0] for t in targets))
        if not licence:
            failures.append((name, version, "(no license field)", where, "Read the crate's LICENSE files by hand."))
            continue
        allowed = ALLOWED | ALLOWED_FONT_DATA if name in FONT_DATA else ALLOWED
        if satisfiable(parse(licence), allowed):
            continue
        reason = next(
            (KNOWN_BAD[leaf] for leaf in leaves(parse(licence)) if leaf in KNOWN_BAD),
            "Not on the allowlist in tools/licences.py. Read it, then either "
            "widen the list with an argument in a session file, or drop the "
            "dependency.",
        )
        failures.append((name, version, licence, where, reason))

    print(f"{len(everything)} third-party crates across {len(BUILDS)} builds\n")

    print("Not visible to cargo, and part of the answer anyway:")
    for what, licence, how, why in NON_CARGO:
        print(f"  {what} — {licence}")
        print(f"      {how}")
        print(f"      {why}")
    print()

    if failures:
        print(f"{len(failures)} crate(s) are not clearly compatible with GPL-3.0-or-later:\n")
        for name, version, licence, where, reason in failures:
            print(f"  {name} {version}  [{where}]")
            print(f"      licence: {licence}")
            print(f"      {reason}")
        return 1

    print("Every crate is compatible with GPL-3.0-or-later.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
