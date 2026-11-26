package server

import (
	"bytes"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	"github.com/pion/turn/v5"
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
		{"GET", "/api/relays"}, {"GET", "/api/relays/check"}, {"POST", "/api/relays/token"},
		{"PATCH", "/api/relays/relay1"}, {"DELETE", "/api/relays/relay1"},
	} {
		want(t, "member "+c.method+" "+c.path, f.status(member, c.method, c.path, nil), http.StatusForbidden)
		want(t, "signed out "+c.method+" "+c.path, f.status(f.browser(), c.method, c.path, nil), http.StatusUnauthorized)
	}
}

func TestRelayCheck(t *testing.T) {
	f, now := relayFixture(t)
	f.s.turn.PublicIP, f.s.turn.Port = "203.0.113.10", 3478
	f.s.turn.Secret, f.s.turn.TLSAddr = f.s.store.TURNSecret, "desk.example:443"
	admin := f.signedIn("me")
	online := func() relayJoined {
		t.Helper()
		node := joinRelay(f, relayToken(f, admin))
		ts := now.Load()
		report := relayReport{ID: node.ID, TS: ts, MAC: relayReportMAC(node.Secret, node.ID, ts)}
		want(t, "report", f.status(f.srv.Client(), "POST", "/api/relays/report", report), http.StatusNoContent)
		return node
	}
	online() // This node's report will have expired by the time of the check.
	now.Add(int64(relayOnlineTTL / time.Second))
	first, second := online(), online()
	want(t, "rename", f.status(admin, "PATCH", "/api/relays/"+first.ID, map[string]string{"name": "Sydney"}), http.StatusNoContent)

	var list []relayCheck
	res := f.do(admin, "GET", "/api/relays/check", nil)
	if res.Header.Get("Cache-Control") != "no-store" {
		t.Fatal("the check response can be cached")
	}
	f.decode(res, &list)
	if len(list) != 3 {
		t.Fatalf("listed %d relays, want the main relay and two online nodes", len(list))
	}
	nodeURLs := []string{"stun:192.0.2.1:3478", "turn:192.0.2.1:3478?transport=udp", "turn:192.0.2.1:3478?transport=tcp"}
	for i, tc := range []struct {
		name, secret string
		urls         []string
	}{
		{"", f.s.turn.Secret, []string{"stun:203.0.113.10:3478", "turn:203.0.113.10:3478?transport=udp", "turn:203.0.113.10:3478?transport=tcp", "turns:desk.example:443?transport=tcp"}},
		{"Sydney", first.Secret, nodeURLs},
		{second.PublicIP, second.Secret, nodeURLs},
	} {
		item := list[i]
		if item.Name != tc.name || !slices.Equal(item.ICE.URLs, tc.urls) {
			t.Errorf("entry %d: got name %q and URLs %v", i, item.Name, item.ICE.URLs)
		}
		auth := turn.LongTermTURNRESTAuthHandler(tc.secret, nil)
		user, key, ok := auth(&turn.RequestAttributes{Username: item.ICE.Username, Realm: "deskway"})
		if !ok || user != f.id("me") || !bytes.Equal(key, turn.GenerateAuthKey(item.ICE.Username, "deskway", item.ICE.Credential)) {
			t.Errorf("entry %d: credentials do not authenticate the admin with this relay's secret", i)
		}
	}
}

func TestRelayCheckWithoutConfiguredRelays(t *testing.T) {
	f := newFixture(t)
	var list []relayCheck
	f.decode(f.do(f.signedIn("me"), "GET", "/api/relays/check", nil), &list)
	if list == nil || len(list) != 0 {
		t.Fatal("expected an empty list of relays")
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

	report := func(ts int64) relayReport {
		return relayReport{ID: r.ID, TS: ts, MAC: relayReportMAC(r.Secret, r.ID, ts)}
	}
	want(t, "report", f.status(f.srv.Client(), "POST", "/api/relays/report", report(now.Load())), http.StatusNoContent)
	want(t, "remove", f.status(admin, "DELETE", path, nil), http.StatusNoContent)
	want(t, "removed relay reports", f.status(f.srv.Client(), "POST", "/api/relays/report", report(now.Add(1))), http.StatusUnauthorized)
	var stored Store
	if err := readState(f.data, &stored); err != nil || len(stored.Relays) != 0 {
		t.Fatalf("relay record still on disk or state unreadable: %v", err)
	}
}

func TestNearestRelay(t *testing.T) {
	main := TURNConfig{PublicIP: "203.0.113.10", Port: 3478, Secret: "main-secret"}
	node := RelayRecord{ID: "node", PublicIP: "192.0.2.1", Port: 3478, Secret: "node-secret"}
	other := RelayRecord{ID: "other", PublicIP: "192.0.2.2", Port: 3478, Secret: "other-secret"}
	online := []RelayRecord{node, other}
	relay := TURNConfig{PublicIP: node.PublicIP, Port: node.Port, Secret: node.Secret}
	second := TURNConfig{PublicIP: other.PublicIP, Port: other.Port, Secret: other.Secret}
	for _, tc := range []struct {
		name   string
		main   TURNConfig
		online []RelayRecord
		rtts   map[string]int
		want   TURNConfig
	}{
		{"no measurement", main, online, nil, main},
		{"main nearest", main, online, map[string]int{mainRelayID: 10, node.ID: 20}, main},
		{"a node nearest", main, online, map[string]int{mainRelayID: 20, node.ID: 0, other.ID: 10}, relay},
		{"nearest node offline", main, online[1:], map[string]int{mainRelayID: 20, node.ID: 5, other.ID: 10}, second},
		{"main relay not configured", TURNConfig{}, online, map[string]int{mainRelayID: 1, node.ID: 5}, relay},
		{"no main relay or usable measurement", TURNConfig{}, online, map[string]int{other.ID: -1}, relay},
		{"no main relay or online node", TURNConfig{}, nil, nil, TURNConfig{}},
		{"unknown id", main, online, map[string]int{"unknown": 1}, main},
		{"negative measurement", main, online, map[string]int{node.ID: -1}, main},
		{"tie", main, online, map[string]int{mainRelayID: 5, node.ID: 5}, main},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := nearestRelay(tc.main, tc.online, tc.rtts); got != tc.want {
				t.Fatalf("nearest relay address = %s, want %s", got.PublicIP, tc.want.PublicIP)
			}
		})
	}
}
