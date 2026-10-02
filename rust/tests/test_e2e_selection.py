"""Exercise the real Make recipe using unprivileged process fixtures."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class BinarySelection(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bpfman-selection-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.go = self.root / "go"
        self.rust = self.root / "rust"
        self.tools = self.root / "tools"
        for directory in (self.go, self.rust, self.tools):
            directory.mkdir()
        self.executable(self.tools / "sudo", 'exec "$@"\n')
        self.executable(self.go / "bpfman", 'echo go\n')
        self.executable(self.rust / "bpfman", 'echo rust\n')
        # Stand in for the Go test binary, then the shell. Exercise both the
        # explicit builtin path and raw/nested-shell PATH resolution.
        self.executable(self.go / "e2e-scripts.test", 'exec bpfman-shell\n')
        self.executable(
            self.go / "bpfman-shell",
            '"$BPFMAN_BIN"\nbpfman\nsh -c "exec bpfman"\n',
        )

    @staticmethod
    def executable(path, body):
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)

    def run_recipe(self, *overrides):
        env = dict(os.environ)
        # A stale caller override must not split the typed and raw paths.
        env["BPFMAN_BIN"] = str(self.go / "bpfman")
        env["PATH"] = str(self.tools) + os.pathsep + env["PATH"]
        return subprocess.run(
            ["make", "--no-print-directory", "-s", "run-e2e-scripts",
             f"BIN_DIR={self.go}", *overrides],
            cwd=ROOT, env=env, text=True, capture_output=True, check=False,
        )

    def test_default_selects_go_for_all_paths(self):
        result = self.run_recipe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["go", "go", "go"])

    def test_override_selects_rust_for_all_paths(self):
        result = self.run_recipe(f"BPFMAN_UNDER_TEST={self.rust / 'bpfman'}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["rust", "rust", "rust"])

    def test_missing_executable_fails_before_runner(self):
        result = self.run_recipe(f"BPFMAN_UNDER_TEST={self.root / 'missing/bpfman'}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not executable", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_different_basename_is_rejected_to_prevent_path_fallback(self):
        binary = self.rust / "bpfman-other"
        self.executable(binary, "echo wrong\n")
        result = self.run_recipe(f"BPFMAN_UNDER_TEST={binary}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("called bpfman", result.stderr)
        self.assertEqual(result.stdout, "")
