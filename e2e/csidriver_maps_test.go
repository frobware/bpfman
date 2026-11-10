//go:build e2e

package e2e

import (
	"context"
	"io"
	"log/slog"
	"path/filepath"
	"testing"

	cilium "github.com/cilium/ebpf"
	"github.com/container-storage-interface/spec/lib/go/csi"
	"github.com/stretchr/testify/require"

	"github.com/bpfman/bpfman"
	csidriver "github.com/bpfman/bpfman/csi"
	"github.com/bpfman/bpfman/kernel"
	"github.com/bpfman/bpfman/manager"
	"github.com/bpfman/bpfman/platform/ebpf"
)

func TestCSIDriver_PublishesObjectMapsUnusedBySelectedFunction(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	env := NewTestEnv(t)
	programs, err := env.LoadFile(ctx, "testdata/bpf/csi_public_maps.bpf.o",
		[]manager.ProgramSpec{
			{Type: bpfman.ProgramTypeTracepoint, Name: "csi_first"},
			{Type: bpfman.ProgramTypeTracepoint, Name: "csi_second"},
		}, manager.LoadOpts{UserMetadata: map[string]string{csidriver.MetadataKeyProgramName: t.Name()}})
	require.NoError(t, err)
	for _, program := range programs {
		t.Cleanup(func() { require.NoError(t, env.Unload(ctx, program.Status.Kernel.ID)) })
	}
	require.Len(t, programs, 2)
	require.Less(t, programs[0].Record.ProgramID, programs[1].Record.ProgramID)
	selected, id, err := env.Manager.FindLoadedProgramByMetadata(ctx, csidriver.MetadataKeyProgramName, t.Name())
	require.NoError(t, err)
	require.Equal(t, programs[0].Record.ProgramID, id, "select the lowest-ID live match")

	mapID := func(path string) kernel.MapID {
		m, err := cilium.LoadPinnedMap(path, nil)
		require.NoError(t, err)
		info, err := m.Info()
		require.NoError(t, m.Close())
		require.NoError(t, err)
		id, available := info.ID()
		require.True(t, available)
		return kernel.MapID(id)
	}
	first := mapID(filepath.Join(selected.Handles.MapsDir.String(), "csi_first_map"))
	second := mapID(filepath.Join(selected.Handles.MapsDir.String(), "csi_second_map"))
	require.NotEqual(t, first, second)
	require.True(t, programs[0].Status.Kernel.HasMapIDs)
	require.Contains(t, programs[0].Status.Kernel.MapIDs, first)
	require.NotContains(t, programs[0].Status.Kernel.MapIDs, second, "the regression requires a published map unused by the selected function")
	require.True(t, programs[1].Status.Kernel.HasMapIDs)
	require.Contains(t, programs[1].Status.Kernel.MapIDs, second)
	require.NotContains(t, programs[1].Status.Kernel.MapIDs, first)

	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	csiRoot := filepath.Join(t.TempDir(), "csi-fs")
	d := csidriver.New("csi.bpfman.io", "test", "node-test", "unix:///unused.sock", logger,
		csidriver.WithProgramFinder(env.Manager),
		csidriver.WithKernel(ebpf.New(ebpf.WithLogger(logger))),
		csidriver.WithCSIFsRoot(csiRoot))
	const volumeID = "public-object-maps"
	target := filepath.Join(t.TempDir(), "target")
	podBpffs := filepath.Join(csiRoot, volumeID)
	t.Cleanup(func() { cleanupCSI(d, volumeID, target, podBpffs) })
	req := publishRequest(volumeID, target, t.Name(), "csi_first_map,csi_second_map", true, "")
	for range 2 {
		_, err = d.NodePublishVolume(ctx, req)
		require.NoError(t, err)
	}
	require.Equal(t, first, mapID(filepath.Join(target, "csi_first_map")))
	require.Equal(t, second, mapID(filepath.Join(target, "csi_second_map")))
	require.Equal(t, 1, countMountpoints(t, target))
	require.Equal(t, 1, countMountpoints(t, podBpffs))

	_, err = d.NodeUnpublishVolume(ctx, &csi.NodeUnpublishVolumeRequest{VolumeId: volumeID, TargetPath: target})
	require.NoError(t, err)
	require.Equal(t, 0, countMountpoints(t, target))
	require.Equal(t, 0, countMountpoints(t, podBpffs))
	require.NoDirExists(t, target)
	require.NoDirExists(t, podBpffs)
}
