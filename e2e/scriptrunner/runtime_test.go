//go:build e2e

package scriptrunner

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// Isolation removes unrelated inventory and writer contention from acceptance.
// Shared-runtime stress remains available by leaving the opt-in unset.
// Verify residue before unmounting, including when the script itself failed.
func isolatedScriptRuntime(t *testing.T, env []string) string {
	t.Helper()
	root := filepath.Join(t.TempDir(), "runtime")
	env = append(env, "BPFMAN_RUNTIME_DIR="+root)
	t.Cleanup(func() {
		defer func() {
			if err := syscall.Unmount(filepath.Join(root, "fs"), 0); err != nil &&
				!errors.Is(err, syscall.EINVAL) && !errors.Is(err, syscall.ENOENT) {
				t.Errorf("unmount script bpffs: %v", err)
			}
		}()
		// A script that fails before running the CLI need not create a runtime.
		if _, err := os.Stat(root); errors.Is(err, os.ErrNotExist) {
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
		binary := os.Getenv("BPFMAN_BIN")
		if binary == "" {
			binary = "bpfman"
		}
		for _, collection := range []string{"program", "link", "dispatcher"} {
			cmd := exec.CommandContext(ctx, binary, collection, "list", "-o", "json")
			cmd.Env = env
			var stderr strings.Builder
			cmd.Stderr = &stderr
			out, err := cmd.Output()
			if err != nil {
				t.Errorf("verify script %s inventory: %v\n%s", collection, err, stderr.String())
				continue
			}
			var inventory map[string]json.RawMessage
			if err := json.Unmarshal(out, &inventory); err != nil {
				t.Errorf("decode script %s inventory: %v", collection, err)
				continue
			}
			var rows []json.RawMessage
			raw := inventory[collection+"s"]
			if err := json.Unmarshal(raw, &rows); err != nil || string(raw) == "null" || len(rows) != 0 {
				t.Errorf("script left %s inventory: %s (decode: %v)", collection, raw, err)
			}
		}
		for _, collection := range []string{"fs/maps", "fs/links", "fs/xdp", "fs/tc-ingress", "fs/tc-egress", "tc", "programs", ".staging"} {
			entries, err := os.ReadDir(filepath.Join(root, collection))
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			if err != nil || len(entries) != 0 {
				t.Errorf("script left artifacts in %s: %v (read: %v)", collection, entries, err)
			}
		}
		entries, err := os.ReadDir(filepath.Join(root, "fs"))
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			t.Errorf("inspect script bpffs: %v", err)
		}
		for _, entry := range entries {
			if strings.HasPrefix(entry.Name(), "prog_") {
				t.Errorf("script left program pin %s", entry.Name())
			}
		}
	})
	return root
}
