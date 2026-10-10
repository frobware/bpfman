//go:build e2e

package scriptrunner

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestScriptCaseEnvironmentIsConfinedAndTyped(t *testing.T) {
	t.Parallel()
	inherited := []string{"BPFMAN_RUNTIME_DIR=/owned", "BPFMAN_BIN=/binary", "BPFMAN_E2E_CASE=stale"}
	params := map[string]json.RawMessage{"fallback": json.RawMessage(`1`), "proceed": json.RawMessage(`"pass,redirect,dispatcher_return"`)}
	env, err := scriptCaseEnvironment(inherited, params)
	require.NoError(t, err)
	require.Equal(t, []string{"BPFMAN_RUNTIME_DIR=/owned", "BPFMAN_BIN=/binary", `BPFMAN_E2E_CASE={"fallback":1,"proceed":"pass,redirect,dispatcher_return"}`}, env)
	require.Equal(t, "BPFMAN_E2E_CASE=stale", inherited[2])
	plain, err := scriptCaseEnvironment(inherited, nil)
	require.NoError(t, err)
	require.Equal(t, inherited[:2], plain)
	_, err = scriptCaseEnvironment(inherited, map[string]json.RawMessage{"bad": json.RawMessage(`invalid`)})
	require.Error(t, err)
}
