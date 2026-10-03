#!/usr/bin/env python3
"""Local tracepoint acceptance: run only in a fresh private mount namespace.

The Go CLI is an independent observer for the Rust load/unload lifecycle. This is
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

            loaded = json.loads(run(args.rust, root, "program", "load", "file", source, "--programs", selected,
                "--application", "rust-slice", "--metadata", "test=acceptance", "-o", "json").stdout)["programs"][0]
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
            rust_observation = json.loads(run(args.rust, root, "program", "get", pid, "-o", "json").stdout)
            assert rust_observation == observation, {"rust": rust_observation, "go": observation}
            assert loaded["record"] == observation["record"]
            assert loaded["status"]["kernel"] == observation["status"]["kernel"]
            assert loaded["status"]["stats"] is None
            assert all(m["pin_path"] == "" and not m["present"] for m in loaded["status"]["maps"])
            rust_list = json.loads(run(args.rust, root, "program", "list", "-o", "json").stdout)
            go_list = json.loads(run(args.go, root, "program", "list", "-o", "json").stdout)
            assert rust_list == go_list, {"rust": rust_list, "go": go_list}
            assert run(args.rust, root, "program", "get", pid).stdout == run(args.go, root, "program", "get", pid).stdout
            print("PASS: Rust get/list JSON and get text match Go observations; load preserves its distinct shape")
            run(args.rust, root, "program", "unload", pid)
            assert not Path(pin).exists()
            assert not Path(bytecode).exists()
            assert not (root / "fs/maps" / str(pid)).exists()
            assert db.execute("SELECT count(*) FROM managed_programs").fetchone()[0] == 0
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 0
            assert not run(args.rust, root, "program", "list", "-q").stdout.strip()
            print("PASS: Rust load/list/unload leaves no owned residue; Go observes the live program")

            def load(binary=args.rust):
                run(binary, root, "program", "load", "file", source, "--programs", selected)
                return db.execute("SELECT max(program_id) FROM managed_programs").fetchone()[0]

            def present(program):
                assert (root / "fs" / f"prog_{program}").exists()
                assert (root / "fs/maps" / str(program) / "tracepoint_stats_map").exists()
                assert (root / "programs" / str(program) / "bytecode.o").exists()

            # Teardown scope must be established before unpinning. A separate
            # program stays alive throughout these failures and partial unloads.
            unrelated = load()
            pid = load()
            for change, restore, diagnostic in [
                ("UPDATE managed_programs SET program_type='xdp' WHERE program_id=?", "UPDATE managed_programs SET program_type='tracepoint' WHERE program_id=?", "only tracepoints"),
                ("INSERT INTO links(kind,kernel_prog_id,created_at) VALUES('tracepoint',?,'now')", "DELETE FROM links WHERE kernel_prog_id=?", "program has links"),
                ("INSERT INTO shared_map_pins VALUES('shared',?)", "DELETE FROM shared_map_pins WHERE program_id=?", "private map sets"),
            ]:
                db.execute(change, (pid,)); db.commit()
                failed = run(args.rust, root, "program", "unload", pid, success=False)
                assert diagnostic in failed.stderr, failed.stderr
                present(pid); present(unrelated)
                db.execute(restore, (pid,)); db.commit()
            # Private ownership cannot be inferred from directory names alone.
            db.execute("UPDATE managed_programs SET map_set_id=? WHERE program_id=?", (pid, unrelated)); db.commit()
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "private map sets" in failed.stderr, failed.stderr
            present(pid); present(unrelated)
            db.execute("UPDATE managed_programs SET map_set_id=program_id WHERE program_id=?", (unrelated,)); db.commit()
            old_pin = str(root / "fs" / f"prog_{pid}")
            db.execute("UPDATE managed_programs SET pin_path='/' WHERE program_id=?", (pid,)); db.commit()
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "canonical runtime" in failed.stderr, failed.stderr
            present(pid); present(unrelated)
            db.execute("UPDATE managed_programs SET pin_path=? WHERE program_id=?", (old_pin, pid)); db.commit()

            # A different live program at the canonical pin name is not ours.
            pin = root / "fs" / f"prog_{pid}"
            other_pin = root / "fs" / f"prog_{unrelated}"
            saved = root / "fs/saved_program"
            pin.rename(saved); other_pin.rename(pin)
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "different kernel identity" in failed.stderr, failed.stderr
            pin.rename(other_pin); saved.rename(pin)
            present(pid); present(unrelated)

            # Likewise, another program's map must not be adopted as a private map.
            map_pin = root / "fs/maps" / str(pid) / "tracepoint_stats_map"
            other_map = root / "fs/maps" / str(unrelated) / "tracepoint_stats_map"
            saved_map = root / "fs/saved_map"
            map_pin.rename(saved_map); other_map.rename(map_pin)
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "does not belong" in failed.stderr, failed.stderr
            map_pin.rename(other_map); saved_map.rename(map_pin)
            present(pid); present(unrelated)
            print("PASS: unsupported relationships, noncanonical paths, and foreign BPF identities are refused before unpinning")

            # Record deletion is the returned post-unpin failure. Independent
            # bytecode cleanup still runs, while map GC waits for row deletion.
            db.execute("CREATE TRIGGER reject_unload BEFORE DELETE ON managed_programs BEGIN SELECT RAISE(ABORT,'injected unload record failure'); END"); db.commit()
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "injected unload record failure" in failed.stderr, failed.stderr
            assert not (root / "fs" / f"prog_{pid}").exists()
            assert not (root / "programs" / str(pid)).exists()
            assert (root / "fs/maps" / str(pid)).exists()
            assert db.execute("SELECT count(*) FROM managed_programs WHERE program_id=?", (pid,)).fetchone()[0] == 1
            present(unrelated)
            # Repeating an unchanged failed condition cannot magically succeed.
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "injected unload record failure" in failed.stderr, failed.stderr
            assert (root / "fs/maps" / str(pid)).exists()
            db.execute("DROP TRIGGER reject_unload"); db.commit()
            run(args.rust, root, "program", "unload", pid)
            assert not (root / "fs/maps" / str(pid)).exists()
            assert db.execute("SELECT count(*) FROM map_sets WHERE id=?", (pid,)).fetchone()[0] == 0
            present(unrelated)
            print("PASS: record deletion failure preserves maps, cleans bytecode, and succeeds only after the injected fault is removed")

            # Map-set GC failures are warnings after successful record deletion.
            # A later CLI request by ID cannot recover residue once that row is gone.
            pid = load()
            db.execute("CREATE TRIGGER reject_map_gc BEFORE DELETE ON map_sets BEGIN SELECT RAISE(ABORT,'injected map-set GC failure'); END"); db.commit()
            warning = run(args.rust, root, "program", "unload", pid)
            assert "Warning: MapSet" in warning.stderr and "injected map-set GC failure" in warning.stderr, warning.stderr
            assert not (root / "fs" / f"prog_{pid}").exists()
            assert not (root / "fs/maps" / str(pid)).exists()
            assert not (root / "programs" / str(pid)).exists()
            assert db.execute("SELECT count(*) FROM managed_programs WHERE program_id=?", (pid,)).fetchone()[0] == 0
            assert db.execute("SELECT count(*) FROM map_sets WHERE id=?", (pid,)).fetchone()[0] == 1
            failed = run(args.rust, root, "program", "unload", pid, success=False)
            assert "not found" in failed.stderr, failed.stderr
            present(unrelated)
            db.execute("DROP TRIGGER reject_map_gc")
            db.execute("DELETE FROM map_sets WHERE id=?", (pid,)); db.commit()
            run(args.rust, root, "program", "unload", unrelated)
            print("PASS: post-record GC failure reports a warning and preserves unrelated state")

            pid = load(args.go)
            run(args.rust, root, "program", "unload", pid)
            assert db.execute("SELECT count(*) FROM managed_programs").fetchone()[0] == 0
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 0
            print("PASS: Rust unload also accepts Go-created private tracepoints")

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

            # Observation happens after commit. Corrupting the just-written
            # record must report that boundary, retaining every committed artifact.
            db.execute("CREATE TRIGGER fail_observation AFTER INSERT ON managed_programs BEGIN UPDATE managed_programs SET created_at='invalid timestamp' WHERE program_id=NEW.program_id; END")
            db.commit()
            failed = run(args.rust, root, "program", "load", "file", source,
                         "--programs", selected, "-o", "json", success=False)
            assert "was committed" in failed.stderr, failed.stderr
            pid = db.execute("SELECT program_id FROM managed_programs").fetchone()[0]
            present(pid)
            assert db.execute("SELECT count(*) FROM map_sets").fetchone()[0] == 1
            created = json.loads((root / "programs" / str(pid) / "provenance.json").read_text())["loaded_at"]
            db.execute("DROP TRIGGER fail_observation")
            db.execute("UPDATE managed_programs SET created_at=? WHERE program_id=?", (created, pid))
            db.commit()
            run(args.rust, root, "program", "get", pid, "-o", "json")
            run(args.rust, root, "program", "unload", pid)
            print("PASS: observation failure after commit retains the loaded program and every owned artifact")

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
