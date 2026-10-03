#!/usr/bin/env python3
"""Run the unchanged tracepoint load/get DSL script in an isolated runtime."""
import argparse
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    args = parser.parse_args()
    assert os.geteuid() == 0
    assert os.readlink("/proc/self/ns/mnt") != os.readlink("/proc/1/ns/mnt"), "use a private mount namespace"
    with tempfile.TemporaryDirectory(prefix="bpfman-rust-observation-") as directory:
        root = Path(directory) / "runtime"
        env = dict(os.environ, BPFMAN_RUNTIME_DIR=str(root), BPFMAN_E2E_BYTECODE_SOURCE="file")
        try:
            subprocess.run([
                "make", "run-e2e-scripts", f"BPFMAN_UNDER_TEST={args.rust}",
                "TEST=TestBPFManScripts/scripts/TestTracepoint_LoadAndGet[.]bpfman$",
            ], env=env, check=True, timeout=120)
            with sqlite3.connect(root / "db/store.db") as db:
                assert db.execute("SELECT count(*) FROM managed_programs").fetchone()[0] == 0
                assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 0
            assert not list((root / "fs").glob("prog_*"))
            assert not list((root / "fs/maps").iterdir())
            assert not list((root / "programs").iterdir())
            print("PASS: unchanged TestTracepoint_LoadAndGet.bpfman passes against Rust and leaves no owned residue")
        finally:
            mount = root / "fs"
            if mount.is_mount():
                subprocess.run(["umount", str(mount)], check=True, timeout=10)


if __name__ == "__main__":
    main()
