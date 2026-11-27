package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"runtime"
	"strconv"
	"syscall"

	"deskway/internal/server"
)

const hostControlVersion = 1

type hostReady struct {
	Kind        string `json:"kind"`
	Version     int    `json:"version"`
	Hostname    string `json:"hostname"`
	Fingerprint string `json:"fingerprint"`
}

type hostUsage struct {
	Kind      string `json:"kind"`
	Computers int    `json:"computers"`
	Browsers  int    `json:"browsers"`
}

type hostPort struct {
	Kind string `json:"kind"`
	Port int    `json:"port"`
}

// The anonymous pipes belong to the supervising Host. EOF also shuts down a
// child whose Host exited unexpectedly.
func serveHost(s *server.Server, ca *server.LocalCA, stop func()) {
	defer stop()
	hostCommands(os.Stdin, os.Stdout, ca.Hostname(), ca.Fingerprint(), s.LocalUsage)
}

func hostCommands(input io.Reader, output io.Writer, hostname, fingerprint string, usage func() (int, int)) {
	out := json.NewEncoder(output)
	if out.Encode(hostReady{"ready", hostControlVersion, hostname, fingerprint}) != nil {
		return
	}
	in := bufio.NewScanner(input)
	for in.Scan() {
		if in.Text() == "usage" {
			computers, browsers := usage()
			if out.Encode(hostUsage{"usage", computers, browsers}) != nil {
				return
			}
		}
	}
}

func reportHostBindError(out io.Writer, err error) {
	var op *net.OpError
	if !errors.As(err, &op) || op.Addr == nil || !hostBindConflict(err) {
		return
	}
	if _, port, err := net.SplitHostPort(op.Addr.String()); err == nil {
		if n, err := strconv.Atoi(port); err == nil {
			_ = json.NewEncoder(out).Encode(hostPort{"port", n})
		}
	}
}

func hostBindConflict(err error) bool {
	if errors.Is(err, syscall.EADDRINUSE) || errors.Is(err, os.ErrPermission) {
		return true
	}
	// Winsock returns its own errors rather than the POSIX aliases in syscall.
	const wsaAccess, wsaAddrInUse = syscall.Errno(10013), syscall.Errno(10048)
	return runtime.GOOS == "windows" && (errors.Is(err, wsaAccess) || errors.Is(err, wsaAddrInUse))
}
