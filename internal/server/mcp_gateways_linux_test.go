package server

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"deskway/internal/proto"
)

func TestMCPGatewayStandIn(t *testing.T) {
	args := os.Args
	if len(args) < 5 || args[len(args)-5] != "--" {
		return
	}
	profile := os.Getenv("DESKWAY_CLI_HOME")
	if _, err := os.Stat(filepath.Join(profile, "stall")); err == nil {
		fmt.Fprintln(os.Stderr, "EARLY"+strings.Repeat("x", 4096))
		fmt.Fprintln(os.Stderr, "could not bind socket")
		time.Sleep(time.Minute)
		return
	}
	path := gatewaySocket(profile)
	if strings.Join(args[len(args)-4:], " ") != "gateway --listen unix:"+path+" --own-proxy" {
		t.Fatalf("gateway arguments: %q", args)
	}
	ln, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	go func() {
		_, _ = io.Copy(io.Discard, os.Stdin)
		_ = ln.Close()
	}()
	_ = http.Serve(ln, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/mcp" || r.Header.Get("Authorization") != "" || r.Header.Get("Cookie") != "" {
			http.Error(w, "wrong URL or forwarded credentials", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Set-Cookie", "private=test")
		w.Header().Set("Allow", "POST")
		w.WriteHeader(http.StatusAccepted)
		fmt.Fprint(w, os.Getpid())
	}))
}

func TestMCPGatewayBodyCap(t *testing.T) {
	for _, sizeKnown := range []bool{true, false} {
		t.Run(fmt.Sprintf("content length set=%v", sizeKnown), func(t *testing.T) {
			pool := newMCPGateways("does-not-exist")
			t.Cleanup(pool.close)
			r := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(strings.Repeat("x", mcpBodyLimit+1)))
			if !sizeKnown {
				r.ContentLength = -1
			}
			w := httptest.NewRecorder()
			// Acquiring a gateway would fail with 502: the body must win even on
			// the first request, before any part of it can be sent to a child.
			pool.forward(w, r, t.TempDir(), nil)
			if w.Code != http.StatusRequestEntityTooLarge {
				t.Fatalf("status %d, want 413", w.Code)
			}
		})
	}
}

func testGatewayProgram(t *testing.T) string {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("DESKWAY_GATEWAY_TEST_BINARY", exe)
	standIn := filepath.Join(t.TempDir(), "deskway")
	if err := os.WriteFile(standIn, []byte("#!/bin/sh\nexec \"$DESKWAY_GATEWAY_TEST_BINARY\" -test.run '^TestMCPGatewayStandIn$' -- \"$@\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	return standIn
}

func TestMCPGatewaySupervision(t *testing.T) {
	standIn := testGatewayProgram(t)
	root := t.TempDir()
	for _, name := range []string{"a", "b", "c"} {
		if err := os.Mkdir(filepath.Join(root, name), 0o700); err != nil {
			t.Fatal(err)
		}
	}
	a, b, c := filepath.Join(root, "a"), filepath.Join(root, "b"), filepath.Join(root, "c")
	pool := newMCPGateways(standIn)
	pool.limit = 2
	t.Cleanup(pool.close)
	request := func(p *mcpGateways, profile string, status int) int {
		t.Helper()
		r := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader("{}"))
		r.Header.Set("Authorization", "Bearer private")
		r.Header.Set("Cookie", "deskway_session=private")
		w := httptest.NewRecorder()
		p.forward(w, r, profile, nil)
		if w.Code != status || w.Header().Get("Set-Cookie") != "" {
			t.Fatalf("status %d, want %d; headers %v; body %s", w.Code, status, w.Header(), w.Body)
		}
		if status == http.StatusServiceUnavailable && w.Header().Get("Retry-After") == "" {
			t.Fatal("busy response lacks Retry-After")
		}
		if status != http.StatusAccepted {
			return 0
		}
		pid, err := strconv.Atoi(w.Body.String())
		if err != nil || w.Header().Get("Content-Type") != "application/json" || w.Header().Get("Allow") != "POST" {
			t.Fatalf("gateway response: headers %v, body %s", w.Header(), w.Body)
		}
		return pid
	}
	child := func(p *mcpGateways, profile string) *mcpGateway {
		t.Helper()
		p.mu.Lock()
		defer p.mu.Unlock()
		g := p.running[profile]
		if g == nil {
			t.Fatal("gateway did not start")
		}
		return g
	}
	wait := func(g *mcpGateway) {
		t.Helper()
		select {
		case <-g.done:
		case <-time.After(5 * time.Second):
			t.Fatal("gateway did not exit")
		}
	}
	if _, err := os.Lstat(gatewaySocket(a)); !os.IsNotExist(err) {
		t.Fatalf("socket before the first request: %v", err)
	}
	first := request(pool, a, http.StatusAccepted)
	if second := request(pool, a, http.StatusAccepted); first != second {
		t.Fatal("same profile started another process")
	}
	g := child(pool, a)
	other := newMCPGateways(standIn)
	t.Cleanup(other.close)
	request(other, a, http.StatusServiceUnavailable)
	second := request(pool, b, http.StatusAccepted)
	if second == first {
		t.Fatal("different profiles share a process")
	}
	request(pool, c, http.StatusServiceUnavailable)
	if err := g.cmd.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	wait(g)
	if info, err := os.Lstat(gatewaySocket(a)); err != nil || info.Mode()&os.ModeSocket == 0 {
		t.Fatalf("forced exit did not leave a stale socket: %v", err)
	}
	if pid := request(pool, a, http.StatusAccepted); pid == first {
		t.Fatal("gateway did not restart")
	}
	g = child(pool, a)
	pool.stop(a)
	wait(g)
	if state := g.cmd.ProcessState; state == nil || !state.Success() {
		t.Fatalf("closing stdin did not stop the gateway cleanly: %v", state)
	}
	if pid := request(pool, b, http.StatusAccepted); pid != second {
		t.Fatal("stopping the first gateway stopped the second")
	}
	request(pool, c, http.StatusAccepted)
	g, last := child(pool, b), child(pool, c)
	pool.close()
	wait(g)
	wait(last)
	request(pool, a, http.StatusServiceUnavailable)
	if err := os.WriteFile(gatewaySocket(a), []byte("keep"), 0o600); err != nil {
		t.Fatal(err)
	}
	request(other, a, http.StatusBadGateway)
	if body, err := os.ReadFile(gatewaySocket(a)); err != nil || string(body) != "keep" {
		t.Fatalf("occupied socket path was changed: %q, %v", body, err)
	}
	idle := newMCPGateways(standIn)
	idle.limit, idle.idle = 1, 50*time.Millisecond
	t.Cleanup(idle.close)
	active, err := idle.acquire(b, nil)
	if err != nil {
		t.Fatal(err)
	}
	time.Sleep(2 * idle.idle)
	select {
	case <-active.done:
		t.Fatal("gateway stopped while a request was active")
	default:
	}
	idle.release(active)
	wait(active)
	request(idle, c, http.StatusAccepted)
}

func TestMCPGatewaySignInLost(t *testing.T) {
	program := filepath.Join(t.TempDir(), "deskway")
	if err := os.WriteFile(program, []byte("#!/bin/sh\nexit 3\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	pool := newMCPGateways(program)
	t.Cleanup(pool.close)
	profile := t.TempDir()
	if err := os.Chmod(profile, 0o700); err != nil {
		t.Fatal(err)
	}
	lost := make(chan string, 1)
	pool.signInLost = func(profile string) { lost <- profile }
	g, err := pool.acquire(profile, nil)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-g.done:
	case <-time.After(5 * time.Second):
		t.Fatal("gateway did not exit")
	}
	if len(lost) != 1 || <-lost != profile {
		t.Fatal("gateway exit did not retire its profile")
	}
	pool.release(g)
	if _, err := pool.acquire(profile, func() bool { return false }); err != errGatewayRevoked {
		t.Fatalf("revoked gateway can restart: %v", err)
	}
}

func TestMCPGatewayStartupDeadline(t *testing.T) {
	pool := newMCPGateways(testGatewayProgram(t))
	pool.limit = 1
	t.Cleanup(pool.close)
	root := t.TempDir()
	stalled, healthy := filepath.Join(root, "stall"), filepath.Join(root, "ok")
	for _, profile := range []string{stalled, healthy} {
		if err := os.Mkdir(profile, 0700); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(stalled, "stall"), nil, 0600); err != nil {
		t.Fatal(err)
	}
	var logs bytes.Buffer
	previous := log.Writer()
	log.SetOutput(&logs)
	defer log.SetOutput(previous)

	g, err := pool.acquire(stalled, nil)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-g.done:
	case <-time.After(mcpStartWait + 2*time.Second):
		t.Fatal("gateway stalled past its startup deadline")
	}
	if g.cmd.ProcessState == nil || g.cmd.ProcessState.Success() {
		t.Fatal("stalled gateway was not stopped and reaped")
	}
	if _, err := pool.acquire(stalled, nil); err != errGatewayBusy {
		t.Fatalf("stalled profile can start immediately: %v", err)
	}
	if !strings.Contains(logs.String(), "could not bind socket") || strings.Contains(logs.String(), "EARLY") {
		t.Fatal("startup log did not keep the bounded stderr tail")
	}
	w := httptest.NewRecorder()
	pool.forward(w, httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader("{}")), healthy, nil)
	want(t, "another profile gets the freed slot", w.Code, http.StatusAccepted)
	pool.release(g)
}

func TestMCPProfileReconciliation(t *testing.T) {
	f, _ := testMCPFixture(t)
	s := f.s
	root := s.mcpProfileRoot()
	present, missing, orphan := randomHex(12), randomHex(12), randomHex(12)
	for _, id := range []string{present, orphan} {
		if err := os.MkdirAll(s.mcpProfilePath(id), 0700); err != nil {
			t.Fatal(err)
		}
	}
	s.store.MCPGrants = make(map[string]*mcpGrant)
	for _, id := range []string{present, missing} {
		s.store.Browsers[id] = &BrowserRecord{Owner: f.id("me"), Approved: true, Role: proto.RoleAI, Key: &proto.BrowserKey{}}
		s.store.MCPGrants[id] = &mcpGrant{User: f.id("me"), Browser: id}
	}
	if err := s.reconcileMCPProfiles(root); err != nil {
		t.Fatal(err)
	}
	var saved Store
	raw, err := os.ReadFile(f.data)
	if err != nil || json.Unmarshal(raw, &saved) != nil {
		t.Fatal("cannot read saved state", err)
	}
	entries, err := os.ReadDir(root)
	if err != nil || len(entries) != 1 || entries[0].Name() != present ||
		saved.MCPGrants[present] == nil || saved.MCPGrants[missing] != nil || saved.Browsers[missing] != nil {
		t.Fatalf("profile reconciliation left directories %v, grants %v: %v", entries, saved.MCPGrants, err)
	}
}
