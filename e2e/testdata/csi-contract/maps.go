package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/cilium/ebpf"
	"github.com/container-storage-interface/spec/lib/go/csi"

	pb "github.com/bpfman/bpfman/server/pb"
)

// publishedMaps distinguishes the public object map set from the maps used by
// one function. These assertions are identical for Go, Rust and CSI-only Rust.
func publishedMaps(ctx context.Context, b management, node csi.NodeClient) (result error) {
	app := *label + "-public-maps"
	loaded, err := b.Load(ctx, &pb.LoadRequest{
		Bytecode: &pb.BytecodeLocation{Location: &pb.BytecodeLocation_File{File: filepath.Join(*objects, "csi_public_maps.bpf.o")}},
		Info: []*pb.LoadInfo{
			{Name: "csi_first", ProgramType: pb.BpfmanProgramType_TRACEPOINT},
			{Name: "csi_second", ProgramType: pb.BpfmanProgramType_TRACEPOINT},
		},
		Metadata: map[string]string{"bpfman.io/ProgramName": app},
	})
	if err != nil {
		return err
	}
	req := request("public-maps")
	req.VolumeContext["csi.bpfman.io/program"] = app
	req.VolumeContext["csi.bpfman.io/maps"] = "csi_first_map,csi_second_map"
	defer func() {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_, err := node.NodeUnpublishVolume(cleanupCtx, &csi.NodeUnpublishVolumeRequest{VolumeId: req.VolumeId, TargetPath: req.TargetPath})
		result = errors.Join(result, err)
		for _, path := range []string{req.TargetPath, filepath.Join(*backing, req.VolumeId)} {
			if count(path) != 0 {
				result = errors.Join(result, fmt.Errorf("public-map cleanup left a mount at %s", path))
			}
			if _, err := os.Stat(path); !os.IsNotExist(err) {
				result = errors.Join(result, fmt.Errorf("public-map cleanup did not remove %s", path))
			}
		}
		for _, program := range loaded.Programs {
			_, err := b.Unload(cleanupCtx, &pb.UnloadRequest{Id: program.KernelInfo.Id})
			result = errors.Join(result, err)
		}
	}()
	if len(loaded.Programs) != 2 {
		return fmt.Errorf("public-map fixture must load two functions")
	}
	first := loaded.Programs[0].KernelInfo.Id
	second := loaded.Programs[1].KernelInfo.Id
	if first >= second {
		return fmt.Errorf("fixture functions must have ascending program IDs")
	}
	ids := make(map[string]ebpf.MapID)
	for _, name := range []string{"csi_first_map", "csi_second_map"} {
		id, err := pinnedMapID(filepath.Join(*root, "fs/maps", fmt.Sprint(first), name))
		if err != nil {
			return err
		}
		ids[name] = id
	}
	for _, function := range []struct {
		id           uint32
		used, unused string
	}{{first, "csi_first_map", "csi_second_map"}, {second, "csi_second_map", "csi_first_map"}} {
		program, err := ebpf.NewProgramFromID(ebpf.ProgramID(function.id))
		if err != nil {
			return err
		}
		info, err := program.Info()
		closeErr := program.Close()
		if err != nil || closeErr != nil {
			return errors.Join(err, closeErr)
		}
		used, available := info.MapIDs()
		if !available || !slices.Contains(used, ids[function.used]) || slices.Contains(used, ids[function.unused]) {
			return fmt.Errorf("fixture must distinguish published maps from each function's used maps")
		}
	}
	for range 2 {
		if _, err := node.NodePublishVolume(ctx, req); err != nil {
			return fmt.Errorf("publish public maps including an unused function map: %w", err)
		}
	}
	if count(req.TargetPath) != 1 || count(filepath.Join(*backing, req.VolumeId)) != 1 {
		return fmt.Errorf("public-map publication stacked mounts")
	}
	for name, want := range ids {
		got, err := pinnedMapID(filepath.Join(req.TargetPath, name))
		if err != nil {
			return err
		}
		if got != want {
			return fmt.Errorf("published %s has map ID %d, want %d", name, got, want)
		}
	}
	return nil
}

func pinnedMapID(path string) (ebpf.MapID, error) {
	m, err := ebpf.LoadPinnedMap(path, nil)
	if err != nil {
		return 0, err
	}
	info, err := m.Info()
	closeErr := m.Close()
	if err != nil || closeErr != nil {
		return 0, errors.Join(err, closeErr)
	}
	id, available := info.ID()
	if !available {
		return 0, fmt.Errorf("map ID unavailable for %s", path)
	}
	return id, nil
}
