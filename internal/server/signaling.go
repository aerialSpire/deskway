package server

import (
	"context"
	"encoding/json"
	"log"
	"maps"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/coder/websocket"

	"deskway/internal/proto"
)

// Signaling: the sockets of browsers and computers, who may say what, and the host's status.
// Every message of a session goes through Sessions (sessions.go), which owns the records and
// routes them. A dropped socket ends no session.

func (s *Server) handleHostWS(w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	if !s.deviceTokenOK(id, r) {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	owner, approved := s.store.DeviceOwner(id)
	if !approved {
		http.Error(w, "waiting for approval", http.StatusForbidden)
		return
	}
	ws, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	s.active.Add(1)
	defer s.active.Done()
	c, ctx := s.newConn(r.Context(), ws)
	c.deviceID = id

	s.mu.Lock()
	// A reconnecting host replaces its stale socket instead of waiting for ping timeout.
	old := s.hosts[id]
	if old != nil {
		s.clearPreviewsLocked(id)
	}
	s.hosts[id] = c
	s.mu.Unlock()
	if old != nil {
		old.cancel()
	}
	s.sessions.HostUp(c)
	c.send(proto.Hello{Type: proto.TypeHello, Version: proto.Version, Caps: []string{proto.CapTerminals, proto.CapTerminalTakeover}})
	s.touch(id)
	log.Printf("server: host online: %.8s", id)
	s.broadcastDevices()
	if owner != "" {
		s.sendDeviceLog(owner)
		s.sendAccount(owner)
	}
	s.locateOn(c)

	watchRelays := sync.OnceFunc(func() { go s.sendRelayLists(ctx, c) })
	s.pump(ctx, c, func(c *conn, m proto.Msg) { s.fromHost(c, m, watchRelays) })

	s.mu.Lock()
	removed := s.hosts[id] == c
	if removed {
		s.forgetHostLocked(id)
	}
	s.mu.Unlock()
	if removed {
		// Its sessions stay: the computer may be back in a moment, and the peer connections do
		// not depend on this socket.
		s.sessions.HostDown(id)
		s.touch(id)
		log.Printf("server: host offline: %.8s", id)
		s.broadcastDevices()
	}
}

func (s *Server) touch(device string) {
	if err := s.store.Touch(device); err != nil {
		log.Printf("server: save: %v", err)
	}
}

// hostOf is the computer's socket while it is connected.
func (s *Server) hostOf(device string) *conn {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.hosts[device]
}

func (s *Server) handleClientWS(w http.ResponseWriter, r *http.Request) {
	// websocket.Accept enforces same-origin, which stops other sites from
	// opening this socket with the user's cookie.
	ws, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	s.active.Add(1)
	defer s.active.Done()
	c, ctx := s.newConn(r.Context(), ws)
	c.user = userOf(r)
	c.browser = browserOf(r)
	c.ai = s.store.BrowserRole(c.browser) == proto.RoleAI
	c.probe = randomHex(16)
	s.mu.Lock()
	s.clients[c] = true
	s.mu.Unlock()
	c.send(proto.Hello{Type: proto.TypeHello, Version: proto.Version, Caps: []string{proto.CapTerminals, proto.CapTerminalTakeover}, Build: s.build})
	c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
	c.send(proto.Msg{Type: proto.TypeApprovals, Approvals: s.store.Approvals(c.user, pendingBrowserTTL)})
	go s.sendICE(ctx, c)

	s.pump(ctx, c, s.fromClient)

	s.mu.Lock()
	delete(s.clients, c)
	s.mu.Unlock()
	// Its sessions go on; their signaling waits until the browser attaches again.
	s.sessions.Unbind(c)
}

// sendICE gives browsers and AI clients the main server's entries and renews their credentials.
func (s *Server) sendICE(ctx context.Context, c *conn) {
	t := time.NewTicker(iceRenew)
	defer t.Stop()
	for {
		c.send(proto.Msg{Type: proto.TypeICE, ICE: s.iceServers(c.user, s.turn)})
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
	}
}

func (s *Server) sendRelayLists(ctx context.Context, c *conn) {
	var previous map[string]string
	for {
		nodes, changed := s.onlineRelays()
		addresses := make(map[string]string, len(nodes)+1)
		if s.turn.PublicIP != "" {
			addresses[mainRelayID] = relayAddress(s.turn.PublicIP, s.turn.Port)
		}
		for _, node := range nodes {
			addresses[node.ID] = relayAddress(node.PublicIP, node.Port)
		}
		if previous == nil || !maps.Equal(addresses, previous) {
			probe := randomHex(16)
			s.mu.Lock()
			c.relayProbe = probe
			s.mu.Unlock()
			c.send(proto.Msg{Type: proto.TypeRelayList, Probe: probe, Relays: &addresses})
			previous = addresses
		}
		select {
		case <-ctx.Done():
			return
		case <-changed:
		}
	}
}

func (s *Server) newConn(parent context.Context, ws *websocket.Conn) (*conn, context.Context) {
	ctx, cancel := context.WithCancel(context.Background())
	// r.Context() ends when the handler returns; only its cancellation is relevant here.
	stop := context.AfterFunc(parent, cancel)
	ws.SetReadLimit(maxMessage)
	return &conn{ws: ws, out: make(chan []byte, 256), cancel: func() { stop(); cancel() }}, ctx
}

// pump runs writer and pinger until the connection fails, dispatching reads to handle.
func (s *Server) pump(ctx context.Context, c *conn, handle func(*conn, proto.Msg)) {
	defer c.cancel()
	defer c.ws.CloseNow()

	go func() {
		t := time.NewTicker(pingInterval)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				c.ws.CloseNow()
				return
			case b := <-c.out:
				wctx, cancel := context.WithTimeout(ctx, pingTimeout)
				err := c.ws.Write(wctx, websocket.MessageText, b)
				cancel()
				if err != nil {
					c.cancel()
					return
				}
			case <-t.C:
				pctx, cancel := context.WithTimeout(ctx, pingTimeout)
				err := c.ws.Ping(pctx)
				cancel()
				if err != nil {
					c.cancel()
					return
				}
			}
		}
	}()

	for {
		typ, b, err := c.ws.Read(ctx)
		if err != nil {
			return
		}
		if typ != websocket.MessageText {
			continue
		}
		var m proto.Msg
		if json.Unmarshal(b, &m) != nil {
			continue
		}
		handle(c, m)
	}
}

// fromClient takes what a browser sends. Identity (account, display name) comes from its
// login, never from the message. A browser reaches only computers its account may use.
func (s *Server) fromClient(c *conn, m proto.Msg) {
	if c.ai && !aiMay(m) {
		if m.Session != "" && len(m.Session) <= 64 {
			c.send(proto.Msg{Type: proto.TypeBye, Session: m.Session, Reason: "forbidden"})
		}
		return
	}
	switch m.Type {
	case proto.TypeHello:
		s.mu.Lock()
		hadTerminals := slices.Contains(c.caps, proto.CapTerminals)
		hadTakeover := slices.Contains(c.caps, proto.CapTerminalTakeover)
		c.caps = m.Caps[:min(len(m.Caps), 64)]
		showTerminals := slices.Contains(c.caps, proto.CapTerminals)
		showTakeover := slices.Contains(c.caps, proto.CapTerminalTakeover)
		s.mu.Unlock()
		if !c.ai && (showTerminals != hadTerminals || showTakeover != hadTakeover) {
			c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
		}
		return
	case proto.TypeWatch:
		s.mu.Lock()
		c.watching = m.On
		s.mu.Unlock()
		if m.On {
			s.syncThumbs(c)
			c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
		}
		return
	case proto.TypeLocate:
		s.onLocate(c, m.Where)
		return
	case proto.TypeAuditReq:
		s.onAuditReq(c, m.Device, m.Query)
		return
	}
	if m.Session == "" || len(m.Session) > 64 || len(m.Auth) > proto.MaxAuth {
		return
	}
	m.User, m.From = c.user, s.store.Display(c.user)
	switch m.Type {
	case proto.TypeChallenge, proto.TypePair, proto.TypePairAsk:
		if !s.store.CanAccess(c.user, m.Device) {
			c.send(proto.Msg{Type: proto.TypeBye, Session: m.Session, Reason: "forbidden"})
			return
		}
		s.sessions.Open(c, m)
	case proto.TypeAttach:
		if !s.store.CanAccess(c.user, m.Device) {
			gone(c, m.Session)
			return
		}
		s.sessions.Attach(c, m.Session, m.Device)
	case proto.TypeOffer, proto.TypeRestart, proto.TypeCandidate, proto.TypeBye:
		s.sessions.FromBrowser(c, m)
	}
}

// aiMay is what an AI client's login may send: only what a session needs, and where it runs
// (locate). Pairing at a
// computer is not among it, so an AI client cannot let a key of its own in as a browser, and
// its offers are always signed with its device key, which the computer knows as an AI client's
// even with device checks off (docs/architecture.md, "Rules the server enforces").
func aiMay(m proto.Msg) bool {
	switch m.Type {
	case proto.TypeHello, proto.TypeChallenge, proto.TypeRestart, proto.TypeCandidate, proto.TypeBye, proto.TypeAttach, proto.TypeLocate:
		return true
	case proto.TypeOffer:
		var a struct {
			K string `json:"k"`
		}
		return json.Unmarshal(m.Auth, &a) == nil && a.K == "dk"
	}
	return false
}

// validHolder bounds what a host may report as the session holding it.
func validHolder(h *proto.Holder) bool {
	return h.Session != "" && len(h.Session) <= 64 && len(h.Cred) <= 256 && len(h.Name) <= 256 && len(h.User) <= 64
}

func validTerminal(terminal proto.KeptTerminal) bool {
	return terminal.Since > 0 && terminal.Name != "" && len(terminal.Name) <= 256 && len(terminal.Cred) <= 256
}

// onStatus takes a host's status: the sessions holding its two seats, what AI clients may do
// there, and its displays.
func (s *Server) onStatus(c *conn, m proto.Msg) {
	valid := func(h *proto.Holder) *proto.Holder {
		if h != nil && !validHolder(h) {
			return nil
		}
		return h
	}
	holder, ai := valid(m.Holder), valid(m.AIHolder)
	kept := m.KeptTerminal
	if kept != nil && !validTerminal(*kept) {
		kept = nil
	}
	terminals := slices.DeleteFunc(m.Terminals, func(terminal proto.TerminalStatus) bool {
		return !validTerminal(terminal.KeptTerminal)
	})
	state := m.AIState
	if state != proto.AIOff && state != proto.AIOn && state != proto.AIStopped {
		state = ""
	}
	s.mu.Lock()
	if s.hosts[c.deviceID] != c {
		s.mu.Unlock()
		return
	}
	if len(m.Displays) > 0 && len(m.Displays) <= 4096 {
		s.displays[c.deviceID] = m.Displays
	}
	c.holder, c.aiHolder, c.aiState, c.aiRefused = holder, ai, state, m.AIRefused
	takeover := slices.Contains(c.caps, proto.CapTerminalTakeover)
	for i := range terminals {
		terminal := &terminals[i]
		terminal.Here = terminal.Here && terminal.Attached && takeover
		if terminal.Here {
			terminal.Cred = ""
		}
	}
	c.keptTerminal, c.terminals = kept, terminals
	switch m.WorkspaceAccess {
	case "locked", "no_user_session", "view_only":
		c.workspaceAccess = m.WorkspaceAccess
	default:
		c.workspaceAccess = ""
	}
	s.mu.Unlock()
	s.sessions.Status(c, holder, ai)
	s.broadcastDevices()
}

// fromHost takes what a computer sends. A host speaks only on sessions addressed to it.
func (s *Server) fromHost(c *conn, m proto.Msg, watchRelays func()) {
	switch m.Type {
	case proto.TypeHello:
		var tools []string
		for _, t := range m.Tools {
			if len(tools) < 128 && t != "" && len(t) <= 64 {
				tools = append(tools, t)
			}
		}
		s.mu.Lock()
		if s.hosts[c.deviceID] != c {
			s.mu.Unlock()
			return
		}
		if len(m.App) > 64 {
			m.App = ""
		}
		if m.Machine != nil && len(m.Machine.CPU) > 256 {
			m.Machine = nil
		}
		first := c.version == 0
		c.version = max(0, m.Version)
		c.machine = m.Machine
		c.caps = m.Caps[:min(len(m.Caps), 64)]
		measureRelays := slices.Contains(c.caps, proto.CapRelayRTT)
		err := s.store.SetHostHello(c.deviceID, c.version, m.App, tools)
		if first {
			s.requestPreviewLocked(c, time.Now())
		}
		s.mu.Unlock()
		if err != nil {
			log.Printf("server: keep %s's hello: %v", c.deviceID, err)
		}
		if measureRelays {
			watchRelays()
		}
		s.broadcastDevices()
		return
	case proto.TypeRelayRTTs:
		s.mu.Lock()
		if c.relayProbe != "" && m.Probe == c.relayProbe {
			c.relayRTTs = m.RelayRTTs
		}
		s.mu.Unlock()
		return
	case proto.TypeStatus:
		s.onStatus(c, m)
		return
	case proto.TypeHere:
		s.onHere(c, m.Probe)
		return
	case proto.TypeAudit:
		s.onAudit(c, m)
		return
	case proto.TypeVitals:
		s.onVitals(c, m)
		return
	case proto.TypeThumb:
		if len(m.Screen) > 64 {
			return
		}
		s.forwardThumb(c, m.Data, m.Screen)
		return
	}
	if len(m.Auth) > proto.MaxAuth || (m.Holder != nil && !validHolder(m.Holder)) {
		return
	}
	s.sessions.FromHost(c, m)
}
