package ebpf

import (
	"testing"

	"github.com/cilium/ebpf"
)

func TestUnsupportedAttachReason(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		typ       ebpf.ProgramType
		at        ebpf.AttachType
		supported bool
	}{
		{"interface xdp", ebpf.XDP, ebpf.AttachXDP, true},
		{"interface xdp, attach none", ebpf.XDP, ebpf.AttachNone, true},
		{"xdp devmap redirect target", ebpf.XDP, ebpf.AttachXDPDevMap, false},
		{"xdp cpumap redirect target", ebpf.XDP, ebpf.AttachXDPCPUMap, false},
		{"single kprobe", ebpf.Kprobe, ebpf.AttachNone, true},
		{"kprobe multi", ebpf.Kprobe, ebpf.AttachTraceKprobeMulti, false},
		{"uprobe multi", ebpf.Kprobe, ebpf.AttachTraceUprobeMulti, false},
		{"kprobe session", ebpf.Kprobe, ebpf.AttachTraceKprobeSession, false},
		{"tracepoint is unaffected", ebpf.TracePoint, ebpf.AttachNone, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			reason := unsupportedAttachReason(tt.typ, tt.at)
			if tt.supported && reason != "" {
				t.Errorf("unsupportedAttachReason(%v, %v) = %q, want supported (empty reason)", tt.typ, tt.at, reason)
			}
			if !tt.supported && reason == "" {
				t.Errorf("unsupportedAttachReason(%v, %v) = empty, want a rejection reason", tt.typ, tt.at)
			}
		})
	}
}
