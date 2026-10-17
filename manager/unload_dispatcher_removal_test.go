package manager_test

import (
	"context"
	"errors"
	"os"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/bpfman/bpfman"
	"github.com/bpfman/bpfman/dispatcher"
	"github.com/bpfman/bpfman/manager"
	"github.com/bpfman/bpfman/platform"
)

// Successful operations delegate unchanged to the real SQLite store. A failed
// observation or deletion cannot authorize skipping the dispatcher's members.
type dispatcherRemovalFaultStore struct {
	platform.Store
	lookupErr error
	deleteErr error
}

func (s *dispatcherRemovalFaultStore) GetDispatcherSnapshot(ctx context.Context, key dispatcher.Key) (platform.DispatcherSnapshot, error) {
	if s.lookupErr != nil {
		return platform.DispatcherSnapshot{}, s.lookupErr
	}
	return s.Store.GetDispatcherSnapshot(ctx, key)
}

func (s *dispatcherRemovalFaultStore) DeleteDispatcherSnapshot(ctx context.Context, key dispatcher.Key) error {
	if s.deleteErr != nil {
		return s.deleteErr
	}
	return s.Store.DeleteDispatcherSnapshot(ctx, key)
}

// Given repeated members, an unexplained missing snapshot or failed deletion
// must stop unload. Only this unload's successful removal consumes later members.
func TestUnload_DispatcherRemovalFailuresRetainProgramForRetry(t *testing.T) {
	t.Parallel()
	for _, fault := range []string{"missing-snapshot", "delete-snapshot"} {
		t.Run(fault, func(t *testing.T) {
			t.Parallel()
			var store *dispatcherRemovalFaultStore
			fix := newTestFixtureWithStore(t, func(base platform.Store) platform.Store {
				store = &dispatcherRemovalFaultStore{Store: base}
				return store
			})
			ctx := context.Background()
			netns := fix.TempNetnsPath()
			spec, err := bpfman.NewLoadSpec(fix.BytecodeFile("tc.o"), "tc_pass", bpfman.ProgramTypeTC)
			require.NoError(t, err)
			prog, err := fix.Load(ctx, spec, manager.LoadOpts{})
			require.NoError(t, err)
			for _, priority := range []int{50, 60} {
				attach, err := bpfman.NewTCAttachSpec(prog.Record.ProgramID, "eth0", bpfman.TCDirectionIngress, priority)
				require.NoError(t, err)
				_, err = fix.Attach(ctx, attach.WithNetns(netns))
				require.NoError(t, err)
			}
			before, err := store.ListLinksByProgram(ctx, prog.Record.ProgramID)
			require.NoError(t, err)
			require.Len(t, before, 2)
			require.NoError(t, os.Remove(netns))
			want := platform.ErrRecordNotFound
			if fault == "missing-snapshot" {
				store.lookupErr = want
			} else {
				want = errors.New("injected snapshot deletion failure")
				store.deleteErr = want
			}
			require.ErrorIs(t, fix.Unload(ctx, prog.Record.ProgramID), want)
			_, err = store.Get(ctx, prog.Record.ProgramID)
			require.NoError(t, err, "program record remains retryable")
			_, err = fix.Kernel.GetProgramByID(ctx, prog.Record.ProgramID)
			require.NoError(t, err, "program pin survives failed dispatcher removal")
			after, err := store.ListLinksByProgram(ctx, prog.Record.ProgramID)
			require.NoError(t, err)
			assert.Equal(t, before, after, "failed removal cannot consume member records")
			store.lookupErr = nil
			store.deleteErr = nil
			require.NoError(t, fix.Unload(ctx, prog.Record.ProgramID), "retry consumes the successfully removed scope once")
			fix.AssertDatabaseEmpty()
			fix.AssertKernelEmpty()
		})
	}
}
