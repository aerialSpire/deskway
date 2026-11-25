package server

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	"github.com/pion/turn/v5"

	"deskway/internal/proto"
)

func relayFixture(t *testing.T) (*fixture, *atomic.Int64) {
	t.Helper()
	var now atomic.Int64
	now.Store(time.Now().Unix())
	f := newHTTPFixture(t, func(s *Server) {
		s.relays.now = func() time.Time { return time.Unix(now.Load(), 0) }
		s.turn.AccountMbps = 17
	}, httptest.NewTLSServer)
	return f, &now
}

func relayToken(f *fixture, admin *http.Client) string {
	f.t.Helper()
	res := f.do(admin, "POST", "/api/relays/token", nil)
	want(f.t, "create join token", res.StatusCode, http.StatusOK)
	var body struct{ Token string }
	f.decode(res, &body)
	if body.Token == "" {
		f.t.Fatal("no join token")
	}
	return body.Token
}

func joinRelay(f *fixture, token string) relayJoined {
	f.t.Helper()
	res := f.do(f.srv.Client(), "POST", "/api/relays/join", relayJoin{Token: token, PublicIP: "192.0.2.1", Port: 3478})
	want(f.t, "join", res.StatusCode, http.StatusOK)
	var joined relayJoined
	f.decode(res, &joined)
	return joined
}

func TestRelayJoinTokens(t *testing.T) {
	f, now := relayFixture(t)
	admin := f.signedIn("me")
	live, expiring := relayToken(f, admin), relayToken(f, admin)
	handler := f.s.Handler()
	const publicIP = "93.184.215.14"
	for _, tc := range []struct {
		name, token, source string
		advance             time.Duration
		status              int
	}{
		{"private source", live, "10.0.0.1", 0, http.StatusBadRequest},
		{"shared source", live, "100.64.0.1", 0, http.StatusBadRequest},
		{"link-local source", live, "169.254.0.1", 0, http.StatusBadRequest},
		{"IPv6 source", live, "2001:4860::1", 0, http.StatusBadRequest},
		{"valid", live, publicIP, 0, http.StatusOK},
		{"used", live, publicIP, 0, http.StatusForbidden},
		{"wrong", "wrong-token", publicIP, 0, http.StatusForbidden},
		{"expired", expiring, publicIP, relayJoinTTL, http.StatusForbidden},
	} {
		t.Run(tc.name, func(t *testing.T) {
			now.Add(int64(tc.advance / time.Second))
			body, err := json.Marshal(relayJoin{Token: tc.token, Port: 3478})
			if err != nil {
				t.Fatal(err)
			}
			req := httptest.NewRequest("POST", "https://desk.example/api/relays/join", bytes.NewReader(body))
			req.RemoteAddr = net.JoinHostPort(tc.source, "1234")
			res := httptest.NewRecorder()
			handler.ServeHTTP(res, req)
			want(t, tc.name, res.Code, tc.status)
			if tc.status == http.StatusOK {
				var joined relayJoined
				f.decode(res.Result(), &joined)
				if joined.ID == "" || joined.Secret == "" || joined.PublicIP != publicIP || joined.AccountMbps != 17 {
					t.Fatal("join did not return the node identity, observed address and account limit")
				}
			}
		})
	}
	if got := len(f.s.store.relayRecords()); got != 1 {
		t.Fatalf("stored %d relays, want 1", got)
	}
}

func TestRelayReportAuthenticationAndPresence(t *testing.T) {
	f, now := relayFixture(t)
	admin := f.signedIn("me")
	r := joinRelay(f, relayToken(f, admin))
	checkPresence := func(online bool, seen int64) {
		t.Helper()
		var list []relayView
		f.decode(f.do(admin, "GET", "/api/relays", nil), &list)
		if len(list) != 1 || list[0].Online != online || list[0].LastSeen != seen {
			t.Fatalf("relays = %+v, want online=%t last_seen=%d", list, online, seen)
		}
	}
	checkPresence(false, 0)
	at := now.Load()
	for _, tc := range []struct {
		name, secret string
		ts           int64
		status       int
	}{
		{"old", r.Secret, at - int64(relayReportInterval/time.Second) - 1, http.StatusUnauthorized},
		{"future", r.Secret, at + int64(relayReportInterval/time.Second) + 1, http.StatusUnauthorized},
		{"wrong MAC", "other secret", at, http.StatusUnauthorized},
		{"valid", r.Secret, at, http.StatusNoContent},
		{"replay", r.Secret, at, http.StatusUnauthorized},
	} {
		report := relayReport{ID: r.ID, TS: tc.ts, MAC: relayReportMAC(tc.secret, r.ID, tc.ts)}
		want(t, tc.name, f.status(f.srv.Client(), "POST", "/api/relays/report", report), tc.status)
	}
	now.Add(int64(relayOnlineTTL/time.Second) - 1)
	checkPresence(true, at)
	now.Add(1)
	checkPresence(false, at)
}

func TestOnlyTheAdminManagesRelays(t *testing.T) {
	f := newFixture(t)
	member := f.signedIn("friend")
	for _, c := range []struct{ method, path string }{
		{"GET", "/api/relays"}, {"POST", "/api/relays/token"},
		{"PATCH", "/api/relays/relay1"}, {"DELETE", "/api/relays/relay1"},
	} {
		want(t, "member "+c.method+" "+c.path, f.status(member, c.method, c.path, nil), http.StatusForbidden)
		want(t, "signed out "+c.method+" "+c.path, f.status(f.browser(), c.method, c.path, nil), http.StatusUnauthorized)
	}
}

func TestAdminRenamesAndRemovesARelay(t *testing.T) {
	f, now := relayFixture(t)
	admin := f.signedIn("me")
	r := joinRelay(f, relayToken(f, admin))
	path := "/api/relays/" + r.ID
	want(t, "rename", f.status(admin, "PATCH", path, map[string]string{"name": " Sydney "}), http.StatusNoContent)
	var list []map[string]any
	f.decode(f.do(admin, "GET", "/api/relays", nil), &list)
	if len(list) != 1 || list[0]["name"] != "Sydney" || list[0]["secret"] != nil {
		t.Fatal("admin list did not show the new name without the secret")
	}

	socket := f.browserSocket(f.srv.URL, admin)
	if got := len(socket.next("ice").ICE); got != 0 {
		t.Fatalf("offered %d relays before a report", got)
	}
	report := func(ts int64) relayReport {
		return relayReport{ID: r.ID, TS: ts, MAC: relayReportMAC(r.Secret, r.ID, ts)}
	}
	want(t, "report", f.status(f.srv.Client(), "POST", "/api/relays/report", report(now.Load())), http.StatusNoContent)
	if got := len(socket.next("ice").ICE); got != 1 {
		t.Fatalf("offered %d relays after a report", got)
	}
	want(t, "remove", f.status(admin, "DELETE", path, nil), http.StatusNoContent)
	if got := len(socket.next("ice").ICE); got != 0 {
		t.Fatalf("still offered %d relays after removal", got)
	}
	want(t, "removed relay reports", f.status(f.srv.Client(), "POST", "/api/relays/report", report(now.Add(1))), http.StatusUnauthorized)
	var stored Store
	if err := readState(f.data, &stored); err != nil || len(stored.Relays) != 0 {
		t.Fatalf("relay record still on disk or state unreadable: %v", err)
	}
}

func TestOpenBrowserSocketDropsAnExpiredRelay(t *testing.T) {
	f, now := relayFixture(t)
	admin := f.signedIn("me")
	r := joinRelay(f, relayToken(f, admin))
	ts := now.Load()
	report := relayReport{ID: r.ID, TS: ts, MAC: relayReportMAC(r.Secret, r.ID, ts)}
	want(t, "report", f.status(f.srv.Client(), "POST", "/api/relays/report", report), http.StatusNoContent)

	// Leave one second on the socket's expiry timer, then advance the relay clock past it.
	now.Add(int64(relayOnlineTTL/time.Second) - 1)
	socket := f.browserSocket(f.srv.URL, admin)
	if got := len(socket.next(proto.TypeICE).ICE); got != 1 {
		t.Fatalf("offered %d relays before expiry, want 1", got)
	}
	now.Add(1)
	if got := len(socket.next(proto.TypeICE).ICE); got != 0 {
		t.Fatalf("still offered %d relays after expiry", got)
	}
}

func TestICEProviderOffersNewestOnlineRelays(t *testing.T) {
	f := newFixtureWithServer(t, func(s *Server) {
		s.turn = TURNConfig{PublicIP: "203.0.113.10", Port: 3478, Secret: s.store.TURNSecret, TLSAddr: "desk.example:443"}
		s.stun = []string{"stun:stun.example:3478"}
	})
	s := f.s
	for _, tc := range []struct {
		n    int
		want []int
	}{
		{0, nil}, {1, []int{1}}, {4, []int{4, 3, 2}},
	} {
		t.Run(fmt.Sprint(tc.n), func(t *testing.T) {
			s.relays.mu.Lock()
			for i := len(s.store.relayRecords()) + 1; i <= tc.n; i++ {
				r := RelayRecord{ID: fmt.Sprint(i), PublicIP: fmt.Sprintf("192.0.2.%d", i), Port: 3478, Secret: randomHex(32)}
				if err := s.store.addRelay(r); err != nil {
					s.relays.mu.Unlock()
					t.Fatal(err)
				}
				s.relays.seen[r.ID] = relayPresence{received: time.Now()}
			}
			s.relays.mu.Unlock()
			const account = "account"
			ice := s.ICEProvider(account)
			if len(ice) != len(tc.want)+2 || !slices.Equal(ice[0].URLs, s.stun) ||
				!slices.Contains(ice[1].URLs, "turns:desk.example:443?transport=tcp") {
				t.Fatalf("unexpected ICE entries with %d nodes", tc.n)
			}
			for i, id := range tc.want {
				node := ice[i+2]
				addr := fmt.Sprintf("192.0.2.%d:3478", id)
				want := []string{"stun:" + addr, "turn:" + addr + "?transport=udp", "turn:" + addr + "?transport=tcp"}
				if !slices.Equal(node.URLs, want) {
					t.Fatalf("node %d URLs = %v, want %v", id, node.URLs, want)
				}
				attrs := turn.RequestAttributes{Username: node.Username, Realm: "deskway"}
				credential := turn.GenerateAuthKey(node.Username, attrs.Realm, node.Credential)
				for _, r := range s.store.relayRecords() {
					user, key, ok := turn.LongTermTURNRESTAuthHandler(r.Secret, nil)(&attrs)
					if !ok || user != account || bytes.Equal(credential, key) != (r.ID == fmt.Sprint(id)) {
						t.Fatalf("credential for node %d has unexpected authentication against node %s", id, r.ID)
					}
				}
			}
		})
	}
}
