// CSI wire contract shared by the production Go and Rust daemons.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/cilium/ebpf"
	"github.com/container-storage-interface/spec/lib/go/csi"
	"golang.org/x/sys/unix"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	pb "github.com/bpfman/bpfman/server/pb"
)

var socket = flag.String("socket", "", "bpfman socket")
var csiSocket = flag.String("csi-socket", "", "CSI socket")
var objects = flag.String("objects", "", "BPF fixtures")
var root = flag.String("runtime", "", "runtime root")
var backing = flag.String("backing", "", "per-volume bpffs root")
var lane = flag.String("lane", "exercise", "exercise, persist-create, persist-read, identity")
var label = flag.String("label", "", "unique test label")

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func require(ok bool, what string) {
	if !ok {
		check(fmt.Errorf("CSI contract: %s", what))
	}
}
func code(err error, want codes.Code) {
	require(status.Code(err) == want, fmt.Sprintf("got %v, want %v", err, want))
}
func connect(path string) *grpc.ClientConn {
	c, e := grpc.NewClient("unix://"+path, grpc.WithTransportCredentials(insecure.NewCredentials()))
	check(e)
	return c
}
func main() {
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	connection := connect(*csiSocket)
	defer connection.Close()
	identity := csi.NewIdentityClient(connection)
	node := csi.NewNodeClient(connection)
	info, err := identity.GetPluginInfo(ctx, &csi.GetPluginInfoRequest{})
	check(err)
	require(info.Name == "csi.bpfman.io" && info.VendorVersion == "0.1.0", "plugin identity")
	caps, err := identity.GetPluginCapabilities(ctx, &csi.GetPluginCapabilitiesRequest{})
	check(err)
	require(len(caps.Capabilities) == 0, "no controller capability")
	ready, err := identity.Probe(ctx, &csi.ProbeRequest{})
	check(err)
	require(ready.GetReady().GetValue(), "ready")
	n, err := node.NodeGetInfo(ctx, &csi.NodeGetInfoRequest{})
	check(err)
	hostname, err := os.Hostname()
	check(err)
	require(n.NodeId == hostname, "hostname")
	nc, err := node.NodeGetCapabilities(ctx, &csi.NodeGetCapabilitiesRequest{})
	check(err)
	require(len(nc.Capabilities) == 1 && nc.Capabilities[0].GetRpc().GetType() == csi.NodeServiceCapability_RPC_VOLUME_MOUNT_GROUP, "mount-group capability")
	_, err = node.NodeStageVolume(ctx, &csi.NodeStageVolumeRequest{})
	code(err, codes.Unimplemented)
	_, err = node.NodeUnstageVolume(ctx, &csi.NodeUnstageVolumeRequest{})
	code(err, codes.Unimplemented)
	_, err = node.NodeGetVolumeStats(ctx, &csi.NodeGetVolumeStatsRequest{})
	code(err, codes.Unimplemented)
	_, err = node.NodeExpandVolume(ctx, &csi.NodeExpandVolumeRequest{})
	code(err, codes.Unimplemented)
	if *lane == "identity" {
		fmt.Println("CSI identity contract passed")
		return
	}
	bconn := connect(*socket)
	defer bconn.Close()
	b := pb.NewBpfmanClient(bconn)
	if *lane == "persist-read" {
		req := request("persist")
		_, err = node.NodePublishVolume(ctx, req)
		check(err)
		require(count(req.TargetPath) == 1, "reopened duplicate target mount")
		cleanup(ctx, node, req)
		list, err := b.List(ctx, &pb.ListRequest{})
		check(err)
		for _, p := range list.Results {
			if p.Info.GetMetadata()["bpfman.io/ProgramName"] == *label {
				_, err = b.Unload(ctx, &pb.UnloadRequest{Id: p.KernelInfo.Id})
				check(err)
			}
		}
		fmt.Println("CSI restart cleanup contract passed")
		return
	}
	load := func() uint32 {
		r, e := b.Load(ctx, &pb.LoadRequest{
			Bytecode: &pb.BytecodeLocation{Location: &pb.BytecodeLocation_File{File: filepath.Join(*objects, "tracepoint_counter.bpf.o")}},
			Info:     []*pb.LoadInfo{{Name: "tracepoint_kill_recorder", ProgramType: pb.BpfmanProgramType_TRACEPOINT}},
			Metadata: map[string]string{"bpfman.io/ProgramName": *label},
		})
		check(e)
		require(len(r.Programs) == 1, "one program")
		return r.Programs[0].KernelInfo.Id
	}
	id := load()
	if *lane == "persist-create" {
		_, err = node.NodePublishVolume(ctx, request("persist"))
		check(err)
		fmt.Println("CSI durable publication created")
		return
	}
	second := load() // Ambiguous metadata must resolve to the lowest live program ID.
	invalid(ctx, node)
	// Missing program and missing requested map are caller-visible NotFound errors.
	absent := request("absent")
	absent.VolumeContext["csi.bpfman.io/program"] = "missing-" + *label
	_, err = node.NodePublishVolume(ctx, absent)
	code(err, codes.NotFound)
	require(count(absent.TargetPath) == 0, "missing program no mount")
	bad := request("bad-map")
	bad.VolumeContext["csi.bpfman.io/maps"] = "tracepoint_stats_map,no_such_map"
	_, err = node.NodePublishVolume(ctx, bad)
	code(err, codes.NotFound)
	require(count(filepath.Join(*backing, bad.VolumeId)) == 0, "failed publish no bpffs mount")
	_, err = os.Stat(filepath.Join(*backing, bad.VolumeId))
	require(os.IsNotExist(err), "failed publish backing removed")
	for i := range 3 {
		req := request(fmt.Sprintf("cycle-%d", i))
		req.VolumeCapability.GetMount().VolumeMountGroup = "12345"
		if i == 1 {
			req.VolumeCapability.GetMount().VolumeMountGroup = "4294979641"
		} // Go's gid_t conversion wraps to 12345.
		_, err = node.NodePublishVolume(ctx, req)
		check(err)
		path := filepath.Join(req.TargetPath, "tracepoint_stats_map")
		var st unix.Stat_t
		check(unix.Stat(path, &st))
		require(st.Gid == 12345 && st.Mode&0777 == 0660, "map fsGroup/mode")
		check(unix.Stat(req.TargetPath, &st))
		require(st.Gid == 12345 && st.Mode&0777 == 0777, fmt.Sprintf("volume fsGroup/mode: gid=%d mode=%#o", st.Gid, st.Mode&07777))
		mapAtTarget, e := ebpf.LoadPinnedMap(path, nil)
		check(e)
		src, e := ebpf.LoadPinnedMap(filepath.Join(*root, "fs/maps", fmt.Sprint(id), "tracepoint_stats_map"), nil)
		check(e)
		a, e := mapAtTarget.Info()
		check(e)
		z, e := src.Info()
		check(e)
		ai, _ := a.ID()
		zi, _ := z.ID()
		require(ai == zi, "same map identity")
		check(mapAtTarget.Close())
		check(src.Close())
		// Map-name normalization and duplicates must not change idempotency.
		duplicate := proto.Clone(req).(*csi.NodePublishVolumeRequest)
		duplicate.VolumeContext["csi.bpfman.io/maps"] = " tracepoint_stats_map,tracepoint_stats_map,, "
		_, err = node.NodePublishVolume(ctx, duplicate)
		check(err)
		require(count(req.TargetPath) == 1 && count(filepath.Join(*backing, req.VolumeId)) == 1, "idempotency does not stack mounts")
		conflict := proto.Clone(req).(*csi.NodePublishVolumeRequest)
		conflict.Readonly = !req.Readonly
		_, err = node.NodePublishVolume(ctx, conflict)
		code(err, codes.AlreadyExists)
		conflict = proto.Clone(req).(*csi.NodePublishVolumeRequest)
		conflict.VolumeContext["csi.bpfman.io/maps"] = "another_map"
		_, err = node.NodePublishVolume(ctx, conflict)
		code(err, codes.AlreadyExists)
		var statfs unix.Statfs_t
		check(unix.Statfs(req.TargetPath, &statfs))
		require((statfs.Flags&unix.ST_RDONLY != 0) == req.Readonly, "read-only state")
		for _, opt := range []string{"nosuid", "nodev", "noexec"} {
			require(mountOption(filepath.Join(*backing, req.VolumeId), opt), "bpffs "+opt)
		}
		cleanup(ctx, node, req)
	}
	// Concurrent same-volume requests may succeed or abort, but cannot stack mounts.
	req := request("concurrent")
	var wg sync.WaitGroup
	failures := make(chan error, 16)
	for range 16 {
		wg.Go(func() {
			_, e := node.NodePublishVolume(ctx, req)
			if e != nil && status.Code(e) != codes.Aborted {
				failures <- e
			}
		})
	}
	wg.Wait()
	close(failures)
	for e := range failures {
		check(e)
	}
	require(count(req.TargetPath) == 1 && count(filepath.Join(*backing, req.VolumeId)) == 1, "concurrent publication does not stack")
	cleanup(ctx, node, req)
	// A retry of the RPC itself must recover after a transient unmount failure.
	retry := request("unpublish-retry0")
	_, err = node.NodePublishVolume(ctx, retry)
	check(err)
	busy := filepath.Join(retry.TargetPath, "busy")
	check(os.Mkdir(busy, 0755))
	check(unix.Mount("tmpfs", busy, "tmpfs", 0, ""))
	_, err = node.NodeUnpublishVolume(ctx, &csi.NodeUnpublishVolumeRequest{VolumeId: retry.VolumeId, TargetPath: retry.TargetPath})
	code(err, codes.Internal)
	require(count(retry.TargetPath) == 1, "busy target retained for explicit retry")
	check(unix.Unmount(busy, 0))
	check(os.Remove(busy))
	cleanup(ctx, node, retry)

	// Existing foreign mounts remain untouched on publish.
	foreign := request("foreign")
	check(os.MkdirAll(foreign.TargetPath, 0755))
	check(unix.Mount("tmpfs", foreign.TargetPath, "tmpfs", 0, ""))
	_, err = node.NodePublishVolume(ctx, foreign)
	code(err, codes.FailedPrecondition)
	require(count(foreign.TargetPath) == 1, "foreign mount retained")
	check(unix.Unmount(foreign.TargetPath, 0))
	check(os.Remove(foreign.TargetPath))
	// A crash between bpffs mount and bind publication is repaired on retry.
	stale := request("stale")
	bp := filepath.Join(*backing, stale.VolumeId)
	check(os.MkdirAll(bp, 0750))
	check(unix.Mount("bpf", bp, "bpf", 0, ""))
	_, err = node.NodePublishVolume(ctx, stale)
	check(err)
	require(count(bp) == 1, "stale backing replaced without stacking")
	cleanup(ctx, node, stale)
	held := request("held-after-unload")
	_, err = node.NodePublishVolume(ctx, held)
	check(err)
	for _, loaded := range []uint32{id, second} {
		_, err = b.Unload(ctx, &pb.UnloadRequest{Id: loaded})
		check(err)
	}
	// A published volume owns its map pins independently of the loaded program.
	_, err = node.NodePublishVolume(ctx, held)
	check(err)
	retained, err := ebpf.LoadPinnedMap(filepath.Join(held.TargetPath, "tracepoint_stats_map"), nil)
	check(err)
	check(retained.Close())
	cleanup(ctx, node, held)
	fmt.Println("CSI wire lifecycle contract passed")
}
func request(suffix string) *csi.NodePublishVolumeRequest {
	return &csi.NodePublishVolumeRequest{
		VolumeId: *label + "-" + suffix, TargetPath: filepath.Join(*root, "targets", suffix), Readonly: !strings.HasSuffix(suffix, "0"),
		VolumeContext:    map[string]string{"csi.bpfman.io/program": *label, "csi.bpfman.io/maps": "tracepoint_stats_map"},
		VolumeCapability: &csi.VolumeCapability{AccessType: &csi.VolumeCapability_Mount{Mount: &csi.VolumeCapability_MountVolume{}}, AccessMode: &csi.VolumeCapability_AccessMode{Mode: csi.VolumeCapability_AccessMode_SINGLE_NODE_WRITER}},
	}
}
func cleanup(ctx context.Context, node csi.NodeClient, req *csi.NodePublishVolumeRequest) {
	for range 2 {
		_, e := node.NodeUnpublishVolume(ctx, &csi.NodeUnpublishVolumeRequest{VolumeId: req.VolumeId, TargetPath: req.TargetPath})
		check(e)
	}
	require(count(req.TargetPath) == 0 && count(filepath.Join(*backing, req.VolumeId)) == 0, "both mounts removed")
	for _, path := range []string{req.TargetPath, filepath.Join(*backing, req.VolumeId)} {
		_, e := os.Stat(path)
		require(os.IsNotExist(e), "directory removed: "+path)
	}
}
func invalid(ctx context.Context, node csi.NodeClient) {
	cases := []func(*csi.NodePublishVolumeRequest){
		func(r *csi.NodePublishVolumeRequest) { r.VolumeId = "" }, func(r *csi.NodePublishVolumeRequest) { r.VolumeId = "../escape" },
		func(r *csi.NodePublishVolumeRequest) { r.TargetPath = "" }, func(r *csi.NodePublishVolumeRequest) { r.TargetPath = "relative" }, func(r *csi.NodePublishVolumeRequest) { r.TargetPath = "/" },
		func(r *csi.NodePublishVolumeRequest) { r.VolumeCapability = nil }, func(r *csi.NodePublishVolumeRequest) {
			r.VolumeCapability.AccessType = &csi.VolumeCapability_Block{Block: &csi.VolumeCapability_BlockVolume{}}
		},
		func(r *csi.NodePublishVolumeRequest) { r.VolumeCapability.GetMount().FsType = "ext4" }, func(r *csi.NodePublishVolumeRequest) { r.VolumeCapability.GetMount().MountFlags = []string{"ro"} },
		func(r *csi.NodePublishVolumeRequest) { r.VolumeCapability.AccessMode = nil }, func(r *csi.NodePublishVolumeRequest) {
			r.VolumeCapability.AccessMode.Mode = csi.VolumeCapability_AccessMode_MULTI_NODE_MULTI_WRITER
		},
		func(r *csi.NodePublishVolumeRequest) { r.VolumeContext["csi.bpfman.io/program"] = "" }, func(r *csi.NodePublishVolumeRequest) { r.VolumeContext["csi.bpfman.io/maps"] = " , ," },
		func(r *csi.NodePublishVolumeRequest) { r.VolumeContext["csi.bpfman.io/maps"] = "../escape" },
	}
	for i, change := range cases {
		r := request(fmt.Sprintf("invalid-%d", i))
		change(r)
		_, e := node.NodePublishVolume(ctx, r)
		code(e, codes.InvalidArgument)
	}
	for _, r := range []*csi.NodeUnpublishVolumeRequest{{}, {VolumeId: "v", TargetPath: "/"}, {VolumeId: "../v", TargetPath: "/tmp/x"}, {VolumeId: "v", TargetPath: "relative"}} {
		_, e := node.NodeUnpublishVolume(ctx, r)
		code(e, codes.InvalidArgument)
	}
}
func mountRows() [][]string {
	data, e := os.ReadFile("/proc/self/mountinfo")
	check(e)
	var out [][]string
	for line := range strings.SplitSeq(string(data), "\n") {
		f := strings.Fields(line)
		if len(f) > 5 {
			out = append(out, f)
		}
	}
	return out
}
func count(path string) int {
	n := 0
	for _, f := range mountRows() {
		if f[4] == path {
			n++
		}
	}
	return n
}
func mountOption(path, opt string) bool {
	for _, f := range mountRows() {
		if f[4] == path {
			for o := range strings.SplitSeq(f[5], ",") {
				if o == opt {
					return true
				}
			}
		}
	}
	return false
}
