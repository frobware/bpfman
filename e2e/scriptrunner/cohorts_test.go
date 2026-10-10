//go:build e2e

package scriptrunner

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

// Configuration names an additional shared-store pass. It does not bypass
// labels or implementation admission, and never changes the process environment.
func parseSharedScripts(raw string) (map[string]bool, error) {
	names := make(map[string]bool)
	if raw == "" {
		return names, nil
	}
	for name := range strings.SplitSeq(raw, ",") {
		if name == "" || filepath.Base(name) != name || strings.ContainsAny(name, `\/#`) || !strings.HasSuffix(name, ".bpfman") {
			return nil, fmt.Errorf("invalid BPFMAN_E2E_SHARED_SCRIPTS filename %q", name)
		}
		if names[name] {
			return nil, fmt.Errorf("duplicate shared script %q", name)
		}
		names[name] = true
	}
	return names, nil
}

func validateSharedScripts(t *testing.T, shared map[string]bool) {
	t.Helper()
	matches, err := filepath.Glob(filepath.Join(e2ePackageDir(t), "scripts", "*.bpfman"))
	if err != nil {
		t.Fatal(err)
	}
	selector := scriptLabelSelector(t)
	implementation := scriptImplementation(t)
	found := make(map[string]bool)
	for _, path := range matches {
		meta := readScriptMetadata(path)
		if meta.err != nil {
			t.Fatalf("read script metadata: %v", meta.err)
		}
		admitted := scriptImplementationSkipReason(implementation, meta.mode.Labels) == "" && scriptSelectorSkipReason(selector, meta.mode.Labels) == ""
		if admitted && meta.mode.Labels.Get("exclusive") == "true" {
			t.Fatalf("exclusive script %s cannot overlap runtime cohorts", filepath.Base(path))
		}
		if shared[filepath.Base(path)] {
			if !admitted {
				t.Fatalf("shared script %s is not admitted", filepath.Base(path))
			}
			found[filepath.Base(path)] = true
		}
	}
	for name := range shared {
		if !found[name] {
			t.Fatalf("shared script %s does not exist", name)
		}
	}
}

func scriptOutcome(failed, skipped bool) string {
	if failed {
		return "failed"
	}
	if skipped {
		return "skipped"
	}
	return "passed"
}

func emitScopedTimelineMarker(action, testName, scope, outcome string) error {
	return writeScriptTimelineMarker(scriptTimelineMarker{
		Time: time.Now(), Action: action, Package: bpfmanShellTestPackage,
		Test: testName, RuntimeScope: scope, Outcome: outcome,
	})
}

func TestScriptCohortConfiguration(t *testing.T) {
	t.Parallel()
	names, err := parseSharedScripts("First.bpfman,Second.bpfman")
	require.NoError(t, err)
	require.Equal(t, map[string]bool{"First.bpfman": true, "Second.bpfman": true}, names)
	names, err = parseSharedScripts("")
	require.NoError(t, err)
	require.Empty(t, names)
	for _, raw := range []string{"../First.bpfman", "/First.bpfman", `dir\First.bpfman`, "First.bpfman#1", "First", "First.bpfman,", "First.bpfman,First.bpfman"} {
		_, err := parseSharedScripts(raw)
		require.Error(t, err, raw)
	}
	require.Equal(t, "passed", scriptOutcome(false, false))
	require.Equal(t, "failed", scriptOutcome(true, false))
	require.Equal(t, "failed", scriptOutcome(true, true))
	require.Equal(t, "skipped", scriptOutcome(false, true))
}
