#!/usr/bin/env python3
"""Assembles or checks the NOTICE file for a distribution.

Serving a `.wasm` build or shipping a binary is distribution under GPL-3.0-or-later.
This tool collects license texts and notices for:
1. The primary project (`3dworld`, GPL-3.0-or-later).
2. OpenCASCADE (LGPL-2.1-only, used under §3, with source distribution notice).
3. Embedded Font Data in `epaint_default_fonts` (OFL-1.1, Ubuntu-font-1.0, MIT).
4. All third-party Rust crates linked across targets.

Usage:
  python3 tools/notice.py           # Writes NOTICE
  python3 tools/notice.py --check   # Fails if NOTICE is missing or outdated
"""

import json
import os
import pathlib
import subprocess
import sys
import tarfile

TARGETS = ("x86_64-unknown-linux-gnu", "wasm32-unknown-unknown")

PROJECT_HEADER = """3dworld - B-rep CAD modeller in Rust and WebAssembly
Copyright (c) 2026 3dworld contributors

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU General Public License for more details.
"""

NON_CARGO_NOTICES = """========================================================================
OpenCASCADE Technology (OCCT)
========================================================================
Version: 7.6.3
License: GNU Lesser General Public License version 2.1 (LGPL-2.1-only)
         with OpenCASCADE Exception 1.0

This software links against OpenCASCADE Technology. Pursuant to Section 3
of the GNU Lesser General Public License version 2.1, this software is
re-licensed under the GNU General Public License version 3.0.

Corresponding Source for OpenCASCADE Technology is available at:
https://dev.opencascade.org/ or https://github.com/Open-Cascade-SAS/OCCT

========================================================================
Embedded Fonts (epaint_default_fonts via egui)
========================================================================
The binary embeds font data used for UI rendering:
1. Hack Font (SIL Open Font License v1.1 - OFL-1.1)
   Copyright (c) 2018 Source Foundry Authors
2. Ubuntu Font Family (Ubuntu Font Licence v1.0 - Ubuntu-font-1.0)
   Copyright 2010 Canonical Ltd.
3. Noto Emoji & Emoji Icon Font (SIL Open Font License v1.1 & MIT)
   Copyright (c) Google Inc.
"""


def get_packages_for_target(target):
    cmd = ["cargo", "metadata", "--format-version=1", f"--filter-platform={target}"]
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
    data = json.loads(res.stdout)
    pkgs = {}
    for p in data.get("packages", []):
        name = p["name"]
        if name.startswith("w3d-"):
            continue
        pkgs[(name, p["version"])] = p
    return pkgs


def find_license_file(manifest_path):
    pkg_dir = pathlib.Path(manifest_path).parent
    candidates = [
        "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE.txt",
        "LICENSE.md", "COPYING", "OFL.txt", "NOTICE"
    ]
    for c in candidates:
        p = pkg_dir / c
        if p.is_file():
            return p
    # Case-insensitive check
    if pkg_dir.is_dir():
        for f in pkg_dir.iterdir():
            if f.is_file() and (f.name.upper().startswith("LICENSE") or f.name.upper().startswith("COPYING")):
                return f
    return None


def find_license_in_crate_archive(name, version):
    home = pathlib.Path.home()
    pattern = f".cargo/registry/cache/*/{name}-{version}.crate"
    candidates = [
        "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE.txt",
        "LICENSE.md", "COPYING", "OFL.txt", "NOTICE"
    ]
    for crate_file in home.glob(pattern):
        try:
            with tarfile.open(crate_file, "r:gz") as tar:
                prefix = f"{name}-{version}/"
                for c in candidates:
                    try:
                        member = tar.getmember(prefix + c)
                        f = tar.extractfile(member)
                        if f:
                            return c, f.read().decode("utf-8", errors="replace").replace("\r\n", "\n").strip()
                    except KeyError:
                        pass
                for member in tar.getmembers():
                    rel = member.name[len(prefix):] if member.name.startswith(prefix) else member.name
                    if "/" not in rel and (rel.upper().startswith("LICENSE") or rel.upper().startswith("COPYING")):
                        f = tar.extractfile(member)
                        if f:
                            return rel, f.read().decode("utf-8", errors="replace").replace("\r\n", "\n").strip()
        except Exception:
            pass
    return None, None


def generate_notice():
    subprocess.run(["cargo", "fetch", "--target", "x86_64-unknown-linux-gnu", "--target", "wasm32-unknown-unknown"], capture_output=True, text=True)

    all_packages = {}
    for target in TARGETS:
        pkgs = get_packages_for_target(target)
        for (name, ver), pkg in pkgs.items():
            all_packages[(name, ver)] = pkg

    sorted_pkgs = sorted(all_packages.values(), key=lambda p: (p["name"].lower(), p["version"]))

    sections = [PROJECT_HEADER.strip(), NON_CARGO_NOTICES.strip()]

    sections.append("========================================================================\nThird-Party Rust Crates\n========================================================================")

    for pkg in sorted_pkgs:
        name = pkg["name"]
        version = pkg["version"]
        license_str = pkg.get("license", "Unknown")

        lic_file = find_license_file(pkg["manifest_path"])
        if lic_file:
            try:
                text = lic_file.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip()
                sections.append(f"--- {name} v{version} ({license_str}) ---\nPath: {lic_file.name}\n\n{text}")
            except Exception as e:
                sections.append(f"--- {name} v{version} ({license_str}) ---\n[Could not read license file: {e}]")
        else:
            arch_name, text = find_license_in_crate_archive(name, version)
            if arch_name and text is not None:
                sections.append(f"--- {name} v{version} ({license_str}) ---\nPath: {arch_name}\n\n{text}")
            else:
                sections.append(f"--- {name} v{version} ({license_str}) ---\n[No license file found in crate package; License declared: {license_str}]")

    return "\n\n".join(sections) + "\n"


def main():
    check_mode = "--check" in sys.argv
    out_path = pathlib.Path("NOTICE")

    content = generate_notice()

    if check_mode:
        if not out_path.exists():
            print("ERROR: NOTICE file does not exist. Run `make notice` to generate it.", file=sys.stderr)
            sys.exit(1)
        existing = out_path.read_text(encoding="utf-8").replace("\r\n", "\n")
        if existing != content:
            print("ERROR: NOTICE file is outdated. Run `make notice` to regenerate.", file=sys.stderr)
            sys.exit(1)
        print("NOTICE file is up-to-date.")
    else:
        out_path.write_text(content, encoding="utf-8")
        print(f"Wrote NOTICE file ({len(content)} bytes for {content.count('--- ')} third-party crates).")


if __name__ == "__main__":
    main()
