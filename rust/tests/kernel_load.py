#!/usr/bin/env python3
"""Local tracepoint acceptance: run only in a fresh private mount namespace.

The Go CLI is an independent observer and cleans up successful loads. This is
an incremental adapter gate; it does not replace the unchanged DSL parity suite.
"""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def run(binary, root, *args, success=True):
    result = subprocess.run([str(binary), "--runtime-dir", str(root), *map(str, args)],
                            capture_output=True, text=True, timeout=30)
    if success:
        assert result.returncode == 0, (result.args, result.stdout, result.stderr)
    else:
        assert result.returncode == 1, (result.args, result.returncode, result.stderr)
        assert not result.stdout, result.stdout
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--go", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    args = parser.parse_args()
    assert os.geteuid() == 0, "requires root and a private mount namespace"
    assert os.readlink("/proc/self/ns/mnt") != os.readlink("/proc/1/ns/mnt"), "use unshare --mount --propagation private"
    source = args.fixtures / "tracepoint_counter.bpf.o"
    selected = "tracepoint:tracepoint_kill_recorder"
    with tempfile.TemporaryDirectory(prefix="bpfman-rust-load-") as temporary:
        root = Path(temporary) / "runtime"
        try:
            # A FIFO must fail validation instead of blocking while opening it.
            fifo = Path(temporary) / "source.fifo"
            os.mkfifo(fifo)
            bad = run(args.rust, root, "program", "load", "file", fifo,
                      "--programs", selected, success=False)
            assert "regular ELF file" in bad.stderr, bad.stderr
            assert not root.exists()
            # ELF-level unsupported input is rejected before runtime setup.
            bad = run(args.rust, root, "program", "load", "file",
                      args.fixtures / "tracepoint_counter_pinned.bpf.o",
                      "--programs", selected, success=False)
            assert "PinByName" in bad.stderr, bad.stderr
            assert not root.exists()
            for fixture, selection, diagnostic in [
                (source, "tracepoint:missing", "does not exist"),
                (args.fixtures / "xdp_pass.bpf.o", "tracepoint:pass", "not a tracepoint"),
            ]:
                bad = run(args.rust, root, "program", "load", "file", fixture,
                          "--programs", selection, success=False)
                assert diagnostic in bad.stderr, bad.stderr
                assert not root.exists()

            run(args.rust, root, "program", "load", "file", source, "--programs", selected,
                "--application", "rust-slice", "--metadata", "test=acceptance")
            # The loader has exited: the program and maps must still be pinned.
            db = sqlite3.connect(root / "db/store.db")
            rows = db.execute("SELECT program_id, program_name, source_path, object_path, pin_path, license, updated_at FROM managed_programs").fetchall()
            assert len(rows) == 1, rows
            pid, name, original, bytecode, pin, license, updated = rows[0]
            assert name == "tracepoint_kill_recorder"
            assert original == str(source)
            assert license == "Dual BSD/GPL" and updated is None
            assert Path(bytecode).read_bytes() == source.read_bytes()
            assert Path(pin).exists()
            assert (root / "fs/maps" / str(pid) / "tracepoint_stats_map").exists()
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 1
            assert db.execute("SELECT count(*) FROM links").fetchone()[0] == 0
            provenance = json.loads((Path(bytecode).parent / "provenance.json").read_text())
            assert provenance["program_id"] == pid and provenance["source"] == str(source)
            assert run(args.rust, root, "program", "list", "-q").stdout.strip() == str(pid)
            observation = json.loads(run(args.go, root, "program", "get", pid, "-o", "json").stdout)
            assert observation["record"]["program_id"] == pid, observation
            assert observation["status"]["kernel"] is not None, observation
            run(args.go, root, "program", "unload", pid)
            assert not Path(pin).exists()
            assert not Path(bytecode).exists()
            assert not (root / "fs/maps" / str(pid)).exists()
            assert db.execute("SELECT count(*) FROM managed_programs").fetchone()[0] == 0
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 0
            print("PASS: Rust load persists across process exit; Go observes and unloads it")

            # Fail after kernel pins and bytecode exist. Both rows must roll back
            # and compensation must remove each pin before its map container.
            db.execute("CREATE TRIGGER reject_rust_load BEFORE INSERT ON managed_programs BEGIN SELECT RAISE(ABORT, 'injected persistence failure'); END")
            db.commit()
            failed = run(args.rust, root, "program", "load", "file", source,
                         "--programs", selected, success=False)
            assert "injected persistence failure" in failed.stderr, failed.stderr
            assert "0 unresolved cleanup resources" in failed.stderr, failed.stderr
            assert db.execute("SELECT count(*) FROM managed_programs").fetchone()[0] == 0
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 0
            assert not list((root / "fs").glob("prog_*"))
            assert not list((root / "fs/maps").iterdir())
            assert not list((root / "programs").iterdir())
            assert not list((root / ".staging").iterdir())
            print("PASS: injected persistence failure leaves no program, maps, bytecode, or rows")
            db.execute("DROP TRIGGER reject_rust_load")
            db.commit()

            # Publication fails after pinning; an outside staging directory
            # must remain untouched and kernel acquisitions must be compensated.
            staging = root / ".staging"
            staging.rename(root / "saved-staging")
            outside = Path(temporary) / "outside"
            outside.mkdir()
            (outside / "sentinel").write_text("keep")
            staging.symlink_to(outside, target_is_directory=True)
            failed = run(args.rust, root, "program", "load", "file", source,
                         "--programs", selected, success=False)
            assert "0 unresolved cleanup resources" in failed.stderr, failed.stderr
            assert not list((root / "fs").glob("prog_*"))
            assert not list((root / "fs/maps").iterdir())
            assert sorted(p.name for p in outside.iterdir()) == ["sentinel"]
            staging.rename(root / "rejected-staging-link")
            (root / "saved-staging").rename(staging)
            print("PASS: publication failure compensates pins and preserves outside state")

            # A failed write to stdout happens after commit and must not unload.
            with open("/dev/full", "w") as sink:
                delivered = subprocess.run([str(args.rust), "--runtime-dir", str(root),
                    "program", "load", "file", str(source), "--programs", selected],
                    stdout=sink, stderr=subprocess.PIPE, text=True, timeout=30)
            assert delivered.returncode == 1, delivered
            ids = db.execute("SELECT program_id FROM managed_programs").fetchall()
            assert len(ids) == 1, ids
            pid = ids[0][0]
            assert (root / "fs" / f"prog_{pid}").exists()
            run(args.go, root, "program", "get", pid)
            run(args.go, root, "program", "unload", pid)
            db.close()
            print("PASS: output-delivery failure preserves the committed program")
        finally:
            mount = root / "fs"
            if mount.is_mount():
                subprocess.run(["umount", str(mount)], check=True, timeout=10)


if __name__ == "__main__":
    main()
