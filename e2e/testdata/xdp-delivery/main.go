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
	"time"

	"golang.org/x/sys/unix"
)

func run() error {
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
	frame := make([]byte, 64)
	copy(frame[:6], []byte{2, 0, 0, 0, 0, 2})   // in0
	copy(frame[6:12], []byte{2, 0, 0, 0, 0, 1}) // source0
	binary.BigEndian.PutUint16(frame[12:14], 0x88b5)
	binary.BigEndian.PutUint32(frame[14:18], 0xb9f00001)
	for sequence := range uint32(3) {
		binary.BigEndian.PutUint32(frame[18:22], sequence)
		if err := unix.Sendto(sockets[0], frame, 0, &unix.SockaddrLinklayer{Protocol: uint16(protocol), Ifindex: sourceIndex}); err != nil {
			return err
		}
	}
	var counts [3]uint32
	var seen [3][3]bool
	deadline := time.Now().Add(300 * time.Millisecond)
	buffer := make([]byte, 2048)
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

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
