package server

import (
	"errors"
	"os"
	"path/filepath"
)

// The state file belongs to one process at a time: the store writes the whole file from memory,
// so what another process wrote in between would be lost on its next save. OpenStore holds an
// OS lock on a file next to it for as long as the store is open. The OS drops the lock when the
// process ends, so a crash leaves nothing to clean up.

// ErrLocked is returned by OpenStore while another process has the store open.
var ErrLocked = errors.New("the state file is open in another process")

// lockState takes the state file's lock, or fails at once with ErrLocked.
func lockState(statePath string) (*os.File, error) {
	if err := os.MkdirAll(filepath.Dir(statePath), 0o700); err != nil {
		return nil, err
	}
	f, err := os.OpenFile(statePath+".lock", os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, err
	}
	if err := lockFile(f); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}
