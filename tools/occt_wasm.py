#!/usr/bin/env python3
"""OpenCASCADE for the browser: a second wasm module, built from a pinned source.

The browser's Rust is `wasm32-unknown-unknown`, built by wasm-bindgen, and
OCCT cannot be linked into it: OCCT needs a libc, a libc++ and C++ exceptions,
and that target has none of them. Moving the Rust to
`wasm32-unknown-emscripten` would lose wasm-bindgen. So OCCT is a module of its
own, built by Emscripten, exporting exactly `kernel-occt/native/w3d_occt.h` —
the same C seam the desktop links — and the Rust side reaches it through JS in
the same worker. Nothing OCCT-shaped crosses; the header's rule holds.

Two things the register said had to be *decided* rather than discovered, and
are decided here:

- **`-fwasm-exceptions`, not `-fexceptions`.** The shim catches
  `Standard_Failure` at every entry point, and OCCT throws for input it
  dislikes, so exceptions are not optional. `-fexceptions` emulates them
  through JS with an invoke wrapper around every call that might throw — a
  large size and speed cost on code this call-heavy. Native wasm exceptions
  are in every engine this page already requires (it requires WebGPU or
  WebGL2 and module workers). No `-fno-exceptions` build exists: without
  exceptions, OCCT's first complaint about a degenerate input is an abort.
- **A real pin.** `native/UPSTREAM` named a version and enforced nothing. Here
  the source is fetched at a tag and **refused unless the tag is the commit
  recorded below**, and the Emscripten that builds it is refused unless it is
  the version recorded below, because a different compiler is a different
  binary.

Steps, each its own subcommand so a failure says which:

  src      clone OCCT at the tag into build/occt-wasm/src, verify the commit
  emsdk    install the pinned Emscripten into tools/.emsdk
  build    configure and build the toolkits the shim needs (static libraries)
  link     compile the shim and link web/dist/occt/w3d_occt.{mjs,wasm}
  all      the four, in order

Nothing here reaches the network except `src` and `emsdk`, which are run on
purpose — the same rule as `make occt-headers` and `make binaryen`. Nothing
here is committed: the sources and libraries are OCCT's (LGPL-2.1 with the
OCCT exception), and the build tree is large.
"""

import os
import re
import shutil
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))

OCCT_REPO = "https://github.com/Open-Cascade-SAS/OCCT.git"
OCCT_TAG = "V7_6_3"
OCCT_COMMIT = "b079fb9877ef64d4a8158a60fa157f59b096debb"

EMSDK_REPO = "https://github.com/emscripten-core/emsdk.git"
EMSCRIPTEN_VERSION = "4.0.15"

# Overridable because the build tree is large and slow to make, and a second
# checkout should be able to reuse one. The pin is still checked on whatever
# is pointed at.
WORK = os.environ.get("OCCT_WASM_DIR", os.path.join(ROOT, "build", "occt-wasm"))
SRC = os.environ.get("OCCT_WASM_SRC", os.path.join(WORK, "src"))
BUILD = os.environ.get("OCCT_WASM_BUILD", os.path.join(WORK, "build"))
EMSDK = os.path.join(ROOT, "tools", ".emsdk")
NATIVE = os.path.join(ROOT, "kernel-occt", "native")
OUT = os.path.join(ROOT, "web", "dist", "occt")

# What the shim links: the list `kernel-occt/build.rs` gives the desktop
# linker, plus what the DataExchange toolkits pull in. Every one is named —
# asking for the leaves builds the leaves and nothing under them, because a
# static library's dependencies are not build dependencies to CMake; the first
# build here produced six archives and no TKernel.
TARGETS = [
    "TKernel", "TKMath", "TKG2d", "TKG3d", "TKGeomBase", "TKBRep", "TKGeomAlgo", "TKTopAlgo",
    "TKPrim", "TKBO", "TKBool", "TKShHealing", "TKMesh", "TKFillet", "TKOffset", "TKXSBase",
    "TKCDF", "TKLCAF", "TKCAF", "TKService", "TKV3d", "TKVCAF", "TKXCAF", "TKSTEPBase",
    "TKSTEPAttr", "TKSTEP209", "TKSTEP", "TKXDESTEP",
]

# Everything OCCT's CMake would otherwise go looking for. Each one is a library
# a browser build has no use for, and each `ON` is a dependency nobody pinned.
CMAKE_FLAGS = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DBUILD_LIBRARY_TYPE=Static",
    "-DCMAKE_CXX_FLAGS=-fwasm-exceptions",
    "-DCMAKE_C_FLAGS=-fwasm-exceptions",
    "-DBUILD_MODULE_Draw=OFF",
    "-DBUILD_MODULE_Visualization=OFF",
    "-DBUILD_MODULE_ApplicationFramework=ON",
    "-DBUILD_MODULE_DataExchange=ON",
    "-DBUILD_DOC_Overview=OFF",
    "-DUSE_FREETYPE=OFF",
    "-DUSE_TK=OFF",
    "-DUSE_OPENGL=OFF",
    "-DUSE_GLES2=OFF",
    "-DUSE_RAPIDJSON=OFF",
    "-DUSE_FREEIMAGE=OFF",
    "-DUSE_VTK=OFF",
    "-DUSE_TBB=OFF",
    "-DUSE_DRACO=OFF",
    "-DUSE_OPENVR=OFF",
]


def run(cmd, **kw):
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True, **kw)


def fail(msg):
    raise SystemExit(f"occt_wasm: {msg}")


def src():
    if not os.path.isdir(os.path.join(SRC, ".git")):
        os.makedirs(WORK, exist_ok=True)
        run(["git", "clone", "--depth", "1", "--branch", OCCT_TAG, OCCT_REPO, SRC])
    head = subprocess.run(
        ["git", "-C", SRC, "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if head != OCCT_COMMIT:
        fail(f"{SRC} is at {head}, not the pinned {OCCT_COMMIT} ({OCCT_TAG}). "
             "A tag can be moved; the commit is the pin.")
    print(f"OCCT {OCCT_TAG} at {head}")


def emsdk_env():
    """The environment with the pinned Emscripten first on PATH."""
    env = dict(os.environ)
    upstream = os.path.join(EMSDK, "upstream", "emscripten")
    if os.path.isdir(upstream):
        env["PATH"] = os.pathsep.join([upstream, os.path.join(EMSDK, "upstream", "bin"), env["PATH"]])
        env["EMSDK"] = EMSDK
    return env


def emsdk():
    if not os.path.isdir(EMSDK):
        run(["git", "clone", "--depth", "1", EMSDK_REPO, EMSDK])
    run([os.path.join(EMSDK, "emsdk"), "install", EMSCRIPTEN_VERSION])
    run([os.path.join(EMSDK, "emsdk"), "activate", EMSCRIPTEN_VERSION])


def check_emcc(env):
    emcc = shutil.which("emcc", path=env["PATH"])
    if not emcc:
        fail("no emcc. `make occt-wasm-emsdk` installs the pinned Emscripten, "
             "or put one on PATH.")
    out = subprocess.run([emcc, "--version"], env=env, check=True, capture_output=True,
                         text=True).stdout
    found = re.search(r"\) (\d+\.\d+\.\d+)", out)
    version = found.group(1) if found else "unknown"
    if version != EMSCRIPTEN_VERSION:
        fail(f"emcc is {version}, not the pinned {EMSCRIPTEN_VERSION}. A different "
             "compiler is a different binary; install the pin or change it here, on purpose.")
    return emcc


def build():
    env = emsdk_env()
    check_emcc(env)
    os.makedirs(BUILD, exist_ok=True)
    run(["emcmake", "cmake", SRC, "-G", "Unix Makefiles", *CMAKE_FLAGS], cwd=BUILD, env=env)
    run(["make", f"-j{os.cpu_count() or 2}", *TARGETS], cwd=BUILD, env=env)


def exported_functions():
    """Every entry point `w3d_occt.h` declares: the header is the list."""
    header = open(os.path.join(NATIVE, "w3d_occt.h")).read()
    names = sorted(set(re.findall(r"\b(w3d_occt_\w+)\s*\(", header)))
    if not names:
        fail("found no entry points in w3d_occt.h")
    return ["_" + n for n in names] + ["_malloc", "_free"]


def libraries():
    """The static libraries the build produced, in link order. OCCT's CMake puts
    them under lin32/clang/lib or similar; searched rather than guessed."""
    found = {}
    for dirpath, _, files in os.walk(BUILD):
        for f in files:
            m = re.fullmatch(r"lib(TK\w+)\.a", f)
            if m:
                found[m.group(1)] = os.path.join(dirpath, f)
    if not found:
        fail("no OCCT libraries under the build tree; run the `build` step")
    return found


def headers():
    """A flat include directory, which is what OCCT's own install makes: every
    header of every package, side by side. OCCT only assembles it on `make
    install`, which would build every toolkit; linking them in is enough."""
    inc = os.path.join(BUILD, "w3d-include")
    os.makedirs(inc, exist_ok=True)
    for package in os.listdir(os.path.join(SRC, "src")):
        pdir = os.path.join(SRC, "src", package)
        if not os.path.isdir(pdir):
            continue
        for f in os.listdir(pdir):
            if f.endswith((".hxx", ".lxx", ".gxx", ".pxx", ".h")):
                dst = os.path.join(inc, f)
                if not os.path.lexists(dst):
                    os.symlink(os.path.join(pdir, f), dst)
    return inc


# Link order: dependents before their dependencies, for a static link. The
# `--start-group` below makes the order forgiving, and this order is the
# desktop's, reversed, so it is right without it.
LINK_ORDER = [
    "TKXDESTEP", "TKSTEP", "TKSTEP209", "TKSTEPAttr", "TKSTEPBase", "TKXCAF", "TKVCAF",
    "TKV3d", "TKService", "TKCAF", "TKLCAF", "TKCDF", "TKXSBase", "TKFillet", "TKOffset",
    "TKBool", "TKBO", "TKShHealing", "TKMesh", "TKPrim", "TKTopAlgo", "TKGeomAlgo", "TKBRep",
    "TKGeomBase", "TKG3d", "TKG2d", "TKMath", "TKernel",
]


def link():
    env = emsdk_env()
    emcc = check_emcc(env)
    libs = libraries()
    missing = [t for t in LINK_ORDER if t not in libs and t not in ("TKV3d", "TKService", "TKVCAF")]
    if missing:
        fail(f"toolkits not built: {', '.join(missing)}")
    includes = [headers()]
    os.makedirs(OUT, exist_ok=True)
    cmd = [
        emcc, os.path.join(NATIVE, "w3d_occt.cpp"),
        "-std=c++17", "-O3", "-fwasm-exceptions",
        "-I", NATIVE, "-I", includes[0],
        "-Wl,--start-group", *[libs[t] for t in LINK_ORDER if t in libs], "-Wl,--end-group",
        "-sMODULARIZE=1", "-sEXPORT_ES6=1", "-sEXPORT_NAME=createOcct",
        "-sENVIRONMENT=web,worker,node",
        "-sALLOW_MEMORY_GROWTH=1", "-sMAXIMUM_MEMORY=4GB", "-sINITIAL_MEMORY=64MB",
        # Emscripten's default stack is 64 KiB since 3.1.27. OCCT's boolean and
        # blend algorithms recurse far deeper than a stack sized for C; a stack
        # overflow here is a trap, not an exception, so the shim cannot catch
        # it. 4 MiB is the desktop's order of magnitude.
        "-sSTACK_SIZE=4MB",
        "-sEXPORTED_FUNCTIONS=" + ",".join(exported_functions()),
        "-sEXPORTED_RUNTIME_METHODS=HEAPU8,HEAPU32,HEAP32,HEAPF32,HEAPF64,UTF8ToString",
        "-o", os.path.join(OUT, "w3d_occt.mjs"),
    ]
    run(cmd, env=env)
    wasm = os.path.join(OUT, "w3d_occt.wasm")
    print(f"{wasm}: {os.path.getsize(wasm) / 1048576:.2f} MiB")


def main():
    steps = {"src": src, "emsdk": emsdk, "build": build, "link": link}
    args = sys.argv[1:]
    if args == ["all"]:
        args = ["src", "emsdk", "build", "link"]
    if not args or any(a not in steps for a in args):
        raise SystemExit(__doc__)
    for a in args:
        steps[a]()


if __name__ == "__main__":
    main()
