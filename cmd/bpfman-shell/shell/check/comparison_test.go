package check

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestCheck_RecordNullComparison(t *testing.T) {
	t.Parallel()

	for _, expression := range []string{"$prog == null", "null == $prog", "$prog != null", "null != $prog"} {
		t.Run(expression, func(t *testing.T) {
			t.Parallel()
			issues := checkSource(t, "guard prog <- bpfman program get 7\nlet result = "+expression+"\nprint $result")
			assert.Empty(t, issues)
		})
	}
}

func TestCheck_RecordComparisonStillRejectsInvalidOperands(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		expression string
		wantError  string
	}{
		{"$prog < null", "null supports only == and !="},
		{"null >= $prog", "null supports only == and !="},
		{"$prog == $prog", "only scalars"},
	} {
		t.Run(tc.expression, func(t *testing.T) {
			t.Parallel()
			issues := checkSource(t, "guard prog <- bpfman program get 7\nlet result = "+tc.expression+"\nprint $result")
			require.Len(t, issues, 1)
			assert.Contains(t, issues[0].Msg, tc.wantError)
		})
	}
}
