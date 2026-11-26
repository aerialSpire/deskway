package server

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"syscall"
	"time"
)

const (
	mcpBodyLimit  = 2 << 20
	mcpBodyWait   = 5 * time.Second
	mcpCallLimit  = 240 * time.Second
	mcpStartWait  = 5 * time.Second
	mcpStartRetry = 5 * time.Second
	mcpStopWait   = 3 * time.Second
)

// mcpGateways runs one process per approved CLI profile. Only the caller that
// authenticated a grant may choose its profile directory and forward its requests.
type mcpGateways struct {
	program    string
	limit      int
	idle       time.Duration
	mu         sync.Mutex
	closed     bool
	running    map[string]*mcpGateway
	failed     map[string]time.Time
	signInLost func(string)
}

type mcpGateway struct {
	cmd      *exec.Cmd
	stdin    io.WriteCloser
	done     chan struct{}
	ready    chan struct{}
	clients  int
	idle     *time.Timer
	stopping bool
}

func newMCPGateways(program string) *mcpGateways {
	return &mcpGateways{program: program, limit: 16, idle: 5 * time.Minute, running: make(map[string]*mcpGateway), failed: make(map[string]time.Time)}
}

var errGatewayBusy = errors.New("no MCP gateway slot is available")

func (p *mcpGateways) acquire(profile string, authorize func() bool) (*mcpGateway, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if authorize != nil && !authorize() {
		return nil, errGatewayRevoked
	}
	if p.closed {
		return nil, errGatewayBusy
	}
	if g := p.running[profile]; g != nil {
		if g.stopping {
			return nil, errGatewayBusy
		}
		if g.idle != nil {
			g.idle.Stop()
			g.idle = nil
		}
		g.clients++
		return g, nil
	}
	now := time.Now()
	for path, until := range p.failed {
		if !now.Before(until) {
			delete(p.failed, path)
		}
	}
	if !p.failed[profile].IsZero() || len(p.running) >= p.limit {
		return nil, errGatewayBusy
	}
	lock, err := lockGatewayProfile(profile)
	if err != nil {
		return nil, err
	}
	defer lock.Close()
	if err := prepareGatewaySocket(gatewaySocket(profile)); err != nil {
		p.startFailed(profile, err, nil)
		return nil, err
	}
	cmd := exec.Command(p.program, "gateway", "--listen", "unix:"+gatewaySocket(profile), "--own-proxy")
	var stderr gatewayStderr
	cmd.Stderr = &stderr
	cmd.WaitDelay = time.Second
	cmd.Env = append(cmd.Environ(), "DESKWAY_CLI_HOME="+profile)
	// Hold the directory lock in the child until it exits, including when its
	// supervisor dies and the next supervisor starts before the child sees EOF.
	cmd.ExtraFiles = []*os.File{lock}
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		stdin.Close()
		p.startFailed(profile, err, nil)
		return nil, err
	}
	deadline := time.Now().Add(mcpStartWait)
	g := &mcpGateway{cmd: cmd, stdin: stdin, done: make(chan struct{}), ready: make(chan struct{}), clients: 1}
	p.running[profile] = g
	go func() {
		err := cmd.Wait()
		p.mu.Lock()
		defer p.mu.Unlock()
		select {
		case <-g.ready:
		default:
			if !g.stopping {
				p.startFailed(profile, err, stderr)
			}
		}
		var exit *exec.ExitError
		if p.signInLost != nil && errors.As(err, &exit) && exit.ExitCode() == 3 {
			p.signInLost(profile)
		}
		if g.idle != nil {
			g.idle.Stop()
		}
		g.stopping = true
		close(g.done)
		delete(p.running, profile)
	}()
	go func() {
		conn, err := dialGateway(gatewaySocket(profile), g.done, deadline)
		if err != nil {
			_ = cmd.Process.Kill()
			return
		}
		conn.Close()
		p.mu.Lock()
		defer p.mu.Unlock()
		if !g.stopping {
			close(g.ready)
		}
	}()
	return g, nil
}

// Only the supervised CLI writes here. OAuth credentials terminate at the server.
type gatewayStderr []byte

func (tail *gatewayStderr) Write(b []byte) (int, error) {
	const limit = 4096
	n := len(b)
	if n >= limit {
		*tail = append((*tail)[:0], b[n-limit:]...)
	} else {
		drop := max(0, len(*tail)+n-limit)
		*tail = append((*tail)[:copy(*tail, (*tail)[drop:])], b...)
	}
	return n, nil
}

func (p *mcpGateways) startFailed(profile string, err error, stderr gatewayStderr) {
	p.failed[profile] = time.Now().Add(mcpStartRetry)
	log.Printf("server: remote MCP gateway startup failed: %v, stderr %q", err, bytes.TrimSpace(stderr))
}

var errGatewayRevoked = errors.New("MCP grant has been revoked")

func gatewaySocket(profile string) string { return filepath.Join(profile, "mcp.sock") }

func lockGatewayProfile(profile string) (*os.File, error) {
	if !filepath.IsAbs(profile) || filepath.Clean(profile) != profile {
		return nil, errors.New("MCP gateway profile must be an absolute, clean path")
	}
	f, err := os.OpenFile(profile, os.O_RDONLY|syscall.O_DIRECTORY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, err
	}
	dir, err := f.Stat()
	if err == nil && (dir.Mode().Perm()&0o077 != 0 || dir.Sys().(*syscall.Stat_t).Uid != uint32(os.Geteuid())) {
		err = errors.New("MCP gateway profile must be private and owned by the server user")
	}
	if err == nil {
		err = syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if errors.Is(err, syscall.EWOULDBLOCK) {
			err = errGatewayBusy
		}
	}
	if err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func prepareGatewaySocket(path string) error {
	if len(path) >= len(syscall.RawSockaddrUnix{}.Path) {
		return errors.New("MCP gateway socket path is too long")
	}
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSocket == 0 {
		return errors.New("MCP gateway socket path is occupied by another file")
	}
	c, err := net.DialTimeout("unix", path, 100*time.Millisecond)
	if err == nil {
		c.Close()
		return errors.New("MCP gateway socket is still listening")
	}
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if !errors.Is(err, syscall.ECONNREFUSED) {
		return err
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

func (p *mcpGateways) release(g *mcpGateway) {
	p.mu.Lock()
	defer p.mu.Unlock()
	g.clients--
	if g.clients != 0 || g.stopping {
		return
	}
	// A callback already waiting for the lock must not stop a freshly used gateway.
	var timer *time.Timer
	timer = time.AfterFunc(p.idle, func() {
		p.mu.Lock()
		defer p.mu.Unlock()
		if g.idle == timer {
			g.stop()
		}
	})
	g.idle = timer
}

// stop is called under the pool lock. The slot is held until Wait reaps the child.
func (g *mcpGateway) stop() {
	if g.stopping {
		return
	}
	g.stopping = true
	_ = g.stdin.Close()
	go func() {
		select {
		case <-g.done:
		case <-time.After(mcpStopWait):
			_ = g.cmd.Process.Kill()
		}
	}()
}

func (p *mcpGateways) stop(profile string) {
	p.mu.Lock()
	g := p.running[profile]
	if g != nil {
		g.stop()
	}
	p.mu.Unlock()
	if g != nil {
		<-g.done
	}
}

func (p *mcpGateways) close() {
	p.mu.Lock()
	p.closed = true
	var children []*mcpGateway
	for _, g := range p.running {
		g.stop()
		children = append(children, g)
	}
	p.mu.Unlock()
	for _, g := range children {
		<-g.done
	}
}

func dialGateway(path string, done <-chan struct{}, deadline time.Time) (net.Conn, error) {
	ctx, cancel := context.WithDeadline(context.Background(), deadline)
	defer cancel()
	dialer := net.Dialer{}
	for {
		select {
		case <-done:
			return nil, errors.New("MCP gateway stopped")
		default:
		}
		conn, err := dialer.DialContext(ctx, "unix", path)
		if err == nil || (!errors.Is(err, os.ErrNotExist) && !errors.Is(err, syscall.ECONNREFUSED)) {
			return conn, err
		}
		select {
		case <-done:
			return nil, errors.New("MCP gateway stopped")
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// forward reads the complete bounded body before starting or contacting the gateway.
// The Unix request carries MCP headers only. Authentication terminates at the caller.
func (p *mcpGateways) forward(w http.ResponseWriter, r *http.Request, profile string, authorize func() bool) {
	if r.ContentLength > mcpBodyLimit {
		http.Error(w, "MCP request is too large", http.StatusRequestEntityTooLarge)
		return
	}
	controller := http.NewResponseController(w)
	_ = controller.SetReadDeadline(time.Now().Add(mcpBodyWait))
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, mcpBodyLimit))
	_ = controller.SetReadDeadline(time.Time{})
	if err != nil {
		var large *http.MaxBytesError
		if errors.As(err, &large) {
			http.Error(w, "MCP request is too large", http.StatusRequestEntityTooLarge)
		} else {
			http.Error(w, "cannot read MCP request", http.StatusBadRequest)
		}
		return
	}
	g, err := p.acquire(profile, authorize)
	if err != nil {
		if errors.Is(err, errGatewayRevoked) {
			return // the resource server sent its challenge
		} else if errors.Is(err, errGatewayBusy) {
			w.Header().Set("Retry-After", "5")
			http.Error(w, "MCP gateway busy", http.StatusServiceUnavailable)
		} else {
			http.Error(w, "cannot start MCP gateway", http.StatusBadGateway)
		}
		return
	}
	defer p.release(g)
	select {
	case <-g.ready:
	case <-g.done:
		http.Error(w, "MCP gateway unavailable", http.StatusBadGateway)
		return
	case <-r.Context().Done():
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), mcpCallLimit)
	defer cancel()
	deadline, _ := ctx.Deadline()
	_ = controller.SetWriteDeadline(deadline)
	defer controller.SetWriteDeadline(time.Time{})
	req, err := http.NewRequestWithContext(ctx, r.Method, "http://deskway/mcp", bytes.NewReader(body))
	if err != nil {
		http.Error(w, "invalid MCP request", http.StatusBadRequest)
		return
	}
	for _, name := range []string{"Accept", "Content-Type", "MCP-Protocol-Version", "Mcp-Method", "Mcp-Name"} {
		for _, value := range r.Header.Values(name) {
			req.Header.Add(name, value)
		}
	}
	transport := http.Transport{
		DisableKeepAlives: true,
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			return (&net.Dialer{}).DialContext(ctx, "unix", gatewaySocket(profile))
		},
	}
	res, err := transport.RoundTrip(req)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) {
			http.Error(w, "MCP gateway timed out", http.StatusGatewayTimeout)
		} else {
			http.Error(w, "MCP gateway unavailable", http.StatusBadGateway)
		}
		return
	}
	defer res.Body.Close()
	for _, name := range []string{"Allow", "Content-Type"} {
		for _, value := range res.Header.Values(name) {
			w.Header().Add(name, value)
		}
	}
	w.WriteHeader(res.StatusCode)
	if _, err := io.Copy(w, res.Body); err != nil {
		panic(http.ErrAbortHandler)
	}
}
