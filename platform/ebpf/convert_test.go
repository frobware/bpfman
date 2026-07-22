package ebpf

import (
	"testing"

	"github.com/cilium/ebpf"
)

func TestUnsupportedXDPAttach(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		typ  ebpf.ProgramType
		at   ebpf.AttachType
		want bool
	}{
		{"interface xdp", ebpf.XDP, ebpf.AttachXDP, false},
		{"interface xdp, attach none", ebpf.XDP, ebpf.AttachNone, false},
		{"xdp devmap redirect target", ebpf.XDP, ebpf.AttachXDPDevMap, true},
		{"xdp cpumap redirect target", ebpf.XDP, ebpf.AttachXDPCPUMap, true},
		{"non-xdp program is never rejected here", ebpf.SchedCLS, ebpf.AttachXDPDevMap, false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if got := unsupportedXDPAttach(tt.typ, tt.at); got != tt.want {
				t.Errorf("unsupportedXDPAttach(%v, %v) = %v, want %v", tt.typ, tt.at, got, tt.want)
			}
		})
	}
}
