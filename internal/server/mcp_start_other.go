//go:build !linux

package server

import (
	"errors"
	"os"
)

func (*Server) EnableRemoteMCP(string) error {
	return errors.New("remote MCP requires a Linux server")
}

func removeMCPProfileDir(path string) error { return os.RemoveAll(path) }
