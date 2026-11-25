package server

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/coder/websocket"

	"deskway/internal/proto"
)

// Signaling routing and authorization: who may send what for which session and computer,
// sessions that outlive their sockets, and attach after a server restart.

// peer is one WebSocket of a browser or a host, reading in the background.
type peer struct {
	t   *testing.T
	ws  *websocket.Conn
	in  chan proto.Msg
	raw chan map[string]any
}

func (f *fixture) dialPeer(url string, opts *websocket.DialOptions) *peer {
	f.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ws, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(url, "http"), opts)
	if err != nil {
		f.t.Fatal(err)
	}
	p := &peer{t: f.t, ws: ws, in: make(chan proto.Msg, 256), raw: make(chan map[string]any, 256)}
	go func() {
		for {
			_, b, err := ws.Read(context.Background())
			if err != nil {
				close(p.in)
				return
			}
			var m proto.Msg
			var raw map[string]any
			_ = json.Unmarshal(b, &m)
			_ = json.Unmarshal(b, &raw)
			p.in <- m
			p.raw <- raw
		}
	}()
	f.t.Cleanup(func() { ws.CloseNow() })
	return p
}

func (p *peer) send(m any) {
	p.t.Helper()
	b, _ := json.Marshal(m)
	if err := p.ws.Write(context.Background(), websocket.MessageText, b); err != nil {
		p.t.Fatal(err)
	}
}

// next waits for a message of type typ, skipping others.
func (p *peer) next(typ string) proto.Msg {
	p.t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case m, ok := <-p.in:
			<-p.raw
			if !ok {
				p.t.Fatalf("socket closed waiting for %s", typ)
			}
			if m.Type == typ {
				return m
			}
		case <-deadline:
			p.t.Fatalf("no %s", typ)
		}
	}
}

// none checks that no message of type typ (for session, if given) arrives for a moment.
func (p *peer) none(typ, session string) {
	p.t.Helper()
	deadline := time.After(400 * time.Millisecond)
	for {
		select {
		case m, ok := <-p.in:
			<-p.raw
			if ok && m.Type == typ && (session == "" || m.Session == session) {
				p.t.Fatalf("unexpected %s: %+v", typ, m)
			}
		case <-deadline:
			return
		}
	}
}

// closed waits until the server closes the socket.
func (p *peer) closed() {
	p.t.Helper()
	deadline := time.After(5 * time.Second)
	for {
		select {
		case _, ok := <-p.in:
			if !ok {
				return
			}
			<-p.raw
		case <-deadline:
			p.t.Fatal("the socket stays open")
		}
	}
}

func (p *peer) close() { p.ws.Close(websocket.StatusNormalClosure, "") }

func (f *fixture) browserSocket(url string, c *http.Client) *peer {
	return f.dialPeer(url+"/ws/client", &websocket.DialOptions{HTTPClient: c})
}

func (f *fixture) hostSocket(url, id string) *peer {
	h := f.dialPeer(url+"/ws/host?id="+id, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer tok-" + id}}})
	h.next(proto.TypeHello)
	return h
}

// signalingFixture: account "me" owns computer "pc1".
func signalingFixture(t *testing.T) *fixture {
	f := newFixture(t)
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), Name: "PC", TokenHash: hashToken("tok-pc1")})
	return f
}

func epoch(n int) *int { return &n }

// holding is a host status naming session sid of account user as the holder.
func holding(sid, user string) proto.Msg {
	return proto.Msg{Type: proto.TypeStatus, Holder: &proto.Holder{Session: sid, Name: "Phone", User: user, Established: true}}
}

// connect runs challenge, nonce, offer and answer for session sid, and the host reports it.
func (f *fixture) connect(t *testing.T, b, h *peer, sid string) {
	t.Helper()
	b.send(proto.Msg{Type: proto.TypeChallenge, Session: sid, Device: "pc1"})
	if ch := h.next(proto.TypeChallenge); ch.Session != sid {
		t.Fatalf("challenge: %+v", ch)
	}
	h.send(proto.Msg{Type: proto.TypeNonce, Session: sid, Nonce: "N"})
	if n := b.next(proto.TypeNonce); n.Nonce != "N" {
		t.Fatalf("nonce: %+v", n)
	}
	b.send(proto.Msg{Type: proto.TypeOffer, Session: sid, Device: "pc1", Epoch: epoch(0), SDP: "offer", User: "forged"})
	if o := h.next(proto.TypeOffer); o.SDP != "offer" || o.User != f.id("me") || o.Epoch == nil || *o.Epoch != 0 {
		t.Fatalf("offer: identity must come from the login, the epoch passes: %+v", o)
	}
	h.send(proto.Msg{Type: proto.TypeAnswer, Session: sid, Epoch: epoch(0), SDP: "answer", Auth: json.RawMessage(`{"sig":"s","hk":"k"}`)})
	if a := b.next(proto.TypeAnswer); a.SDP != "answer" || a.Epoch == nil || *a.Epoch != 0 {
		t.Fatalf("answer: %+v", a)
	}
	h.send(holding(sid, f.id("me")))
	b.next(proto.TypeDevices)
}

func TestHellosCarryVersionAndHostCaps(t *testing.T) {
	f := signalingFixture(t)
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	<-b.in
	if m := <-b.raw; m["type"] != "hello" || m["version"] != float64(proto.Version) || m["caps"] == nil {
		t.Fatalf("first message: %v", m)
	}
	h := f.hostSocket(f.srv.URL, "pc1")
	older := proto.MinVersion - 1
	h.send(proto.Msg{Type: proto.TypeHello, Version: older, Caps: []string{"input", "files"}, Tools: []string{"run", "", strings.Repeat("x", 65), "screenshot"}})
	for {
		d := b.next(proto.TypeDevices)
		if len(d.Devices) == 1 && d.Devices[0].Online && d.Devices[0].Version == older && len(d.Devices[0].Caps) == 2 {
			if got := d.Devices[0].Tools; len(got) != 2 || got[0] != "run" || got[1] != "screenshot" {
				t.Fatalf("tools: %v (empty and too long names are dropped)", got)
			}
			break
		}
	}
	h.close()
	for {
		m := b.next(proto.TypeDevices)
		if d := m.Devices[0]; !d.Online {
			if d.Version != older || len(d.Tools) != 2 || len(d.Caps) != 0 {
				t.Fatalf("offline computer: %+v", d)
			}
			break
		}
	}
	var saved Store
	if err := readState(f.data, &saved); err != nil {
		t.Fatal(err)
	}
	if got := saved.Devices["pc1"].Version; got != older {
		t.Fatalf("saved version: %d; want %d", got, older)
	}

	// A freshly upgraded host is online before its new hello can arrive.
	h = f.hostSocket(f.srv.URL, "pc1")
	if d := b.next(proto.TypeDevices).Devices[0]; !d.Online || d.Version != 0 {
		t.Fatalf("online before hello: %+v", d)
	}
	h.send(proto.Msg{Type: proto.TypeHello, Version: proto.Version})
	if d := b.next(proto.TypeDevices).Devices[0]; !d.Online || d.Version != proto.Version {
		t.Fatalf("online after hello: %+v", d)
	}
}

func TestSessionsAreRoutedOnlyBetweenTheirEnds(t *testing.T) {
	f := signalingFixture(t)
	me := f.signedIn("me")
	b := f.browserSocket(f.srv.URL, me)
	other := f.browserSocket(f.srv.URL, f.signedIn("me"))
	friend := f.browserSocket(f.srv.URL, f.signedIn("friend"))
	h := f.hostSocket(f.srv.URL, "pc1")
	f.connect(t, b, h, "S1")

	// Another account cannot reach the computer at all.
	friend.send(proto.Msg{Type: proto.TypeChallenge, Session: "F1", Device: "pc1"})
	if bye := friend.next(proto.TypeBye); bye.Reason != "forbidden" {
		t.Fatalf("friend: %+v", bye)
	}
	// Another browser, even of the same account, cannot speak on this session.
	for _, typ := range []string{proto.TypeOffer, proto.TypeRestart, proto.TypeCandidate, proto.TypeBye} {
		other.send(proto.Msg{Type: typ, Session: "S1", Device: "pc1", SDP: "x"})
		friend.send(proto.Msg{Type: typ, Session: "S1", Device: "pc1", SDP: "x"})
	}
	h.none(proto.TypeRestart, "S1")
	// Its own browser can: an ICE restart and candidates reach the host, the answer comes back.
	b.send(proto.Msg{Type: proto.TypeRestart, Session: "S1", Epoch: epoch(1), SDP: "restart"})
	if r := h.next(proto.TypeRestart); r.SDP != "restart" || r.Epoch == nil || *r.Epoch != 1 {
		t.Fatalf("restart: %+v", r)
	}
	h.send(proto.Msg{Type: proto.TypeAnswer, Session: "S1", Epoch: epoch(1), SDP: "answer2", Auth: json.RawMessage(`{}`)})
	if a := b.next(proto.TypeAnswer); a.SDP != "answer2" || *a.Epoch != 1 {
		t.Fatalf("restart answer: %+v", a)
	}
	// A session id cannot be started twice (taken over by someone else's challenge).
	other.send(proto.Msg{Type: proto.TypeChallenge, Session: "S1", Device: "pc1"})
	h.none(proto.TypeChallenge, "S1")
}

func TestAHostSpeaksOnlyOnItsOwnSessions(t *testing.T) {
	f := signalingFixture(t)
	f.s.store.putDevice("pc2", &DeviceRecord{Owner: f.id("me"), Name: "PC2", TokenHash: hashToken("tok-pc2")})
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	h := f.hostSocket(f.srv.URL, "pc1")
	h2 := f.hostSocket(f.srv.URL, "pc2")
	f.connect(t, b, h, "S1")
	h2.send(proto.Msg{Type: proto.TypeAnswer, Session: "S1", Epoch: epoch(0), SDP: "evil", Auth: json.RawMessage(`{}`)})
	h2.send(proto.Msg{Type: proto.TypeBye, Session: "S1", Reason: "taken"})
	b.none(proto.TypeAnswer, "S1")
	// Nor can it name another computer's session as its own.
	h2.send(holding("S1", f.id("me")))
	b.send(proto.Msg{Type: proto.TypeRestart, Session: "S1", Epoch: epoch(1), SDP: "r"})
	h.next(proto.TypeRestart)
	h2.none(proto.TypeRestart, "S1")
}

func TestComputerOffline(t *testing.T) {
	f := signalingFixture(t)
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	h := f.hostSocket(f.srv.URL, "pc1")
	for {
		if d := b.next(proto.TypeDevices); len(d.Devices) == 1 && d.Devices[0].Online {
			break
		}
	}
	h.close()
	for {
		if d := b.next(proto.TypeDevices); len(d.Devices) == 1 && !d.Devices[0].Online {
			break
		}
	}
	b.send(proto.Msg{Type: proto.TypeChallenge, Session: "S1", Device: "pc1"})
	if bye := b.next(proto.TypeBye); bye.Reason != "offline" {
		t.Fatalf("%+v", bye)
	}
}

func TestSocketLossSendsNoBye(t *testing.T) {
	f := signalingFixture(t)
	me := f.signedIn("me")
	b := f.browserSocket(f.srv.URL, me)
	h := f.hostSocket(f.srv.URL, "pc1")
	f.connect(t, b, h, "S1")
	b.close()
	h.none(proto.TypeBye, "")

	b = f.browserSocket(f.srv.URL, me)
	b.send(proto.Msg{Type: proto.TypeAttach, Session: "S1", Device: "pc1"})
	if a := b.next(proto.TypeAttached); a.Host != "online" {
		t.Fatalf("%+v", a)
	}
	h.close()
	if p := b.next(proto.TypePresence); p.Session != "S1" || p.Host != "away" {
		t.Fatalf("%+v", p)
	}
	b.none(proto.TypeBye, "")
	// The host is back and still runs it: signaling is routed again.
	h = f.hostSocket(f.srv.URL, "pc1")
	h.send(holding("S1", f.id("me")))
	if p := b.next(proto.TypePresence); p.Host != "online" {
		t.Fatalf("%+v", p)
	}
	b.next(proto.TypeDevices)
	b.send(proto.Msg{Type: proto.TypeCandidate, Session: "S1", Candidate: json.RawMessage(`{"candidate":"c"}`)})
	h.next(proto.TypeCandidate)
}

func TestAttachNeedsTheSessionsAccount(t *testing.T) {
	f := signalingFixture(t)
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	friend := f.browserSocket(f.srv.URL, f.signedIn("friend"))
	h := f.hostSocket(f.srv.URL, "pc1")
	f.connect(t, b, h, "S1")
	friend.send(proto.Msg{Type: proto.TypeAttach, Session: "S1", Device: "pc1"})
	if bye := friend.next(proto.TypeBye); bye.Reason != "gone" {
		t.Fatalf("%+v", bye)
	}
	friend.send(proto.Msg{Type: proto.TypeRestart, Session: "S1", Epoch: epoch(1), SDP: "x"})
	h.none(proto.TypeRestart, "S1")
}

func TestAStatusNotNamingTheSessionEndsIt(t *testing.T) {
	f := signalingFixture(t)
	me := f.signedIn("me")
	b := f.browserSocket(f.srv.URL, me)
	h := f.hostSocket(f.srv.URL, "pc1")
	f.connect(t, b, h, "S1")
	h.send(proto.Msg{Type: proto.TypeStatus})
	if bye := b.next(proto.TypeBye); bye.Session != "S1" || bye.Reason != "ended" {
		t.Fatalf("%+v", bye)
	}
	b.send(proto.Msg{Type: proto.TypeAttach, Session: "S1", Device: "pc1"})
	if bye := b.next(proto.TypeBye); bye.Reason != "gone" {
		t.Fatalf("%+v", bye)
	}
}

func TestAttachAfterAServerRestart(t *testing.T) {
	f := signalingFixture(t)
	me := f.signedIn("me")
	// A new server on the same state knows no session.
	s2 := New(f.s.store, TURNConfig{}, nil, fstest.MapFS{})
	srv2 := httptest.NewServer(s2.Handler())
	t.Cleanup(func() { srv2.Close(); s2.Shutdown() })

	// The browser is back before the computer: its attach waits for the computer's status.
	b := f.browserSocket(srv2.URL, me)
	b.send(proto.Msg{Type: proto.TypeAttach, Session: "S1", Device: "pc1"})
	b.none(proto.TypeBye, "S1")
	h := f.hostSocket(srv2.URL, "pc1")
	h.send(holding("S1", f.id("me")))
	if a := b.next(proto.TypeAttached); a.Session != "S1" || a.Host != "online" {
		t.Fatalf("%+v", a)
	}
	b.send(proto.Msg{Type: proto.TypeRestart, Session: "S1", Epoch: epoch(1), SDP: "r"})
	h.next(proto.TypeRestart)
	h.send(proto.Msg{Type: proto.TypeAnswer, Session: "S1", Epoch: epoch(1), SDP: "a", Auth: json.RawMessage(`{}`)})
	b.next(proto.TypeAnswer)

	// The computer has reported what it runs: an attach for something else is gone at once.
	b.send(proto.Msg{Type: proto.TypeAttach, Session: "S9", Device: "pc1"})
	if bye := b.next(proto.TypeBye); bye.Session != "S9" || bye.Reason != "gone" {
		t.Fatalf("%+v", bye)
	}
}

func TestPairingRepliesEndTheirSession(t *testing.T) {
	f := signalingFixture(t)
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	h := f.hostSocket(f.srv.URL, "pc1")
	b.send(proto.Msg{Type: proto.TypePairAsk, Session: "P1", Device: "pc1", Auth: json.RawMessage(`{"cred":"c"}`)})
	if p := h.next(proto.TypePairAsk); p.User != f.id("me") || p.From != "me" {
		t.Fatalf("%+v", p)
	}
	h.send(proto.Msg{Type: proto.TypePaired, Session: "P1", Auth: json.RawMessage(`{"hk":"k"}`)})
	b.next(proto.TypePaired)
	h.send(proto.Msg{Type: proto.TypePaired, Session: "P1", Auth: json.RawMessage(`{"hk":"again"}`)})
	b.none(proto.TypePaired, "P1")
}
