// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

// This test fixture runs inside a private namespace. It opens every capture
// socket before sending three marked frames, then reports received frames at
// source0 (TX), in0 (PASS), and sink0 (REDIRECT). Outgoing copies never count.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"strconv"
	"time"

	"github.com/cilium/ebpf"
	"golang.org/x/sys/unix"
)

func run(size int) error {
	protocol := int(binary.NativeEndian.Uint16([]byte{0x88, 0xb5}))
	var sockets []int
	defer func() {
		for _, fd := range sockets {
			_ = unix.Close(fd)
		}
	}()
	var polls []unix.PollFd
	var sourceIndex int
	for _, name := range []string{"source0", "in0", "sink0"} {
		iface, err := net.InterfaceByName(name)
		if err != nil {
			return err
		}
		fd, err := unix.Socket(unix.AF_PACKET, unix.SOCK_RAW|unix.SOCK_CLOEXEC|unix.SOCK_NONBLOCK, protocol)
		if err != nil {
			return err
		}
		sockets = append(sockets, fd)
		if err := unix.Bind(fd, &unix.SockaddrLinklayer{Protocol: uint16(protocol), Ifindex: iface.Index}); err != nil {
			return err
		}
		polls = append(polls, unix.PollFd{Fd: int32(fd), Events: unix.POLLIN})
		if name == "source0" {
			sourceIndex = iface.Index
		}
	}
	frame := make([]byte, size)
	copy(frame[:6], []byte{2, 0, 0, 0, 0, 2})   // in0
	copy(frame[6:12], []byte{2, 0, 0, 0, 0, 1}) // source0
	binary.BigEndian.PutUint16(frame[12:14], 0x88b5)
	binary.BigEndian.PutUint32(frame[14:18], 0xb9f00001)
	// An offset-dependent pattern detects loss, reordering, and corruption of
	// fragments as well as truncation. BPF probes check this same pattern.
	for offset := 22; offset < len(frame); offset++ {
		frame[offset] = byte((offset-22)%251 + 1)
	}
	for sequence := range uint32(3) {
		binary.BigEndian.PutUint32(frame[18:22], sequence)
		if err := unix.Sendto(sockets[0], frame, 0, &unix.SockaddrLinklayer{Protocol: uint16(protocol), Ifindex: sourceIndex}); err != nil {
			return err
		}
	}
	var counts [3]uint32
	var seen [3][3]bool
	deadline := time.Now().Add(300 * time.Millisecond)
	buffer := make([]byte, size+1)
	for time.Now().Before(deadline) {
		remaining := max(1, int(time.Until(deadline).Milliseconds()))
		if _, err := unix.Poll(polls, remaining); err != nil && err != unix.EINTR {
			return err
		}
		for index, poll := range polls {
			if poll.Revents&(unix.POLLERR|unix.POLLHUP|unix.POLLNVAL) != 0 {
				return fmt.Errorf("capture socket %d failed: %d", index, poll.Revents)
			}
			if poll.Revents&unix.POLLIN == 0 {
				continue
			}
			for {
				n, from, err := unix.Recvfrom(sockets[index], buffer, 0)
				if err == unix.EAGAIN {
					break
				}
				if err != nil {
					return err
				}
				addr, ok := from.(*unix.SockaddrLinklayer)
				if !ok || addr.Pkttype == unix.PACKET_OUTGOING || n < 22 || binary.BigEndian.Uint32(buffer[14:18]) != 0xb9f00001 {
					continue
				}
				sequence := binary.BigEndian.Uint32(buffer[18:22])
				if sequence >= 3 || seen[index][sequence] {
					return fmt.Errorf("invalid or duplicate frame at capture %d: %d", index, sequence)
				}
				if n != len(frame) || !bytes.Equal(buffer[22:n], frame[22:]) {
					return fmt.Errorf("damaged frame at capture %d: %d bytes", index, n)
				}
				seen[index][sequence] = true
				counts[index]++
			}
		}
	}
	return json.NewEncoder(os.Stdout).Encode(counts)
}

// DEVMAP updates resolve ifindices in the updating process's network namespace.
// The caller runs this fixture via ip netns exec, just like packet capture.
func updateTarget(args []string) error {
	if len(args) != 2 && len(args) != 3 {
		return fmt.Errorf("usage: devmap-delete PIN | devmap-set PIN INTERFACE")
	}
	if args[0] != "devmap-delete" && args[0] != "devmap-set" {
		return fmt.Errorf("unknown operation %q", args[0])
	}
	if (args[0] == "devmap-delete" && len(args) != 2) || (args[0] == "devmap-set" && len(args) != 3) {
		return fmt.Errorf("invalid arguments for %s", args[0])
	}
	m, err := ebpf.LoadPinnedMap(args[1], nil)
	if err != nil {
		return err
	}
	defer m.Close()
	if m.Type() != ebpf.DevMap || m.MaxEntries() != 1 || (m.ValueSize() != 4 && m.ValueSize() != 8) {
		return fmt.Errorf("expected a one-entry DEVMAP with 4- or 8-byte values")
	}
	key := uint32(0)
	if args[0] == "devmap-delete" {
		return m.Delete(key)
	}
	iface, err := net.InterfaceByName(args[2])
	if err != nil {
		return err
	}
	// Aya upgrades values to bpf_devmap_val on supporting kernels. Leave the
	// optional egress-program FD zero; this slice only selects an interface.
	value := make([]byte, m.ValueSize())
	binary.NativeEndian.PutUint32(value[:4], uint32(iface.Index))
	return m.Update(key, value, ebpf.UpdateAny)
}

func main() {
	var err error
	if len(os.Args) == 1 {
		err = run(64)
	} else if os.Args[1] == "packets" {
		if len(os.Args) != 3 {
			err = fmt.Errorf("usage: packets SIZE (64 through 9014 bytes)")
		} else {
			var size int
			size, err = strconv.Atoi(os.Args[2])
			if err == nil {
				if size < 64 || size > 9014 {
					err = fmt.Errorf("frame size must be between 64 and 9014 bytes")
				} else {
					err = run(size)
				}
			}
		}
	} else {
		err = updateTarget(os.Args[1:])
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
