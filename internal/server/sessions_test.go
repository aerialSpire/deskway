package server

import (
	"encoding/json"
	"testing"
	"time"

	"deskway/internal/proto"
)

// The rules of Sessions (docs/session-lifecycle.md, "Signaling loss" and "Ending"), on fake
// sockets and a clock the test moves.

type sessionsTest struct {
	t     *testing.T
	ss    *Sessions
	now   time.Time
	hosts map[string]*conn
}

// fakeConn is one socket of a browser (its account and browser record).
func fakeConn(user, browser string) *conn {
	return &conn{out: make(chan []byte, 64), cancel: func() {}, user: user, browser: browser}
}

// The device key credential of each test browser: browser "b1" signs with "C-b1".
func credOf(browser string) string { return "C-" + browser }

func newSessionsTest(t *testing.T) *sessionsTest {
	st := &sessionsTest{t: t, now: time.Unix(1000, 0), hosts: map[string]*conn{"pc1": fakeConn("", "")}}
	st.hosts["pc1"].deviceID = "pc1"
	describe := func(c *conn, h *proto.Holder) *proto.Controller {
		return &proto.Controller{Name: h.Name, Connecting: !h.Established}
	}
	st.ss = newSessions(func() time.Time { return st.now }, func(d string) *conn { return st.hosts[d] }, func(user string, _ *conn) []proto.ICEServer { return []proto.ICEServer{{Username: user}} }, describe, credOf)
	return st
}

// got is everything sent to c since the last call.
func got(c *conn) []proto.Msg {
	var out []proto.Msg
	for {
		select {
		case b := <-c.out:
			var m proto.Msg
			_ = json.Unmarshal(b, &m)
			out = append(out, m)
		default:
			return out
		}
	}
}

// one is the single message sent to c since the last call.
func one(t *testing.T, c *conn) proto.Msg {
	t.Helper()
	ms := got(c)
	if len(ms) != 1 {
		t.Fatalf("want one message, got %+v", ms)
	}
	return ms[0]
}

func nothing(t *testing.T, c *conn) {
	t.Helper()
	if ms := got(c); len(ms) != 0 {
		t.Fatalf("want nothing, got %+v", ms)
	}
}

func (st *sessionsTest) challenge(c *conn, sid string) {
	st.ss.Open(c, proto.Msg{Type: proto.TypeChallenge, Session: sid, Device: "pc1", User: c.user, From: "Me"})
}

// live opens sid for c and has the host's status name it.
func (st *sessionsTest) live(c *conn, sid string) {
	st.challenge(c, sid)
	st.ss.Status(st.hosts["pc1"], &proto.Holder{Session: sid, Name: "Phone", User: c.user})
	got(c)
	got(st.hosts["pc1"])
}

func TestSessionsOpenOnAChallengeAndTheHostIsAskedAgainOnlyByItsBrowser(t *testing.T) {
	st := newSessionsTest(t)
	host, page, other := st.hosts["pc1"], fakeConn("u1", "b1"), fakeConn("u1", "b2")
	st.challenge(page, "S1")
	if m := one(t, host); m.Type != proto.TypeChallenge || m.Session != "S1" || m.User != "" || m.From != "" {
		t.Fatalf("the host is asked for a nonce, nothing more: %+v", m)
	}
	st.challenge(page, "S1")
	if m := one(t, host); m.Type != proto.TypeChallenge {
		t.Fatalf("the page repeats its challenge: %+v", m)
	}
	st.challenge(other, "S1")
	nothing(t, host)
	// B20: the page's socket renewed while connecting: its challenge again on the new socket
	// goes to the host, and the nonce comes to the new socket.
	renewed := fakeConn("u1", "b1")
	st.challenge(renewed, "S1")
	if m := one(t, host); m.Type != proto.TypeChallenge {
		t.Fatalf("the challenge from the renewed socket: %+v", m)
	}
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeNonce, Session: "S1", Nonce: "N"})
	if m := one(t, renewed); m.Type != proto.TypeNonce || m.Nonce != "N" {
		t.Fatalf("%+v", m)
	}
	nothing(t, page)
	// The computer is offline (its socket dropped): refused at once, and nothing is kept.
	delete(st.hosts, "pc1")
	st.ss.HostDown("pc1")
	got(page)
	st.challenge(page, "S2")
	if m := one(t, page); m.Type != proto.TypeBye || m.Reason != "offline" {
		t.Fatalf("%+v", m)
	}
	if st.ss.records["S2"] != nil {
		t.Fatal("an offline challenge left a record")
	}
}

func TestSessionsAChallengeForAComputerNotHeardFromSinceTheServerStartedWaitsForIt(t *testing.T) {
	st := newSessionsTest(t)
	host, page := st.hosts["pc1"], fakeConn("u1", "b1")
	delete(st.hosts, "pc1")
	st.challenge(page, "S1")
	if m := one(t, page); m.Type != proto.TypePresence || m.Host != "away" {
		t.Fatalf("A12: after a server restart the computer may be about to connect: %+v", m)
	}
	st.hosts["pc1"] = host
	st.ss.HostUp(st.hosts["pc1"])
	nothing(t, page)
	st.ss.Status(st.hosts["pc1"])
	if m := one(t, page); m.Type != proto.TypePresence || m.Host != "online" {
		t.Fatalf("%+v", m)
	}
	st.challenge(page, "S1")
	if m := one(t, host); m.Type != proto.TypeChallenge || m.Session != "S1" {
		t.Fatalf("the page asks again, and the computer is asked: %+v", m)
	}
}

func TestSessionsRouteOnlyTheOwnBrowsersMessagesAndDropThemWhileTheHostIsAway(t *testing.T) {
	st := newSessionsTest(t)
	host, page, other := st.hosts["pc1"], fakeConn("u1", "b1"), fakeConn("u1", "b2")
	st.challenge(page, "S1")
	got(host)
	st.ss.FromBrowser(other, proto.Msg{Type: proto.TypeOffer, Session: "S1", SDP: "x"})
	nothing(t, host)
	// The offer keeps the browser's display and gains the session's account and relay credentials.
	const display = "secondary"
	st.ss.FromBrowser(page, proto.Msg{Type: proto.TypeOffer, Session: "S1", Epoch: epoch(0), SDP: "o", Display: display, User: "forged"})
	m := one(t, host)
	if m.Type != proto.TypeOffer || *m.Epoch != 0 || m.User != "u1" || len(m.ICE) != 1 || m.ICE[0].Username != "u1" {
		t.Fatalf("%+v", m)
	}
	if m.Display != display {
		t.Fatalf("offer display = %q, want %q", m.Display, display)
	}
	// While the computer is away nothing is answered for it: the page sends again later.
	delete(st.hosts, "pc1")
	st.ss.FromBrowser(page, proto.Msg{Type: proto.TypeOffer, Session: "S1", Epoch: epoch(0), SDP: "o"})
	nothing(t, page)
	st.hosts["pc1"] = host
	st.ss.FromBrowser(page, proto.Msg{Type: proto.TypeBye, Session: "S1", Reason: "left"})
	if m := one(t, host); m.Type != proto.TypeBye || m.Reason != "left" {
		t.Fatalf("%+v", m)
	}
	if st.ss.records["S1"] != nil {
		t.Fatal("a bye from the page ends the record")
	}
}

func TestSessionsKeepARecordWhenItsPageSocketDropsAndAttachBindsItAgain(t *testing.T) {
	st := newSessionsTest(t)
	host, page := st.hosts["pc1"], fakeConn("u1", "b1")
	st.live(page, "S1")
	st.ss.Unbind(page)
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeCandidate, Session: "S1"})
	nothing(t, page)

	again := fakeConn("u1", "b1")
	st.ss.Attach(again, "S1", "pc1")
	if m := one(t, again); m.Type != proto.TypeAttached || m.Host != "online" {
		t.Fatalf("%+v", m)
	}
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeAnswer, Session: "S1", Epoch: epoch(2)})
	if m := one(t, again); m.Type != proto.TypeAnswer || *m.Epoch != 2 {
		t.Fatalf("%+v", m)
	}
	// The computer away: the attach says so.
	delete(st.hosts, "pc1")
	st.ss.Attach(again, "S1", "pc1")
	if m := one(t, again); m.Type != proto.TypeAttached || m.Host != "away" {
		t.Fatalf("%+v", m)
	}
	// Another account's page, or another browser of the same account, is not given it.
	st.hosts["pc1"] = host
	for _, stranger := range []*conn{fakeConn("u2", "b9"), fakeConn("u1", "b2")} {
		st.ss.Attach(stranger, "S1", "pc1")
		if m := one(t, stranger); m.Type != proto.TypeBye || m.Reason != "gone" {
			t.Fatalf("%+v", m)
		}
	}
}

func TestSessionsBelongToTheBrowserNotToOneSocket(t *testing.T) {
	st := newSessionsTest(t)
	host, page := st.hosts["pc1"], fakeConn("u1", "b1")
	st.live(page, "S1")
	// A message of the session from another socket of the same browser binds the session to it.
	renewed := fakeConn("u1", "b1")
	st.ss.FromBrowser(renewed, proto.Msg{Type: proto.TypeCandidate, Session: "S1", Candidate: []byte(`{"candidate":"c"}`)})
	if m := one(t, host); m.Type != proto.TypeCandidate {
		t.Fatalf("%+v", m)
	}
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeAnswer, Session: "S1", Epoch: epoch(1)})
	if m := one(t, renewed); m.Type != proto.TypeAnswer {
		t.Fatalf("the session's messages go to the socket that last spoke for it: %+v", m)
	}
	nothing(t, page)
	// Another browser of the same account cannot act on it.
	other := fakeConn("u1", "b2")
	for _, typ := range []string{proto.TypeOffer, proto.TypeRestart, proto.TypeCandidate, proto.TypeBye} {
		st.ss.FromBrowser(other, proto.Msg{Type: typ, Session: "S1", Reason: "left"})
	}
	nothing(t, host)
	// B19: a bye queued while the socket renewed arrives on yet another socket of the browser,
	// with the session still bound to the old one: it is taken, and the computer is told.
	last := fakeConn("u1", "b1")
	st.ss.FromBrowser(last, proto.Msg{Type: proto.TypeBye, Session: "S1", Reason: "left"})
	if m := one(t, host); m.Type != proto.TypeBye || m.Reason != "left" {
		t.Fatalf("%+v", m)
	}
	if st.ss.records["S1"] != nil {
		t.Fatal("the bye ends the record")
	}
}

func TestSessionsAttachForAnUnknownSessionWaitsForTheHostsFirstStatus(t *testing.T) {
	st := newSessionsTest(t)
	page, lost, stranger := fakeConn("u1", "b1"), fakeConn("u1", "b2"), fakeConn("u1", "b3")
	st.ss.Attach(page, "S1", "pc1")
	st.ss.Attach(lost, "S2", "pc1")
	st.ss.Attach(stranger, "S3", "pc1")
	nothing(t, page)
	st.ss.Unbind(lost)
	st.ss.Status(st.hosts["pc1"], &proto.Holder{Session: "S1", Name: "Phone", User: "u1", Established: true})
	if m := one(t, page); m.Type != proto.TypeAttached || m.Session != "S1" || m.Host != "online" {
		t.Fatalf("%+v", m)
	}
	nothing(t, lost)
	if m := one(t, stranger); m.Type != proto.TypeBye || m.Session != "S3" || m.Reason != "gone" {
		t.Fatalf("a session the status does not name is gone: %+v", m)
	}
	// Once the host has reported, an unknown session is gone at once.
	st.ss.Attach(stranger, "S4", "pc1")
	if m := one(t, stranger); m.Reason != "gone" {
		t.Fatalf("%+v", m)
	}
	// A host that reconnects has not reported on its new socket yet: attaches wait again.
	host := st.hosts["pc1"]
	delete(st.hosts, "pc1")
	st.ss.HostDown("pc1")
	st.hosts["pc1"] = host
	st.ss.HostUp(host)
	got(page)
	st.ss.Attach(stranger, "S5", "pc1")
	nothing(t, stranger)
}

func TestSessionsAfterAServerRestartTheBrowserTheStatusNamesClaimsItsSession(t *testing.T) {
	st := newSessionsTest(t)
	st.ss.Status(st.hosts["pc1"], &proto.Holder{Session: "S1", Cred: credOf("b1"), Name: "Phone", User: "u1", Established: true})
	other, page := fakeConn("u1", "b2"), fakeConn("u1", "b1")
	st.ss.Attach(other, "S1", "pc1")
	if m := one(t, other); m.Reason != "gone" {
		t.Fatalf("another browser of the account, not the one holding the computer: %+v", m)
	}
	st.ss.Attach(page, "S1", "pc1")
	if m := one(t, page); m.Type != proto.TypeAttached {
		t.Fatalf("%+v", m)
	}
	if r := st.ss.records["S1"]; r.browser != "b1" {
		t.Fatalf("the session is the browser's again: %+v", r)
	}
}

func TestSessionsAStatusNoLongerNamingALiveSessionEndsIt(t *testing.T) {
	st := newSessionsTest(t)
	page, connecting := fakeConn("u1", "b1"), fakeConn("u1", "b2")
	st.live(page, "S1")
	st.challenge(connecting, "S2")
	st.ss.Status(st.hosts["pc1"], nil)
	if m := one(t, page); m.Type != proto.TypeBye || m.Session != "S1" || m.Reason != "ended" {
		t.Fatalf("%+v", m)
	}
	nothing(t, connecting)
	if st.ss.records["S1"] != nil || st.ss.records["S2"] == nil {
		t.Fatal("only the live record goes")
	}
}

func TestSessionsTellPagesTheirComputerWentAwayAndCameBack(t *testing.T) {
	st := newSessionsTest(t)
	page, unbound := fakeConn("u1", "b1"), fakeConn("u1", "b2")
	st.live(page, "S1")
	st.challenge(unbound, "S2")
	st.ss.Unbind(unbound)
	host := st.hosts["pc1"]
	delete(st.hosts, "pc1")
	st.ss.HostDown("pc1")
	if m := one(t, page); m.Type != proto.TypePresence || m.Session != "S1" || m.Host != "away" {
		t.Fatalf("%+v", m)
	}
	st.hosts["pc1"] = host
	st.ss.HostUp(host)
	nothing(t, page)
	st.ss.Status(host, &proto.Holder{Session: "S1"})
	if m := one(t, page); m.Type != proto.TypePresence || m.Host != "online" {
		t.Fatalf("%+v", m)
	}
	nothing(t, unbound)
	if st.ss.records["S1"] == nil {
		t.Fatal("a host dropping ends nothing")
	}
}

func TestSessionsARebootEndsTheOldHolderBeforeAnnouncingPresence(t *testing.T) {
	st := newSessionsTest(t)
	page := fakeConn("u1", "b1")
	st.live(page, "S1")
	old := st.hosts["pc1"]
	delete(st.hosts, "pc1")
	st.ss.HostDown("pc1")
	got(page)
	host := fakeConn("", "")
	host.deviceID = "pc1"
	st.hosts["pc1"] = host
	st.ss.HostUp(host)
	// Messages still buffered on the old socket cannot claim a holder or end it.
	st.ss.Status(old, &proto.Holder{Session: "S1"})
	st.ss.FromHost(old, proto.Msg{Type: proto.TypeBye, Session: "S1", Reason: "gone"})
	nothing(t, page)
	st.ss.Attach(page, "S1", "pc1")
	if m := one(t, page); m.Type != proto.TypeAttached || m.Host != "away" {
		t.Fatalf("the new socket has not named its sessions yet: %+v", m)
	}
	st.ss.Status(st.hosts["pc1"])
	if m := one(t, page); m.Type != proto.TypeBye || m.Session != "S1" || m.Reason != "ended" {
		t.Fatalf("the old session ends without ever being told to restart ICE: %+v", m)
	}
	st.challenge(page, "S2")
	if m := one(t, st.hosts["pc1"]); m.Type != proto.TypeChallenge || m.Session != "S2" {
		t.Fatalf("a fresh connection goes straight to the host: %+v", m)
	}
}

func TestSessionsANeverLiveSessionExpiresWithItsNonceAndALiveOneNever(t *testing.T) {
	st := newSessionsTest(t)
	connecting, live, pairing := fakeConn("u1", "b1"), fakeConn("u1", "b2"), fakeConn("u1", "b3")
	st.challenge(connecting, "S1")
	st.live(live, "S2")
	st.ss.Open(pairing, proto.Msg{Type: proto.TypePairAsk, Session: "P1", Device: "pc1", User: "u1", From: "Me"})
	st.ss.Sweep(st.now.Add(connectExpiry))
	nothing(t, connecting)
	st.ss.Sweep(st.now.Add(connectExpiry + time.Second))
	if m := one(t, connecting); m.Type != proto.TypeBye || m.Reason != "failed" {
		t.Fatalf("%+v", m)
	}
	nothing(t, pairing)
	st.ss.Sweep(st.now.Add(24 * time.Hour))
	if m := one(t, pairing); m.Reason != "failed" {
		t.Fatalf("a pairing request expires after its window: %+v", m)
	}
	nothing(t, live)
	if st.ss.records["S2"] == nil {
		t.Fatal("a live session has no timer on the server")
	}
}

func TestSessionsABusyByeTellsThePageWhoHoldsTheComputer(t *testing.T) {
	st := newSessionsTest(t)
	page := fakeConn("u1", "b1")
	st.challenge(page, "S1")
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeBye, Session: "S1", Reason: "busy", Holder: &proto.Holder{Session: "S0", Name: "Laptop", User: "u1"}})
	m := one(t, page)
	if m.Reason != "busy" || m.Controller == nil || m.Controller.Name != "Laptop" || !m.Controller.Connecting || m.Holder != nil {
		t.Fatalf("%+v", m)
	}
	if st.ss.records["S1"] != nil {
		t.Fatal("a bye from the host ends the record")
	}
	// A taken bye passes who took it and whether it was the same device.
	st.challenge(page, "S2")
	st.ss.FromHost(st.hosts["pc1"], proto.Msg{Type: proto.TypeBye, Session: "S2", Reason: "taken", By: "Phone", Same: true})
	if m := one(t, page); m.By != "Phone" || !m.Same || m.Controller != nil {
		t.Fatalf("%+v", m)
	}
}

// A computer's two seats: a person's session and an AI client's live side by side; a status
// that drops one of them ends only that one.
func TestSessionsBothSeatsNamedStayLiveAndEachEndsAlone(t *testing.T) {
	st := newSessionsTest(t)
	person, ai := fakeConn("u1", "b1"), fakeConn("u1", "ai1")
	st.challenge(person, "S1")
	st.challenge(ai, "A1")
	st.ss.Status(st.hosts["pc1"], &proto.Holder{Session: "S1"}, &proto.Holder{Session: "A1"})
	got(person)
	got(ai)
	got(st.hosts["pc1"])
	if st.ss.records["S1"] == nil || !st.ss.records["S1"].live || st.ss.records["A1"] == nil || !st.ss.records["A1"].live {
		t.Fatal("both named: both live")
	}
	st.ss.Status(st.hosts["pc1"], &proto.Holder{Session: "S1"}, nil)
	if m := one(t, ai); m.Type != proto.TypeBye || m.Session != "A1" || m.Reason != "ended" {
		t.Fatalf("%+v", m)
	}
	nothing(t, person)
}
