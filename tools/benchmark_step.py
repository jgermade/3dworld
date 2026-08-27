#!/usr/bin/env python3
"""Benchmarks STEP import, memory usage, and tessellation performance.

Measures wall-clock time, peak RSS memory, body counts, face counts, and triangle
counts for STEP assemblies using `w3d-kernel-occt`.

Usage:
  python3 tools/benchmark_step.py
"""

import os
import pathlib
import resource
import subprocess
import time
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SAMPLES_DIR = ROOT / "samples" / "step"


def get_occt_env():
    env = os.environ.copy()
    if sys.platform == "darwin":
        brew_occt = pathlib.Path("/opt/homebrew/opt/opencascade")
        if brew_occt.is_dir():
            env.setdefault("OCCT_INCLUDE_DIR", str(brew_occt / "include" / "opencascade"))
            env.setdefault("OCCT_LIB_DIR", str(brew_occt / "lib"))
    return env


def benchmark_file(path):
    if not path.is_file():
        return None

    file_size_mb = path.stat().st_size / (1024 * 1024)
    env = get_occt_env()

    # Pre-build example binary so compilation time is excluded from benchmark
    subprocess.run(
        ["cargo", "build", "-q", "-p", "w3d-kernel-occt", "--example", "import_step"],
        cwd=ROOT,
        env=env,
        check=True,
    )

    cmd = [
        "cargo", "run", "-q", "-p", "w3d-kernel-occt", "--example", "import_step", "--",
        str(path)
    ]

    start_time = time.perf_counter()
    usage_start = resource.getrusage(resource.RUSAGE_CHILDREN)
    res = subprocess.run(
        cmd,
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    duration_sec = time.perf_counter() - start_time
    usage_end = resource.getrusage(resource.RUSAGE_CHILDREN)

    # Memory in MB (maxrss is bytes on macOS, KB on Linux)
    rss_unit = 1.0 if sys.platform == "darwin" else 1024.0
    peak_rss_mb = usage_end.ru_maxrss / (1024.0 * 1024.0 / rss_unit)

    output = res.stdout + res.stderr
    return {
        "file": path.name,
        "size_mb": file_size_mb,
        "duration_sec": duration_sec,
        "peak_rss_mb": peak_rss_mb,
        "output": output.strip(),
        "exit_code": res.returncode,
    }


def main():
    if not SAMPLES_DIR.is_dir() or not list(SAMPLES_DIR.glob("*.stp")):
        print("Fetching STEP sample files...")
        subprocess.run([sys.executable, str(ROOT / "tools" / "step_samples.py"), "--fetch"], check=True)

    files = sorted(SAMPLES_DIR.glob("*.stp"))
    print(f"========================================================================")
    print(f"STEP Import & Tessellation Benchmark")
    print(f"========================================================================")
    print(f"{'File':<32} {'Size (MB)':<10} {'Time (s)':<10} {'Status':<10}")
    print("-" * 65)

    for f in files:
        if f.name == "splinecage.stp": # surface model meant to be refused
            continue
        bench = benchmark_file(f)
        if bench:
            status = "OK" if bench["exit_code"] == 0 else "Refused/Err"
            print(f"{bench['file']:<32} {bench['size_mb']:<10.2f} {bench['duration_sec']:<10.3f} {status:<10}")
            if bench["output"]:
                for line in bench["output"].splitlines()[:5]:
                    print(f"  > {line}")

    print("-" * 65)


if __name__ == "__main__":
    main()
