// A stock Go protobuf client for a separately hosted bpfman.v1 service.
// This fixture is shared by the Rust test host and the Go daemon. It never
// launches, replaces or emulates the server under test.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/cilium/ebpf/link"
	"golang.org/x/sys/unix"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	pb "github.com/bpfman/bpfman/server/pb"
)

var socket = flag.String("socket", "", "Server Unix socket")
var objects = flag.String("objects", "", "BPF fixture directory")
var root = flag.String("runtime", "", "Isolated runtime directory")
var serverPID = flag.Int("server-pid", 0, "Long-lived server PID")
var image = flag.String("image", "", "Local registry fixture reference")
var iface = flag.String("interface", "", "Isolated network fixture interface")
var uprobeTarget = flag.String("uprobe-target", "", "Native libc target")
var lane = flag.String("lane", "lifecycle", "lifecycle, families, image, cancellation or persistence")

func main() {
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	connection, err := grpc.NewClient("unix://"+*socket, grpc.WithTransportCredentials(insecure.NewCredentials()))
	check(err)
	defer connection.Close()
	client := pb.NewBpfmanClient(connection)
	switch *lane {
	case "lifecycle":
		lifecycle(ctx, client)
		concurrency(ctx, client)
	case "persist-create":
		id := load(ctx, client)
		attach(ctx, client, id)
		noLinkFDs()
	case "persist-read":
		persisted(ctx, client)
	case "families":
		families(ctx, client)
	case "image":
		images(ctx, client)
	case "cancellation":
		cancellation(ctx, client)
	default:
		check(fmt.Errorf("unknown lane %q", *lane))
	}
	fmt.Println("gRPC contract passed:", *lane)
}

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func require(ok bool, message string) {
	if !ok {
		check(fmt.Errorf("contract: %s", message))
	}
}
func code(err error, want codes.Code) {
	require(status.Code(err) == want, fmt.Sprintf("status: got %v, want %v", err, want))
}
func ptr[T any](v T) *T { return &v }

func request() *pb.LoadRequest {
	return &pb.LoadRequest{
		Bytecode: &pb.BytecodeLocation{Location: &pb.BytecodeLocation_File{File: filepath.Join(*objects, "tracepoint_counter.bpf.o")}},
		Info:     []*pb.LoadInfo{{Name: "tracepoint_kill_recorder", ProgramType: pb.BpfmanProgramType_TRACEPOINT}},
		Metadata: map[string]string{"test": "grpc-wire", "bpfman.io/application": "grpc-wire"},
	}
}
func load(ctx context.Context, c pb.BpfmanClient) uint32 {
	req := request()
	result, err := c.Load(ctx, req)
	check(err)
	require(len(result.Programs) == 1, "one loaded program")
	p := result.Programs[0]
	require(p.KernelInfo != nil && p.KernelInfo.Id != 0, "loaded kernel identity")
	require(p.Info.Name == req.Info[0].Name && proto.Equal(p.Info.Bytecode, req.Bytecode), "load source and name")
	require(p.KernelInfo.ProgramType == 2 && p.KernelInfo.GplCompatible, "wire program type and GPL")
	require(p.KernelInfo.Jited == (p.KernelInfo.BytesJited > 0), "load JIT field")
	require(len(p.Info.MapUsedBy) == 1 && p.Info.MapUsedBy[0] == fmt.Sprint(p.KernelInfo.Id), "private map set membership")
	return p.KernelInfo.Id
}
func attach(ctx context.Context, c pb.BpfmanClient, id uint32) uint32 {
	r, err := c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_TracepointAttachInfo{TracepointAttachInfo: &pb.TracepointAttachInfo{Tracepoint: "syscalls/sys_enter_kill", Metadata: map[string]string{"ignored": "matches-go"}}}}})
	check(err)
	require(r.LinkId != 0, "nonzero link handle")
	return r.LinkId
}
func unload(ctx context.Context, c pb.BpfmanClient, id uint32) {
	_, err := c.Unload(ctx, &pb.UnloadRequest{Id: id})
	check(err)
}

func lifecycle(ctx context.Context, c pb.BpfmanClient) {
	_, err := c.Load(ctx, &pb.LoadRequest{})
	code(err, codes.InvalidArgument)
	empty := request()
	empty.Info = nil
	_, err = c.Load(ctx, empty)
	code(err, codes.InvalidArgument)
	bad := request()
	bad.Info[0].ProgramType = 99
	_, err = c.Load(ctx, bad)
	code(err, codes.InvalidArgument)
	_, err = c.Attach(ctx, &pb.AttachRequest{})
	code(err, codes.InvalidArgument)
	_, err = c.Get(ctx, &pb.GetRequest{})
	code(err, codes.NotFound)
	_, err = c.Unload(ctx, &pb.UnloadRequest{})
	code(err, codes.NotFound)
	_, err = c.Detach(ctx, &pb.DetachRequest{})
	code(err, codes.NotFound)
	_, err = c.List(ctx, &pb.ListRequest{ProgramType: ptr(uint32(99))})
	code(err, codes.InvalidArgument)
	_, err = c.PullBytecode(ctx, &pb.PullBytecodeRequest{})
	code(err, codes.InvalidArgument)
	_, err = c.PullBytecode(ctx, &pb.PullBytecodeRequest{Image: &pb.BytecodeImage{Url: "invalid", ImagePullPolicy: 99}})
	code(err, codes.InvalidArgument)
	absentOwner := request()
	absentOwner.MapOwnerId = ptr(^uint32(0))
	_, err = c.Load(ctx, absentOwner)
	code(err, codes.InvalidArgument)
	id := load(ctx, c)
	joined := request()
	joined.MapOwnerId = ptr(id)
	dependent, err := c.Load(ctx, joined)
	check(err)
	require(len(dependent.Programs) == 1 && dependent.Programs[0].Info.MapOwnerId == nil && len(dependent.Programs[0].Info.MapUsedBy) == 2, "Load omits map-owner field but reports shared membership")
	shared, err := c.Get(ctx, &pb.GetRequest{Id: dependent.Programs[0].KernelInfo.Id})
	check(err)
	require(shared.Info.GetMapOwnerId() == id, "Get reports durable map owner")
	unload(ctx, c, dependent.Programs[0].KernelInfo.Id)
	got, err := c.Get(ctx, &pb.GetRequest{Id: id})
	check(err)
	require(got.KernelInfo.Id == id && got.KernelInfo.ProgramType == 2 && got.KernelInfo.GplCompatible, "Get typed identity")
	require(!got.KernelInfo.Jited && got.KernelInfo.BytesMemlock == 0 && got.KernelInfo.VerifiedInsns == 0, "Go Get projection")
	require(got.Info.Bytecode.GetFile() != request().Bytecode.GetFile(), "Get uses captured bytecode path")
	listed, err := c.List(ctx, &pb.ListRequest{ProgramType: ptr(uint32(2)), BpfmanProgramsOnly: ptr(false), MatchMetadata: map[string]string{"test": "grpc-wire"}})
	check(err)
	require(len(listed.Results) == 1 && listed.Results[0].KernelInfo.Id == id, "List managed metadata and type selection")
	require(!listed.Results[0].KernelInfo.GplCompatible && len(listed.Results[0].Info.Links) == 0, "Go List projection")
	listed, err = c.List(ctx, &pb.ListRequest{MatchMetadata: map[string]string{"test": "absent"}})
	check(err)
	require(len(listed.Results) == 0, "metadata filter excludes records")
	_, err = c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_XdpAttachInfo{XdpAttachInfo: &pb.XDPAttachInfo{Iface: "lo"}}}})
	code(err, codes.FailedPrecondition)
	_, err = c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_TracepointAttachInfo{TracepointAttachInfo: &pb.TracepointAttachInfo{Tracepoint: "broken"}}}})
	code(err, codes.InvalidArgument)
	_, err = c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_TracepointAttachInfo{TracepointAttachInfo: &pb.TracepointAttachInfo{Tracepoint: "syscalls/bpfman_missing_event"}}}})
	code(err, codes.NotFound)
	linkID := attach(ctx, c, id)
	kernelID := pinnedLinkControl(linkID)
	noLinkFDs()
	got, err = c.Get(ctx, &pb.GetRequest{Id: id})
	check(err)
	require(len(got.Info.Links) == 1 && got.Info.Links[0] == linkID, "Get returns managed link handle")
	_, err = c.Detach(ctx, &pb.DetachRequest{LinkId: linkID})
	check(err)
	native, err := link.NewFromID(kernelID)
	if err == nil {
		_ = native.Close()
		require(false, "kernel link survives Detach return")
	}
	require(errors.Is(err, os.ErrNotExist), "kernel link absent at Detach return")
	_, err = c.Detach(ctx, &pb.DetachRequest{LinkId: linkID})
	code(err, codes.NotFound)
	got, err = c.Get(ctx, &pb.GetRequest{Id: id})
	check(err)
	require(len(got.Info.Links) == 0, "Detach publishes removal before return")
	attach(ctx, c, id)
	unload(ctx, c, id)
	_, err = c.Get(ctx, &pb.GetRequest{Id: id})
	code(err, codes.NotFound)
	_, err = c.Unload(ctx, &pb.UnloadRequest{Id: id})
	code(err, codes.NotFound)
	noLinkFDs()
	// Repeated members share one source but acquire independent map sets.
	batch := request()
	batch.Info = append(batch.Info, batch.Info[0])
	loaded, err := c.Load(ctx, batch)
	check(err)
	require(len(loaded.Programs) == 2, "batch response length")
	for _, p := range loaded.Programs {
		require(p.Info.MapOwnerId == nil && len(p.Info.MapUsedBy) == 1, "batch never fabricates map ownership")
		unload(ctx, c, p.KernelInfo.Id)
	}
}
func noLinkFDs() {
	if *serverPID == 0 {
		return
	}
	entries, err := os.ReadDir(fmt.Sprintf("/proc/%d/fd", *serverPID))
	check(err)
	for _, entry := range entries {
		target, err := os.Readlink(fmt.Sprintf("/proc/%d/fd/%s", *serverPID, entry.Name()))
		if err == nil {
			require(!strings.Contains(target, "bpf-link") && !strings.Contains(target, "bpf_link"), "long-lived server retains no link FD")
		}
	}
}
func concurrency(ctx context.Context, c pb.BpfmanClient) {
	var group sync.WaitGroup
	for range 16 {
		group.Go(func() {
			id := load(ctx, c)
			linkID := attach(ctx, c, id)
			_, err := c.Get(ctx, &pb.GetRequest{Id: id})
			check(err)
			_, err = c.List(ctx, &pb.ListRequest{})
			check(err)
			_, err = c.Detach(ctx, &pb.DetachRequest{LinkId: linkID})
			check(err)
			unload(ctx, c, id)
		})
	}
	group.Wait()
	noLinkFDs()
}
func cancellation(ctx context.Context, c pb.BpfmanClient) {
	id := load(ctx, c)
	file, err := os.OpenFile(filepath.Join(*root, ".lock"), os.O_RDWR, 0)
	check(err)
	defer file.Close()
	check(unix.Flock(int(file.Fd()), unix.LOCK_EX))
	// A waiting writer must not block a lock-free read on the same connection.
	done := make(chan error, 1)
	deadline, cancel := context.WithTimeout(ctx, 600*time.Millisecond)
	go func() { _, e := c.Unload(deadline, &pb.UnloadRequest{Id: id}); done <- e }()
	waitLockFDs(1)
	readCtx, readCancel := context.WithTimeout(ctx, time.Second)
	_, err = c.Get(readCtx, &pb.GetRequest{Id: id})
	readCancel()
	check(err)
	code(<-done, codes.DeadlineExceeded)
	cancel()
	cancelled, stop := context.WithCancel(ctx)
	go func() {
		_, e := c.Attach(cancelled, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_TracepointAttachInfo{TracepointAttachInfo: &pb.TracepointAttachInfo{Tracepoint: "syscalls/sys_enter_kill"}}}})
		done <- e
	}()
	waitLockFDs(1)
	stop()
	code(<-done, codes.Canceled)
	// Observe server-side writer descriptor closure before releasing the
	// external flock. This proves cancellation reached the server admission.
	waitLockFDs(0)
	blocked, cancelBlocked := context.WithCancel(ctx)
	var group sync.WaitGroup
	for range 24 {
		group.Go(func() {
			_, e := c.Attach(blocked, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_TracepointAttachInfo{TracepointAttachInfo: &pb.TracepointAttachInfo{Tracepoint: "syscalls/sys_enter_kill"}}}})
			code(e, codes.Canceled)
		})
	}
	waitLockFDs(8)
	// All mutation workers are blocked; reserved readers must still run.
	readCtx, readCancel = context.WithTimeout(ctx, time.Second)
	_, err = c.Get(readCtx, &pb.GetRequest{Id: id})
	readCancel()
	check(err)
	cancelBlocked()
	group.Wait()
	waitLockFDs(0)
	check(unix.Flock(int(file.Fd()), unix.LOCK_UN))
	got, err := c.Get(ctx, &pb.GetRequest{Id: id})
	check(err)
	require(len(got.Info.Links) == 0, "cancelled writer never attaches after lock release")
	unload(ctx, c, id)
}
func images(ctx context.Context, c pb.BpfmanClient) {
	img := &pb.BytecodeImage{Url: *image, ImagePullPolicy: 0}
	_, err := c.PullBytecode(ctx, &pb.PullBytecodeRequest{Image: img})
	check(err)
	img.ImagePullPolicy = 2
	result, err := c.Load(ctx, &pb.LoadRequest{Bytecode: &pb.BytecodeLocation{Location: &pb.BytecodeLocation_Image{Image: img}}, Info: []*pb.LoadInfo{{Name: "pass", ProgramType: pb.BpfmanProgramType_XDP}}})
	check(err)
	require(len(result.Programs) == 1 && result.Programs[0].Info.Bytecode.GetImage().Url == *image, "image load provenance")
	unload(ctx, c, result.Programs[0].KernelInfo.Id)
}

func families(ctx context.Context, c pb.BpfmanClient) {
	type family struct {
		object, name string
		kind         pb.BpfmanProgramType
		specific     *pb.ProgSpecificInfo
		attach       *pb.AttachInfo
		actual       string
	}
	traceTarget := func(exit bool) *pb.ProgSpecificInfo {
		if exit {
			return &pb.ProgSpecificInfo{Info: &pb.ProgSpecificInfo_FexitLoadInfo{FexitLoadInfo: &pb.FexitLoadInfo{FnName: "do_unlinkat"}}}
		}
		return &pb.ProgSpecificInfo{Info: &pb.ProgSpecificInfo_FentryLoadInfo{FentryLoadInfo: &pb.FentryLoadInfo{FnName: "do_unlinkat"}}}
	}
	cases := []family{
		{object: "xdp_pass.bpf.o", name: "pass", kind: pb.BpfmanProgramType_XDP, attach: &pb.AttachInfo{Info: &pb.AttachInfo_XdpAttachInfo{XdpAttachInfo: &pb.XDPAttachInfo{Iface: *iface, Priority: 7}}}},
		{object: "tc_pass.bpf.o", name: "alpha", kind: pb.BpfmanProgramType_TC, attach: &pb.AttachInfo{Info: &pb.AttachInfo_TcAttachInfo{TcAttachInfo: &pb.TCAttachInfo{Iface: *iface, Direction: "ingress", Priority: 7, ProceedOn: []int32{3, 30}}}}},
		{object: "tcx_counter.bpf.o", name: "tcx_stats", kind: pb.BpfmanProgramType_TCX, attach: &pb.AttachInfo{Info: &pb.AttachInfo_TcxAttachInfo{TcxAttachInfo: &pb.TCXAttachInfo{Iface: *iface, Direction: "egress", Priority: 7}}}},
		{object: "kprobe_counter.bpf.o", name: "kprobe_counter", kind: pb.BpfmanProgramType_KPROBE, attach: &pb.AttachInfo{Info: &pb.AttachInfo_KprobeAttachInfo{KprobeAttachInfo: &pb.KprobeAttachInfo{FnName: "do_unlinkat"}}}},
		{object: "multi_prog_mixed_counter.bpf.o", name: "mixed_krp", kind: pb.BpfmanProgramType_KPROBE, actual: "kretprobe", attach: &pb.AttachInfo{Info: &pb.AttachInfo_KprobeAttachInfo{KprobeAttachInfo: &pb.KprobeAttachInfo{FnName: "do_unlinkat"}}}},
		{object: "uprobe_counter.bpf.o", name: "uprobe_counter", kind: pb.BpfmanProgramType_UPROBE, attach: &pb.AttachInfo{Info: &pb.AttachInfo_UprobeAttachInfo{UprobeAttachInfo: &pb.UprobeAttachInfo{Target: *uprobeTarget, FnName: ptr("getpid")}}}},
		{object: "uprobe_counter.bpf.o", name: "uprobe_counter", kind: pb.BpfmanProgramType_UPROBE, actual: "uretprobe", attach: &pb.AttachInfo{Info: &pb.AttachInfo_UprobeAttachInfo{UprobeAttachInfo: &pb.UprobeAttachInfo{Target: *uprobeTarget, FnName: ptr("getpid")}}}},
		{object: "fentry_counter.bpf.o", name: "test_fentry", kind: pb.BpfmanProgramType_FENTRY, specific: traceTarget(false), attach: &pb.AttachInfo{Info: &pb.AttachInfo_FentryAttachInfo{FentryAttachInfo: &pb.FentryAttachInfo{}}}},
		{object: "fentry_counter.bpf.o", name: "test_fexit", kind: pb.BpfmanProgramType_FEXIT, specific: traceTarget(true), attach: &pb.AttachInfo{Info: &pb.AttachInfo_FexitAttachInfo{FexitAttachInfo: &pb.FexitAttachInfo{}}}},
		{object: "lsm_exact.bpf.o", name: "test_lsm", kind: pb.BpfmanProgramType_LSM, specific: &pb.ProgSpecificInfo{Info: &pb.ProgSpecificInfo_LsmLoadInfo{LsmLoadInfo: &pb.LsmLoadInfo{HookName: "file_open"}}}, attach: &pb.AttachInfo{Info: &pb.AttachInfo_LsmAttachInfo{LsmAttachInfo: &pb.LsmAttachInfo{}}}},
	}
	for _, test := range cases {
		metadata := map[string]string{"test": "grpc-family"}
		if test.actual != "" {
			metadata["bpfman.io/actual-type:"+test.name] = test.actual
		}
		r, err := c.Load(ctx, &pb.LoadRequest{Bytecode: &pb.BytecodeLocation{Location: &pb.BytecodeLocation_File{File: filepath.Join(*objects, test.object)}}, Info: []*pb.LoadInfo{{Name: test.name, ProgramType: test.kind, Info: test.specific}}, Metadata: metadata})
		check(fmtWrap(err, "load "+test.name))
		require(len(r.Programs) == 1, "family load result")
		id := r.Programs[0].KernelInfo.Id
		if test.kind == pb.BpfmanProgramType_XDP {
			_, err = c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: &pb.AttachInfo{Info: &pb.AttachInfo_XdpAttachInfo{XdpAttachInfo: &pb.XDPAttachInfo{Iface: "bpfman-missing"}}}})
			code(err, codes.InvalidArgument)
		}
		for round := range 2 {
			attached, err := c.Attach(ctx, &pb.AttachRequest{Id: id, Attach: test.attach})
			check(fmtWrap(err, "attach "+test.name))
			require(attached.LinkId != 0, "family managed link handle")
			noLinkFDs()
			if round == 0 {
				_, err = c.Detach(ctx, &pb.DetachRequest{LinkId: attached.LinkId})
				check(err)
			} else {
				unload(ctx, c, id)
			}
		}
		_, err = c.Get(ctx, &pb.GetRequest{Id: id})
		code(err, codes.NotFound)
	}
}
func fmtWrap(err error, operation string) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("%s: %w", operation, err)
}

func waitLockFDs(minimum int) {
	deadline := time.Now().Add(3 * time.Second)
	for {
		entries, err := os.ReadDir(fmt.Sprintf("/proc/%d/fd", *serverPID))
		check(err)
		count := 0
		for _, entry := range entries {
			target, err := os.Readlink(fmt.Sprintf("/proc/%d/fd/%s", *serverPID, entry.Name()))
			if err == nil && target == filepath.Join(*root, ".lock") {
				count++
			}
		}
		if (minimum == 0 && count == 0) || (minimum > 0 && count >= minimum) {
			return
		}
		require(time.Now().Before(deadline), fmt.Sprintf("writer descriptor observation: got %d, want %d", count, minimum))
		time.Sleep(2 * time.Millisecond)
	}
}

func linkFDCount(pid int) int {
	entries, err := os.ReadDir(fmt.Sprintf("/proc/%d/fd", pid))
	check(err)
	count := 0
	for _, entry := range entries {
		target, err := os.Readlink(fmt.Sprintf("/proc/%d/fd/%s", pid, entry.Name()))
		if err == nil && (strings.Contains(target, "bpf_link") || strings.Contains(target, "bpf-link")) {
			count++
		}
	}
	return count
}
func pinnedLinkControl(id uint32) link.ID {
	before := linkFDCount(os.Getpid())
	native, err := link.LoadPinnedLink(filepath.Join(*root, "fs", "links", fmt.Sprint(id)), nil)
	check(err)
	require(linkFDCount(os.Getpid()) == before+1, "link FD matcher detects a real retained descriptor")
	info, err := native.Info()
	check(err)
	check(native.Close())
	require(linkFDCount(os.Getpid()) == before, "closing the control releases its descriptor")
	return info.ID
}

func persisted(ctx context.Context, c pb.BpfmanClient) {
	listed, err := c.List(ctx, &pb.ListRequest{MatchMetadata: map[string]string{"test": "grpc-wire"}})
	check(err)
	require(len(listed.Results) == 1, "reopened host reads one persisted program")
	id := listed.Results[0].KernelInfo.Id
	got, err := c.Get(ctx, &pb.GetRequest{Id: id})
	check(err)
	require(len(got.Info.Links) == 1, "attachment survives prior host shutdown")
	noLinkFDs()
	unload(ctx, c, id)
	_, err = c.Get(ctx, &pb.GetRequest{Id: id})
	code(err, codes.NotFound)
}
