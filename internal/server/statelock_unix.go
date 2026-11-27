//go:build unix

package server

import (
	"errors"
	"os"

	"golang.org/x/sys/unix"
)

func lockFile(f *os.File) error {
	err := unix.Flock(int(f.Fd()), unix.LOCK_EX|unix.LOCK_NB)
	if errors.Is(err, unix.EWOULDBLOCK) {
		return ErrLocked
	}
	return err
}

func openStateSnapshot(path string) (*os.File, error) {
	return os.Open(path)
}

func replaceStateFile(from, to string) error {
	return os.Rename(from, to)
}
