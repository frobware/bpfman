package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"

	"google.golang.org/grpc"

	pb "github.com/bpfman/bpfman/server/pb"
)

// The CSI assertions also run with program management supplied by the CLI.
// This narrow interface keeps the existing gRPC-backed contract unchanged.
type management interface {
	Load(context.Context, *pb.LoadRequest, ...grpc.CallOption) (*pb.LoadResponse, error)
	List(context.Context, *pb.ListRequest, ...grpc.CallOption) (*pb.ListResponse, error)
	Unload(context.Context, *pb.UnloadRequest, ...grpc.CallOption) (*pb.UnloadResponse, error)
}

type cliManagement struct {
	binary string
	config string
}

type cliProgram struct {
	Record struct {
		ID   uint32 `json:"program_id"`
		Meta struct {
			Metadata map[string]string `json:"metadata"`
		} `json:"meta"`
	} `json:"record"`
}

func (c cliManagement) run(ctx context.Context, args ...string) ([]cliProgram, error) {
	base := []string{"--runtime-dir", *root}
	if c.config != "" {
		base = append(base, "--config", c.config)
	}
	cmd := exec.CommandContext(ctx, c.binary, append(base, args...)...)
	data, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("program-management CLI: %w", err)
	}
	if len(data) == 0 {
		return nil, nil
	}
	var result struct {
		Programs []cliProgram `json:"programs"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, err
	}
	return result.Programs, nil
}

func (c cliManagement) Load(ctx context.Context, req *pb.LoadRequest, _ ...grpc.CallOption) (*pb.LoadResponse, error) {
	if len(req.Info) != 1 || req.Info[0].ProgramType != pb.BpfmanProgramType_TRACEPOINT || req.Bytecode.GetFile() == "" {
		return nil, fmt.Errorf("CLI fixture requires one file-backed tracepoint")
	}
	args := []string{"program", "load", "file", req.Bytecode.GetFile(), "--programs", "tracepoint:" + req.Info[0].Name, "--output=json"}
	for key, value := range req.Metadata {
		args = append(args, "--metadata", key+"="+value)
	}
	programs, err := c.run(ctx, args...)
	if err != nil {
		return nil, err
	}
	out := &pb.LoadResponse{}
	for _, p := range programs {
		out.Programs = append(out.Programs, &pb.LoadResponseInfo{KernelInfo: &pb.KernelProgramInfo{Id: p.Record.ID}})
	}
	return out, nil
}

func (c cliManagement) List(ctx context.Context, _ *pb.ListRequest, _ ...grpc.CallOption) (*pb.ListResponse, error) {
	programs, err := c.run(ctx, "program", "list", "--output=json")
	if err != nil {
		return nil, err
	}
	out := &pb.ListResponse{}
	for _, p := range programs {
		out.Results = append(out.Results, &pb.ListResponse_ListResult{
			Info: &pb.ProgramInfo{Metadata: p.Record.Meta.Metadata}, KernelInfo: &pb.KernelProgramInfo{Id: p.Record.ID},
		})
	}
	return out, nil
}

func (c cliManagement) Unload(ctx context.Context, req *pb.UnloadRequest, _ ...grpc.CallOption) (*pb.UnloadResponse, error) {
	_, err := c.run(ctx, "program", "unload", strconv.FormatUint(uint64(req.Id), 10))
	return &pb.UnloadResponse{}, err
}
