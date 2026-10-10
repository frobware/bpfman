//go:build e2e

package scriptrunner

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
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
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			waits, err := unmountScriptBpffs(ctx, filepath.Join(root, "fs"))
			if err != nil {
				t.Errorf("unmount script bpffs: %v", err)
			}
			if waits > 0 {
				t.Logf("bpffs unmount waited for busy references: polls=%d", waits)
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

// Inventory and pin checks precede this release. A transient mount reference
// must drain before normal unmount; persistent holders still fail cleanup.
// Never use lazy/forced unmount, which would hide unresolved ownership.
func unmountScriptBpffs(ctx context.Context, path string) (int, error) {
	waits := 0
	for {
		err := syscall.Unmount(path, 0)
		if err == nil || errors.Is(err, syscall.EINVAL) || errors.Is(err, syscall.ENOENT) {
			return waits, nil
		}
		if !errors.Is(err, syscall.EBUSY) {
			return waits, err
		}
		select {
		case <-ctx.Done():
			return waits, fmt.Errorf("mount references did not drain: %w; %w", err, context.Cause(ctx))
		case <-time.After(10 * time.Millisecond):
			waits++
		}
	}
}

func TestScriptBpffsUnmountWaitsForReleasedDirectory(t *testing.T) {
	t.Parallel()
	path, held := busyScriptMount(t)
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()
	released := make(chan error, 1)
	go func() {
		time.Sleep(20 * time.Millisecond)
		released <- held.Close()
	}()
	waits, err := unmountScriptBpffs(ctx, path)
	if releaseErr := <-released; releaseErr != nil {
		t.Fatal(releaseErr)
	}
	if err != nil {
		t.Fatalf("did not release mount: polls=%d err=%v", waits, err)
	}
	t.Logf("normal unmount completed after directory release: polls=%d", waits)
}

func TestScriptBpffsUnmountRejectsPersistentDirectoryHolder(t *testing.T) {
	t.Parallel()
	path, held := busyScriptMount(t)
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Millisecond)
	defer cancel()
	_, err := unmountScriptBpffs(ctx, path)
	if !errors.Is(err, syscall.EBUSY) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("persistent mount reference was accepted: %v", err)
	}
	// A failed release must leave the mount attached for explicit recovery.
	if err := syscall.Unmount(path, 0); !errors.Is(err, syscall.EBUSY) {
		t.Fatalf("busy mount was detached: %v", err)
	}
	if err := held.Close(); err != nil {
		t.Fatal(err)
	}
	recovery, stop := context.WithTimeout(t.Context(), time.Second)
	defer stop()
	if _, err := unmountScriptBpffs(recovery, path); err != nil {
		t.Fatal(err)
	}
}

func busyScriptMount(t *testing.T) (string, *os.File) {
	t.Helper()
	path := t.TempDir()
	if err := syscall.Mount("bpf", path, "bpf", 0, ""); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := syscall.Unmount(path, 0); err != nil && !errors.Is(err, syscall.EINVAL) {
			t.Errorf("release contract bpffs: %v", err)
		}
	})
	held, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = held.Close() })
	if err := syscall.Unmount(path, 0); !errors.Is(err, syscall.EBUSY) {
		t.Fatalf("directory FD did not retain mount: %v", err)
	}
	return path, held
}
