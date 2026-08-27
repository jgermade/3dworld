#!/usr/bin/env python3
"""Build script for compiling OpenCASCADE C ABI to WebAssembly via Emscripten.

Usage:
  python3 tools/build_occt_wasm.py
"""

import argparse
import os
import shutil
import subprocess
import sys

def main():
    parser = argparse.ArgumentParser(description="Build w3d_occt for WebAssembly via Emscripten")
    parser.add_argument("--build-dir", default="build-wasm", help="Output build directory")
    args = parser.parse_args()

    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    native_dir = os.path.join(repo_root, "kernel-occt", "native")
    build_dir = os.path.join(native_dir, args.build_dir)

    emcmake = shutil.which("emcmake")
    emmake = shutil.which("emmake")

    if not emcmake or not emmake:
        print("Notice: Emscripten (emcmake / emmake) not found in PATH.")
        print("To compile OpenCASCADE C ABI for WebAssembly, install Emscripten SDK and source emsdk_env.sh.")
        print("Skipping WebAssembly compilation step.")
        sys.exit(0)

    os.makedirs(build_dir, exist_ok=True)

    print(f"Configuring w3d_occt with emcmake in {build_dir}...")
    cmd_config = [emcmake, "cmake", "-B", build_dir, "-S", native_dir]
    res = subprocess.run(cmd_config)
    if res.returncode != 0:
        print("Error: emcmake failed to configure w3d_occt CMake project.")
        sys.exit(res.returncode)

    print("Building w3d_occt with emmake...")
    cmd_build = [emmake, "make", "-C", build_dir]
    res = subprocess.run(cmd_build)
    if res.returncode != 0:
        print("Error: emmake failed to compile w3d_occt.")
        sys.exit(res.returncode)

    print("Successfully built w3d_occt for WebAssembly!")

if __name__ == "__main__":
    main()
