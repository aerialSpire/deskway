//go:build !linux

package server

import (
	"context"
	"errors"
	"os"
)

var errRemoteMCPPlatform = errors.New("remote MCP requires a Linux server")

func (*Server) EnableRemoteMCP(string) error { return errRemoteMCPPlatform }

func (*clientPrograms) checkRemoteMCP(context.Context, string) (string, error) {
	return "", errRemoteMCPPlatform
}

func removeMCPProfileDir(path string) error { return os.RemoveAll(path) }
