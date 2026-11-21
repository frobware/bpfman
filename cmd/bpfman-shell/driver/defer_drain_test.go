package driver

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/runtime"
	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/source"
	"github.com/bpfman/bpfman/cmd/internal/cli"
)

// TestCancelledRun_DrainsDefersUnderCleanupContext pins the
// interrupt-cleanup contract: cancelling the root context mid-script
// (operator ^C, runner failfast abort, script timeout) aborts the
// in-flight statement, but the program-level defers still execute,
// under a fresh bounded context detached from the cancellation.
// Without that detachment every deferred action dies immediately
// with "context canceled" and the script leaks whatever its defers
// guarded.
func TestCancelledRun_DrainsDefersUnderCleanupContext(t *testing.T) {
	t.Parallel()

	marker := filepath.Join(t.TempDir(), "defer-ran")
	src := fmt.Sprintf("defer exec touch %s\nguard _ <- exec sleep 30\n", marker)

	ctx, cancel := context.WithCancelCause(context.Background())
	defer cancel(nil)
	go func() {
		time.Sleep(300 * time.Millisecond)
		cancel(fmt.Errorf("interrupted by signal: %w", context.Canceled))
	}()

	cli := &cli.CLI{Out: io.Discard, Err: io.Discard}
	start := time.Now()
	err := Run(ctx, Config{
		CLI:        cli,
		LineReader: NewScannerReader(strings.NewReader(src), nil),
		File:       "cancel-drain.bpfman",
	})
	elapsed := time.Since(start)

	require.Error(t, err, "a cancelled run must not report success")
	require.Less(t, elapsed, 10*time.Second, "cancellation must abort the sleep, not wait it out")

	_, statErr := os.Stat(marker)
	require.NoError(t, statErr, "deferred command must run under the cleanup context after cancellation")
}

// TestCancelledRun_DeferDrainIsBounded pins the other half of the
// contract: the cleanup context is time-boxed, so a deferred command
// that blocks cannot make interrupt shutdown unbounded.
//
//nolint:paralleltest // mutates the package-level deferDrainBudget; cannot run in parallel.
func TestCancelledRun_DeferDrainIsBounded(t *testing.T) {
	prev := deferDrainBudget
	deferDrainBudget = 500 * time.Millisecond
	t.Cleanup(func() { deferDrainBudget = prev })

	src := "defer exec sleep 30\nguard _ <- exec sleep 30\n"

	ctx, cancel := context.WithCancelCause(context.Background())
	defer cancel(nil)
	go func() {
		time.Sleep(300 * time.Millisecond)
		cancel(fmt.Errorf("interrupted by signal: %w", context.Canceled))
	}()

	cli := &cli.CLI{Out: io.Discard, Err: io.Discard}
	start := time.Now()
	err := Run(ctx, Config{
		CLI:        cli,
		LineReader: NewScannerReader(strings.NewReader(src), nil),
		File:       "cancel-drain-bounded.bpfman",
	})
	elapsed := time.Since(start)

	require.Error(t, err)
	require.Less(t, elapsed, 5*time.Second, "a blocking defer must be cut off by the drain budget, not waited out")
}

func TestCancelledRunPreservesCleanupError(t *testing.T) {
	t.Parallel()

	cause := fmt.Errorf("interrupted by signal: %w", context.Canceled)
	cleanup := errors.New("remove adapter file: directory not empty")
	for _, tc := range []struct {
		name        string
		commandErr  error
		wantCleanup bool
	}{
		{"bare cancellation", context.Canceled, false},
		{"bare cancellation joined with cleanup", errors.Join(context.Canceled, cleanup), true},
		{"cause joined with cleanup", errors.Join(cause, cleanup), true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			ctx, cancel := context.WithCancelCause(t.Context())
			defer cancel(nil)
			err := Run(ctx, Config{
				CLI:        &cli.CLI{Out: io.Discard, Err: io.Discard},
				LineReader: NewScannerReader(strings.NewReader("guard _ <- test-command\n"), nil),
				File:       "cancel-cleanup.bpfman",
				BindFallback: func(context.Context, *cli.CLI, *runtime.Session, *runtime.Env, []runtime.Arg, SourceLoc, source.Span) (bool, runtime.BindResult, error) {
					cancel(cause)
					return true, runtime.BindResult{}, tc.commandErr
				},
			})
			require.ErrorIs(t, err, cause)
			require.Equal(t, 1, strings.Count(err.Error(), cause.Error()))
			if tc.wantCleanup {
				require.ErrorIs(t, err, cleanup)
			} else {
				require.NotErrorIs(t, err, cleanup)
			}
		})
	}
}
