package driver

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/runtime"
)

func TestArgTexts(t *testing.T) {
	t.Parallel()

	args := []runtime.Arg{
		runtime.WordArg{Text: "show"},
		runtime.WordArg{Text: "program"},
		runtime.ScalarValueArg{Text: "42"},
	}
	got := ArgTexts(args)
	assert.Equal(t, []string{"show", "program", "42"}, got)
}

func TestArgTexts_Empty(t *testing.T) {
	t.Parallel()

	got := ArgTexts(nil)
	assert.Empty(t, got)
}

func TestArgTexts_StructuredValueArg(t *testing.T) {
	t.Parallel()

	args := []runtime.Arg{
		runtime.WordArg{Text: "show"},
		runtime.WordArg{Text: "program"},
		runtime.StructuredValueArg{Name: "prog", Value: runtime.ValueFromMap(map[string]any{"id": "42"})},
	}
	got := ArgTexts(args)
	assert.Equal(t, []string{"show", "program", "$prog"}, got)
}

func TestArgTexts_QuotedArg(t *testing.T) {
	t.Parallel()

	args := []runtime.Arg{
		runtime.WordArg{Text: "load"},
		runtime.QuotedArg{Text: "my file.o"},
	}
	got := ArgTexts(args)
	assert.Equal(t, []string{"load", "my file.o"}, got)
}

func TestRemoveAdapterFilesContinuesAfterFailure(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	nonempty := filepath.Join(root, "nonempty")
	require.NoError(t, os.Mkdir(nonempty, 0o700))
	require.NoError(t, os.WriteFile(filepath.Join(nonempty, "keep"), nil, 0o600))
	remove := filepath.Join(root, "remove")
	require.NoError(t, os.WriteFile(remove, nil, 0o600))

	err := RemoveAdapterFiles([]string{filepath.Join(root, "missing"), nonempty, remove})
	require.ErrorContains(t, err, nonempty)
	_, err = os.Stat(remove)
	require.ErrorIs(t, err, os.ErrNotExist)
}
