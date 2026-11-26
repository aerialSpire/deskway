// Package server implements the rendezvous service: accounts, device directory,
// signaling relay, an embedded TURN relay, and the web client. docs/architecture.md ("Signaling
// and server") says which file holds what.
package server

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"net/http"
	"sync"
	"time"

	"github.com/coder/websocket"

	"deskway/internal/proto"
)

const (
	pingInterval = 15 * time.Second
	pingTimeout  = 10 * time.Second
	maxMessage   = 2 << 20
)

// Server holds all live connections.
type Server struct {
	store  *Store
	turn   TURNConfig
	stun   []string
	relays relayState
	web    fs.FS
	build  string // which client files are served: their hash, told to every browser
	login  *loginLimiter
	ai     *aiState // AI sign-ins (aiclients.go)

	ctx      context.Context
	cancel   context.CancelFunc
	programs *clientPrograms
	sessions *Sessions

	mu       sync.Mutex
	hosts    map[string]*conn
	clients  map[*conn]bool
	displays map[string]json.RawMessage // device id -> its attached displays, while online
	// Latest preview per device and display, in memory only. A computer is asked for new ones
	// only when someone looks and these are older than previewMaxAge.
	previews map[string]map[string]preview
	asked    map[string]time.Time // device id -> when it was last asked for previews

	// WebSocket handlers and the program fetcher outlive what net/http tracks.
	active sync.WaitGroup
}

// conn serializes writes to one WebSocket through a queue, so a slow peer
// cannot stall message routing for everyone else.
type conn struct {
	ws       *websocket.Conn
	out      chan []byte
	cancel   context.CancelFunc
	deviceID string // hosts
	user     string // browsers
	browser  string // browsers
	ai       bool   // browsers: an AI client's login (aiMay)
	watching bool
	// Hosts: version and capabilities from this connection's hello.
	version int
	caps    []string
	// Hosts: the current list's probe and the latest reported timings. Guarded by Server.mu.
	relayProbe string
	relayRTTs  map[string]int
	// Hosts: the session holding its desk seat and its AI seat, and what AI clients may do there,
	// from its latest status; nil: nobody, "": a host before version 4.
	holder   *proto.Holder
	aiHolder *proto.Holder
	aiState  string
	// The time of the newest AI switch it refused, as no device it trusts signed it; 0: none.
	aiRefused int64
	// Browsers and AI clients: where they run (locate), the id computers answer it and auditreq
	// by, and the computers that said they are where it runs (here.go, audit.go).
	where []string
	probe string
	here  map[string]bool
}

func (c *conn) send(m any) {
	b, err := json.Marshal(m)
	if err != nil {
		return
	}
	select {
	case c.out <- b:
	default:
		// Queue full: the peer is stuck. Dropping it makes it reconnect cleanly.
		c.cancel()
	}
}

// New creates a Server.
func New(store *Store, turn TURNConfig, stun []string, web fs.FS) *Server {
	ctx, cancel := context.WithCancel(context.Background())
	s := &Server{
		store:    store,
		turn:     turn,
		stun:     stun,
		relays:   newRelayState(),
		web:      web,
		build:    buildOf(web),
		login:    newLoginLimiter(),
		ai:       newAIState(),
		ctx:      ctx,
		cancel:   cancel,
		hosts:    map[string]*conn{},
		clients:  map[*conn]bool{},
		displays: map[string]json.RawMessage{},
		previews: map[string]map[string]preview{},
		asked:    map[string]time.Time{},
	}
	s.sessions = newSessions(time.Now, s.hostOf, s.hostICE, s.controllerFor, store.BrowserCred)
	go s.sessions.run()
	return s
}

// Shutdown cancels program fetching, ends every WebSocket and waits for them to finish.
// Call it after the HTTP server stops accepting connections.
func (s *Server) Shutdown() {
	s.cancel()
	s.mu.Lock()
	var all []*conn
	for _, c := range s.hosts {
		all = append(all, c)
	}
	for c := range s.clients {
		all = append(all, c)
	}
	s.mu.Unlock()
	for _, c := range all {
		c.cancel()
	}
	s.active.Wait()
	s.sessions.close()
}

// Handler returns the HTTP routes.
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /api/login", s.handleLogin)
	mux.HandleFunc("GET /api/login/status", s.handleLoginStatus)
	mux.HandleFunc("POST /api/logout", s.handleLogout)
	mux.HandleFunc("POST /api/login/device", s.handleBrowserKeyAttach)
	mux.HandleFunc("POST /api/login/ai", s.handleAILogin)
	mux.HandleFunc("GET /api/ai/window", s.requireLogin(s.handleAIWindow))
	mux.HandleFunc("POST /api/ai/window", s.requireLogin(s.handleAIWindow))
	mux.HandleFunc("GET /api/ai/clients", s.requireClient(s.handleClients))
	mux.HandleFunc("GET /download/{file}", s.handleDownload)
	mux.HandleFunc("GET /api/me", s.requireClient(s.handleMe))
	mux.HandleFunc("PATCH /api/me", s.requireLogin(s.handleDisplay))
	mux.HandleFunc("POST /api/me/name", s.requireLogin(s.handleAccountName))
	mux.HandleFunc("POST /api/password", s.requireLogin(s.handlePassword))
	mux.HandleFunc("POST /api/super-password", s.requireLogin(s.handleSuper))
	mux.HandleFunc("POST /api/initial-password", s.requireInitial(s.handleInitial))
	mux.HandleFunc("GET /api/members", s.requireAdmin(s.handleMembers))
	mux.HandleFunc("POST /api/members", s.requireAdmin(s.handleMemberCreate))
	mux.HandleFunc("POST /api/members/{id}/password", s.requireAdmin(s.handleMemberReset))
	mux.HandleFunc("DELETE /api/members/{id}", s.requireAdmin(s.handleMemberRemove))
	mux.HandleFunc("GET /api/relays", s.requireAdmin(s.handleRelays))
	mux.HandleFunc("GET /api/relays/check", s.requireAdmin(s.handleRelayCheck))
	mux.HandleFunc("POST /api/relays/token", s.requireAdmin(s.handleRelayToken))
	mux.HandleFunc("PATCH /api/relays/{id}", s.requireAdmin(s.handleRelayRename))
	mux.HandleFunc("DELETE /api/relays/{id}", s.requireAdmin(s.handleRelayRemove))
	mux.HandleFunc("POST /api/relays/join", s.handleRelayJoin)
	mux.HandleFunc("POST /api/relays/report", s.handleRelayReport)
	mux.HandleFunc("POST /api/approvals", s.requireLogin(s.handleApprove))
	mux.HandleFunc("GET /api/devices", s.requireClient(s.handleBrowserKeys))
	mux.HandleFunc("DELETE /api/browsers/{id}", s.requireLogin(s.handleBrowserRemove))
	mux.HandleFunc("PATCH /api/browsers/{id}", s.requireLogin(s.handleBrowserRename))
	mux.HandleFunc("GET /api/policy", s.requireLogin(s.handleGetPolicy))
	mux.HandleFunc("POST /api/policy", s.requireLogin(s.handlePolicy))
	mux.HandleFunc("PATCH /api/computers/{id}", s.requireLogin(s.handleComputerRename))
	mux.HandleFunc("DELETE /api/computers/{id}", s.requireLogin(s.handleComputerRemove))
	mux.HandleFunc("POST /api/computers/{id}/ai", s.requireLogin(s.handleAISwitch))
	mux.HandleFunc("POST /api/host/enroll", s.handleEnroll)
	mux.HandleFunc("POST /api/host/leave", s.handleLeave)
	mux.HandleFunc("GET /ws/client", s.requireClient(s.handleClientWS))
	mux.HandleFunc("GET /ws/host", s.handleHostWS)
	mux.Handle("GET /", http.FileServerFS(s.web))
	return securityHeaders(mux)
}

// buildOf names the client files: a hash of every file's path and contents. A browser that was
// told another one than when its page loaded has an older page.
func buildOf(web fs.FS) string {
	h := sha256.New()
	_ = fs.WalkDir(web, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}
		b, err := fs.ReadFile(web, path)
		if err != nil {
			return nil
		}
		fmt.Fprintf(h, "%s\x00%d\x00", path, len(b))
		h.Write(b)
		return nil
	})
	return hex.EncodeToString(h.Sum(nil))[:12]
}

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("Content-Security-Policy", "default-src 'self'; img-src 'self' data:; connect-src 'self' ws: wss:; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'")
		h.Set("Permissions-Policy", "publickey-credentials-get=(self), publickey-credentials-create=(self), camera=(), microphone=(), geolocation=()")
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("Cache-Control", "no-cache")
		if r.TLS != nil {
			h.Set("Strict-Transport-Security", "max-age=31536000")
		}
		next.ServeHTTP(w, r)
	})
}

// ---- request helpers ----

// decodeBody reads a JSON body of at most limit bytes into v. On failure it answers 400 and
// returns false.
func decodeBody(w http.ResponseWriter, r *http.Request, limit int64, v any) bool {
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, limit)).Decode(v); err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

// storeError answers a store error: what the request got wrong with its own message, anything
// else as a failed save.
func storeError(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, errBadName), errors.Is(err, errBadSecret):
		http.Error(w, err.Error(), http.StatusBadRequest)
	case errors.Is(err, errNameTaken), errors.Is(err, errStale):
		http.Error(w, err.Error(), http.StatusConflict)
	case errors.Is(err, errNoUser), errors.Is(err, errNoRelay):
		http.Error(w, err.Error(), http.StatusNotFound)
	case errors.Is(err, errTooMany):
		http.Error(w, err.Error(), http.StatusTooManyRequests)
	case errors.Is(err, errComputers):
		http.Error(w, err.Error(), http.StatusForbidden)
	default:
		http.Error(w, "save failed", http.StatusInternalServerError)
	}
}
