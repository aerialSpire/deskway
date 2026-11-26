package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"time"

	"deskway/internal/proto"
)

func removeMCPProfileDir(path string) error {
	lock, err := lockGatewayProfile(path)
	if errors.Is(err, os.ErrNotExist) || errors.Is(err, errGatewayBusy) {
		return nil
	}
	if err != nil {
		return err
	}
	defer lock.Close()
	return os.RemoveAll(path)
}

// EnableRemoteMCP preflights the local AI client before publishing any routes.
func (s *Server) EnableRemoteMCP(origin string) error {
	m, err := newRemoteMCP(origin)
	if err != nil {
		return err
	}
	arch := runtime.GOARCH
	if arch == "amd64" {
		arch = "x64"
	}
	name := "deskway-linux-" + arch
	if arch != "x64" && arch != "arm64" {
		return errors.New("remote MCP requires the Linux x64 or ARM64 Deskway AI client")
	}
	f, err := s.programs.open(name)
	if err != nil {
		return fmt.Errorf("remote MCP: cannot open %s: %w", name, err)
	}
	if f == nil {
		return fmt.Errorf("remote MCP: install the executable %s in the server's AI client downloads directory", name)
	}
	program, err := filepath.Abs(f.Name())
	f.Close()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(s.ctx, 5*time.Second)
	defer cancel()
	if err := exec.CommandContext(ctx, program, "supports-remote-mcp").Run(); err != nil {
		return fmt.Errorf("remote MCP: cannot run %s with remote enrollment support: %w (use a current executable AI client and its shared libraries; the static server container is unsupported)", program, err)
	}
	root := s.mcpProfileRoot()
	if len(gatewaySocket(filepath.Join(root, strings.Repeat("a", 24)))) >= len(syscall.RawSockaddrUnix{}.Path) {
		return errors.New("remote MCP: the data directory path is too long for a private gateway socket")
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return fmt.Errorf("remote MCP: create profile directory: %w", err)
	}
	lock, err := lockGatewayProfile(root)
	if err != nil {
		return fmt.Errorf("remote MCP: open profile directory: %w", err)
	}
	lock.Close()
	if err := s.reconcileMCPProfiles(root); err != nil {
		return fmt.Errorf("remote MCP: reconcile profiles: %w", err)
	}
	pool := newMCPGateways(program)
	pool.signInLost = func(path string) {
		id := filepath.Base(path)
		if err := s.store.mcpLoseSignIn(id); err != nil {
			log.Print("server: cannot save a remote MCP grant's sign-out: ", err)
		}
		s.discardMCPProfile(id)
	}
	m.gateways = pool
	u, _ := url.Parse(m.issuer)
	m.metadata = newMCPMetadata(u.Hostname(), s.turn.PublicIP)
	run := func(ctx context.Context, id string, args ...string) ([]byte, error) {
		ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
		defer cancel()
		path := s.mcpProfilePath(id)
		lock, err := lockGatewayProfile(path)
		if err != nil {
			return nil, err
		}
		defer lock.Close()
		cmd := exec.CommandContext(ctx, program, append([]string{"login"}, args...)...)
		cmd.Env = append(cmd.Environ(), "DESKWAY_CLI_HOME="+path)
		return cmd.Output()
	}
	m.prepare = func(ctx context.Context, id, user, name string) (mcpPrepared, error) {
		var d mcpPrepared
		if err := os.Mkdir(s.mcpProfilePath(id), 0o700); err != nil {
			return d, err
		}
		out, err := run(ctx, id, "--prepare", "--server", m.issuer, "--user", user, "--name", name)
		if err == nil && json.Unmarshal(out, &d) == nil && validMCPID(d.ID, 64) &&
			d.Device.Valid() && d.Device.Role == proto.RoleAI && d.Device.Name == name {
			d.Profile = id
			return d, nil
		}
		s.discardMCPProfile(id)
		return mcpPrepared{}, errors.New("could not prepare the remote AI client")
	}
	m.finish = func(ctx context.Context, id string) error {
		_, err := run(ctx, id, "--finish")
		return err
	}
	s.mcp = m
	return nil
}

func (s *Server) reconcileMCPProfiles(root string) error {
	entries, err := os.ReadDir(root)
	if err != nil {
		return err
	}
	present := make(map[string]bool)
	for _, entry := range entries {
		if entry.IsDir() && validMCPID(entry.Name(), 24) {
			present[entry.Name()] = true
		}
	}
	if err := s.store.mcpReconcileProfiles(present); err != nil {
		return err
	}
	for id := range present {
		s.discardMCPProfile(id)
	}
	return nil
}
