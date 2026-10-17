package runtime

import (
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/semantics"
	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/source"
)

func TestRuntime_StructuredNullComparison(t *testing.T) {
	t.Parallel()

	for name, value := range map[string]Value{
		"object": ValueFromAny(map[string]any{"image_url": "example/test:latest"}),
		"list":   ValueFromAny([]any{"example/test:latest"}),
		"record": ValueFromRecord(map[string]Value{"name": StringValue("test")}).WithKind(semantics.OriginProgram),
	} {
		for _, expression := range []string{"$value == null", "null == $value", "$value != null", "null != $value"} {
			t.Run(name+"/"+expression, func(t *testing.T) {
				t.Parallel()
				s := NewSession()
				s.Set("value", value)
				prog := parseProgram(t, "let result = "+expression)
				require.NoError(t, execParsedProgram(t, prog, &Env{Session: s}))
				result, ok := s.Get("result")
				require.True(t, ok)
				got, err := AsBool(result)
				require.NoError(t, err)
				assert.Equal(t, expression == "$value != null" || expression == "null != $value", got)
			})
		}
	}
}

func TestRuntime_NullComparisonStillRejectsInvalidOperands(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name       string
		value      Value
		expression string
		wantError  string
	}{
		{"absent_left", Value{}, "$value == null", "left side"},
		{"absent_right", Value{}, "null != $value", "right side"},
		{"object_comparison", ValueFromAny(map[string]any{}), "$value == $value", "only scalars"},
		{"list_comparison", ValueFromAny([]any{}), "$value != $value", "only scalars"},
		{"null_ordering_left", ValueFromAny(map[string]any{}), "null < $value", "null supports only == and !="},
		{"null_ordering_right", ValueFromAny([]any{}), "$value >= null", "null supports only == and !="},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s := NewSession()
			s.Set("value", tc.value)
			err := execParsedProgram(t, parseProgram(t, "let result = "+tc.expression), &Env{Session: s})
			require.ErrorContains(t, err, tc.wantError)
		})
	}
}

func TestRuntime_ImageSourceNullBranch(t *testing.T) {
	t.Parallel()

	for _, imageSource := range []string{"null", `{"image_url":"example/test:latest"}`} {
		t.Run(imageSource, func(t *testing.T) {
			t.Parallel()
			prog, err := ValueFromJSON([]byte(fmt.Sprintf(`{"record":{"load":{"image_source":%s}}}`, imageSource)))
			require.NoError(t, err)
			s := NewSession()
			s.Set("prog", prog)
			var calls []string
			env := &Env{
				Session: s,
				ExecCommand: func(args []Arg, _ source.Span) (Value, error) {
					calls = append(calls, argText0(recordedCall{args: args}))
					return Value{}, nil
				},
			}
			require.NoError(t, execParsedProgram(t, parseProgram(t, `
if $prog.record.load.image_source == null {
    file_source
} else {
    image_source
}`), env))
			if imageSource == "null" {
				assert.Equal(t, []string{"file_source"}, calls)
			} else {
				assert.Equal(t, []string{"image_source"}, calls)
			}
		})
	}
}
