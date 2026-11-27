//go:build unix

package server

import (
	"fmt"
	"io"
	"os"
	"syscall"
)

func writeLocalKey(path string, data []byte) error {
	return os.WriteFile(path, data, 0o600)
}

func readLocalKey(path string) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 || info.Sys().(*syscall.Stat_t).Uid != uint32(os.Geteuid()) {
		return nil, fmt.Errorf("%s must be a regular file owned by the server account with owner-only permissions (0600)", path)
	}
	return io.ReadAll(f)
}
