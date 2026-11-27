//go:build unix

package server

import (
	"runtime"

	"golang.org/x/sys/unix"
)

func reuseMDNSPort(fd uintptr) error {
	if err := unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_REUSEADDR, 1); err != nil {
		return err
	}
	switch runtime.GOOS {
	case "darwin", "dragonfly", "freebsd", "netbsd", "openbsd":
		return unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_REUSEPORT, 1)
	}
	return nil
}
