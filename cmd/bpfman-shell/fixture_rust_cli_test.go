package main

import (
	"bytes"
	"context"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestRustShellFixtureContracts reuses the Go workflow expectations at the
// CLI boundary. The Go tests continue to assert their internal session state.
func TestRustShellFixtureContracts(t *testing.T) {
	binary := os.Getenv("BPFMAN_RUST_SHELL_TEST_BIN")
	if binary == "" {
		t.Skip("set BPFMAN_RUST_SHELL_TEST_BIN to run the Rust shell fixture contracts")
	}
	root := workflowFixtureDir("")
	count := 0
	require.NoError(t, filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.Name() != "expect.yaml" {
			return err
		}
		fixture, err := filepath.Rel(root, filepath.Dir(path))
		if err != nil {
			return err
		}
		count++
		t.Run(fixture, func(t *testing.T) {
			t.Parallel()
			tc := applyFixtureExpectation(t, fixtureWorkflowCase{fixture: fixture})
			ctx, cancel := context.WithTimeout(t.Context(), fixtureWorkflowTimeout)
			defer cancel()
			var stdout, stderr bytes.Buffer
			cmd := exec.CommandContext(ctx, binary, workflowFixtureMain(fixture))
			cmd.Stdout, cmd.Stderr = &stdout, &stderr
			runErr := cmd.Run()
			if tc.wantErrText != "" || tc.wantErrIs != nil {
				var exited *exec.ExitError
				require.ErrorAs(t, runErr, &exited, stderr.String())
				require.Equal(t, 1, exited.ExitCode(), stderr.String())
				if tc.wantErrText != "" {
					assert.Contains(t, stderr.String(), tc.wantErrText)
				}
			} else {
				require.NoError(t, runErr, stderr.String())
			}
			assertFixtureWorkflowOutput(t, tc, stdout.String(), stderr.String())
			for _, counter := range []struct {
				prefix string
				want   *int
			}{
				{"[assert] FAIL:", tc.wantAssertFails},
				{"[defer] FAIL at ", tc.wantDeferFails},
				{"[job] FAIL ", tc.wantJobLeaks},
			} {
				assert.Equal(t, wantCounter(counter.want), strings.Count(stderr.String(), counter.prefix), "counter for %q", counter.prefix)
			}
		})
		return nil
	}))
	require.Positive(t, count, "expected workflow fixtures under %s", root)
}
