package server

import (
	"sync"
	"time"

	"deskway/internal/proto"
)

// Sessions owns the signaling session records: which browser each session belongs to, which of
// its sockets carries it now, and what is routed where (docs/session-lifecycle.md, "Authority",
// "Signaling loss" and "Ending"). The server decides neither whether a session is alive nor who
// controls a computer: the host's status says which session holds it, and the record follows.
//
//   - A record is made by a browser's challenge (or pairing request) and belongs to that browser
//     (its account and browser record), never to one socket: any socket of that browser may act
//     on it, and the record's messages go to the socket that last spoke for it.
//   - A record named by its host's status is live. A live record the host's status no longer
//     names is over: its page is told bye{ended}.
//   - A record that never became live is removed when it expires, telling its page bye{failed}.
//   - A browser socket dropping keeps its records unbound; attach binds them to the new socket.
//   - A challenge for a computer not heard from since this server started waits for it (the
//     server may have just restarted); one the server saw go offline is refused bye{offline}.
//   - A host socket dropping or coming back is told to the pages with a session on it (presence).
//
// Every method takes its own lock; callers hold no server lock while calling them.
type Sessions struct {
	now      func() time.Time
	hostOf   func(device string) *conn
	ice      func(user string, host *conn) []proto.ICEServer
	describe func(c *conn, h *proto.Holder) *proto.Controller
	// credOf is the credential of a browser's device key ("" without one).
	credOf func(browser string) string

	mu      sync.Mutex
	records map[string]*record
	// Attaches for sessions not known yet, answered by their host's next status.
	waiting []pendingAttach
	// Hosts whose status arrived on their current socket: they have said what they run.
	reported map[string]bool
	// Hosts whose socket this server saw drop (and not come back): known to be offline. A host
	// not heard from since the server started may be about to connect.
	down map[string]bool
	stop chan struct{}
}

// record is one signaling session: a connection being set up, a running one, or a pairing.
type record struct {
	device string
	// The browser's account: from its login, or from the host's status after a server restart.
	user string
	// The browser record it belongs to. After a server restart it is known again from the first
	// attach of the browser whose device key the host's status names (cred).
	browser string
	cred    string
	// The browser socket its messages go to; nil while no socket has it.
	client *conn
	// The host's status named it.
	live    bool
	pairing bool
	// When a record that never became live is given up.
	expires time.Time
}

type pendingAttach struct {
	client  *conn
	session string
	device  string
}

const (
	// connectExpiry is the life of a connect that no status named yet: the host's nonce lifetime.
	connectExpiry = 60 * time.Second
	// pairExpiry is how long a pairing request may wait for its answer (the host's ask window).
	pairExpiry = 5 * time.Minute
	sweepEvery = 5 * time.Second
)

func newSessions(now func() time.Time, hostOf func(string) *conn, ice func(string, *conn) []proto.ICEServer, describe func(*conn, *proto.Holder) *proto.Controller, credOf func(string) string) *Sessions {
	return &Sessions{now: now, hostOf: hostOf, ice: ice, describe: describe, credOf: credOf, records: map[string]*record{}, reported: map[string]bool{}, down: map[string]bool{}, stop: make(chan struct{})}
}

// belongsLocked says whether socket c is of the browser the record belongs to. A record known
// only from its host's status is claimed by the browser whose device key the status names (by
// any browser of the account when the host checks no device keys).
func (ss *Sessions) belongsLocked(r *record, c *conn) bool {
	if r.user != c.user {
		return false
	}
	if r.browser == "" {
		if r.cred != "" && r.cred != ss.credOf(c.browser) {
			return false
		}
		r.browser = c.browser
	}
	return r.browser == c.browser
}

// run sweeps expired records until close.
func (ss *Sessions) run() {
	t := time.NewTicker(sweepEvery)
	defer t.Stop()
	for {
		select {
		case <-ss.stop:
			return
		case <-t.C:
			ss.Sweep(ss.now())
		}
	}
}

func (ss *Sessions) close() { close(ss.stop) }

func gone(c *conn, sid string) {
	c.send(proto.Msg{Type: proto.TypeBye, Session: sid, Reason: "gone"})
}

// Open starts a session for the browser: a connect (challenge) or a pairing request. A
// challenge the browser repeats for its own record (on any of its sockets) goes to the host
// again, and the record follows that socket; any other known id is ignored. A computer known
// to be offline refuses it with bye{offline}. A challenge for a computer not heard from since
// this server started (it restarted, and the computer may be about to connect) waits as an
// attach does: its page is told presence{away}, then online when the computer connects, and
// asks again. m.User and m.From are the browser's, from its login.
func (ss *Sessions) Open(c *conn, m proto.Msg) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	host := ss.hostOf(m.Device)
	if r := ss.records[m.Session]; r != nil {
		if m.Type == proto.TypeChallenge && !r.pairing && r.device == m.Device && ss.belongsLocked(r, c) {
			r.client = c
			if host != nil {
				host.send(proto.Msg{Type: proto.TypeChallenge, Session: m.Session})
			}
		}
		return
	}
	pairing := m.Type != proto.TypeChallenge
	if host == nil && (pairing || ss.down[m.Device]) {
		c.send(proto.Msg{Type: proto.TypeBye, Session: m.Session, Reason: "offline"})
		return
	}
	life := connectExpiry
	if pairing {
		life = pairExpiry
	}
	ss.records[m.Session] = &record{device: m.Device, user: m.User, browser: c.browser, client: c, pairing: pairing, expires: ss.now().Add(life)}
	if host == nil {
		c.send(proto.Msg{Type: proto.TypePresence, Session: m.Session, Host: "away"})
	} else if pairing {
		host.send(proto.Msg{Type: m.Type, Session: m.Session, Auth: m.Auth, User: m.User, From: m.From})
	} else {
		host.send(proto.Msg{Type: proto.TypeChallenge, Session: m.Session})
	}
}

// FromBrowser passes offers, ICE restarts, candidates and byes of the browser's own records to
// their host, from any socket of that browser (what was queued while its socket was renewed
// arrives on the new one); the record follows the socket that spoke. While the host is away
// they are dropped: the browser sends them again when it hears the host is back.
func (ss *Sessions) FromBrowser(c *conn, m proto.Msg) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	r := ss.records[m.Session]
	if r == nil || !ss.belongsLocked(r, c) {
		return
	}
	r.client = c
	if m.Type == proto.TypeBye {
		delete(ss.records, m.Session)
	}
	host := ss.hostOf(r.device)
	if host == nil {
		return
	}
	switch m.Type {
	case proto.TypeOffer:
		host.send(proto.Msg{Type: m.Type, Session: m.Session, Epoch: m.Epoch, SDP: m.SDP, ICE: ss.ice(r.user, host), View: m.View, Force: m.Force, User: r.user, Auth: m.Auth})
	case proto.TypeRestart:
		host.send(proto.Msg{Type: m.Type, Session: m.Session, Epoch: m.Epoch, SDP: m.SDP})
	case proto.TypeCandidate:
		host.send(proto.Msg{Type: m.Type, Session: m.Session, Candidate: m.Candidate})
	case proto.TypeBye:
		host.send(proto.Msg{Type: m.Type, Session: m.Session, Reason: m.Reason})
	}
}

// Attach binds a session of the browser to this socket of it: answered attached with whether
// the host is connected, or bye{gone} when the session is not known (or is another browser's).
// A session not known yet while its host has not reported since it (re)connected waits for
// that report.
func (ss *Sessions) Attach(c *conn, sid, device string) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	if r := ss.records[sid]; r != nil {
		if r.device != device || !ss.belongsLocked(r, c) {
			gone(c, sid)
			return
		}
		r.client = c
		host := "online"
		if ss.hostOf(device) == nil || !ss.reported[device] {
			host = "away"
		}
		c.send(proto.Msg{Type: proto.TypeAttached, Session: sid, Host: host})
		return
	}
	if ss.reported[device] {
		gone(c, sid)
		return
	}
	for i, a := range ss.waiting {
		if a.client == c && a.session == sid {
			ss.waiting = append(ss.waiting[:i], ss.waiting[i+1:]...)
			break
		}
	}
	ss.waiting = append(ss.waiting, pendingAttach{client: c, session: sid, device: device})
}

// Unbind lets go of a browser socket that closed: its records stay, without a socket.
func (ss *Sessions) Unbind(c *conn) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	for _, r := range ss.records {
		if r.client == c {
			r.client = nil
		}
	}
	kept := ss.waiting[:0]
	for _, a := range ss.waiting {
		if a.client != c {
			kept = append(kept, a)
		}
	}
	ss.waiting = kept
}

// HostUp: the computer's socket connected. Resume routing on its first status, after it has
// identified which sessions survived this connection.
func (ss *Sessions) HostUp(host *conn) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	device := host.deviceID
	if ss.hostOf(device) != host {
		return
	}
	ss.reported[device] = false
	delete(ss.down, device)
}

// HostDown: the computer's socket dropped. Its sessions go on.
func (ss *Sessions) HostDown(device string) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	if ss.hostOf(device) != nil {
		return
	}
	delete(ss.reported, device)
	ss.down[device] = true
	ss.presenceLocked(device, "away")
}

func (ss *Sessions) presenceLocked(device, host string) {
	for id, r := range ss.records {
		if r.device == device && r.client != nil && !r.pairing {
			r.client.send(proto.Msg{Type: proto.TypePresence, Session: id, Host: host})
		}
	}
}

// Status takes the sessions the host says hold it (nil ones: none on that seat). Live records
// of that host it does not name are over; those it names are live (known again after a server
// restart), and attaches waiting for this host are answered. Which seat a session holds is the
// host's business: the server only follows who is named.
func (ss *Sessions) Status(host *conn, holders ...*proto.Holder) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	device := host.deviceID
	if ss.hostOf(device) != host {
		return
	}
	named := map[string]bool{}
	for _, h := range holders {
		if h != nil {
			named[h.Session] = true
		}
	}
	for id, r := range ss.records {
		if r.device == device && r.live && !named[id] {
			delete(ss.records, id)
			if r.client != nil {
				r.client.send(proto.Msg{Type: proto.TypeBye, Session: id, Reason: "ended"})
			}
		}
	}
	for _, holder := range holders {
		if holder == nil {
			continue
		}
		r := ss.records[holder.Session]
		if r == nil {
			r = &record{device: device, user: holder.User, cred: holder.Cred}
			ss.records[holder.Session] = r
		}
		if r.device == device {
			r.live = true
		}
	}
	if reported, registered := ss.reported[device]; registered && !reported {
		ss.presenceLocked(device, "online")
	}
	ss.reported[device] = true
	kept := ss.waiting[:0]
	for _, a := range ss.waiting {
		if a.device != device {
			kept = append(kept, a)
			continue
		}
		r := ss.records[a.session]
		if r != nil && r.device == device && ss.belongsLocked(r, a.client) {
			r.client = a.client
			a.client.send(proto.Msg{Type: proto.TypeAttached, Session: a.session, Host: "online"})
		} else {
			gone(a.client, a.session)
		}
	}
	ss.waiting = kept
}

// FromHost passes what a computer says on its own sessions to their page. A bye or a pairing
// answer ends the record; a busy bye tells the page who holds the computer, as it sees them.
func (ss *Sessions) FromHost(host *conn, m proto.Msg) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	device := host.deviceID
	if ss.hostOf(device) != host {
		return
	}
	r := ss.records[m.Session]
	if r == nil || r.device != device {
		return
	}
	if m.Type == proto.TypeBye || m.Type == proto.TypePaired {
		delete(ss.records, m.Session)
	}
	c := r.client
	if c == nil {
		return
	}
	switch m.Type {
	case proto.TypeNonce:
		c.send(proto.Msg{Type: m.Type, Session: m.Session, Nonce: m.Nonce})
	case proto.TypeAnswer:
		c.send(proto.Msg{Type: m.Type, Session: m.Session, Epoch: m.Epoch, SDP: m.SDP, Auth: m.Auth})
	case proto.TypeCandidate:
		c.send(proto.Msg{Type: m.Type, Session: m.Session, Candidate: m.Candidate})
	case proto.TypeBye:
		out := proto.Msg{Type: m.Type, Session: m.Session, Reason: m.Reason, By: m.By, Same: m.Same}
		if m.Holder != nil {
			out.Controller = ss.describe(c, m.Holder)
		}
		c.send(out)
	case proto.TypePaired:
		c.send(proto.Msg{Type: m.Type, Session: m.Session, Auth: m.Auth})
	}
}

// Sweep removes the records that never became live and have expired, telling their pages.
func (ss *Sessions) Sweep(now time.Time) {
	ss.mu.Lock()
	defer ss.mu.Unlock()
	for id, r := range ss.records {
		if !r.live && now.After(r.expires) {
			delete(ss.records, id)
			if r.client != nil {
				r.client.send(proto.Msg{Type: proto.TypeBye, Session: id, Reason: "failed"})
			}
		}
	}
}
