package server

import (
	"context"
	"encoding/json"
	"net/http"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"deskway/internal/proto"
)

// Enrolling computers, approving them, and their owner renaming and removing them.

func TestNewComputerStaysHiddenUntilApproved(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	res := f.do(f.browser(), "POST", "/api/host/enroll", map[string]string{"user": "me", "password": testPassword, "name": "PC", "os": "windows"})
	want(t, "enroll", res.StatusCode, http.StatusOK)
	var dev struct{ ID, Token string }
	if err := json.NewDecoder(res.Body).Decode(&dev); err != nil {
		t.Fatal(err)
	}
	if n := len(f.s.deviceList(&conn{user: f.id("me")})); n != 0 {
		t.Errorf("waiting computer is listed")
	}
	if f.s.store.CanAccess(f.id("me"), dev.ID) {
		t.Errorf("waiting computer can be reached")
	}
	dial := func() (*websocket.Conn, *http.Response, error) {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		return websocket.Dial(ctx, "ws"+strings.TrimPrefix(f.srv.URL, "http")+"/ws/host?id="+dev.ID, &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + dev.Token}}})
	}
	_, r, err := dial()
	if err == nil || r == nil || r.StatusCode != http.StatusForbidden {
		t.Fatalf("waiting computer connected: %v %v", err, r)
	}
	list := f.pending()
	if len(list) != 1 || list[0].Kind != "device" || list[0].Label != "PC" {
		t.Fatalf("approvals: %+v", list)
	}
	want(t, "approve", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "device", "id": dev.ID, "allow": true}), http.StatusNoContent)
	ws, _, err := dial()
	if err != nil {
		t.Fatalf("approved computer: %v", err)
	}
	ws.CloseNow()
	if n := len(f.s.deviceList(&conn{user: f.id("me")})); n != 1 {
		t.Errorf("approved computer not listed")
	}
}

// The approval shows a computer's six digits only when it said it shows them too: an older
// computer has none to compare.
func TestTheApprovalShowsTheCodeOnlyOfAComputerThatShowsIt(t *testing.T) {
	for _, shows := range []bool{true, false} {
		f := newFixture(t)
		f.signedIn("me")
		res := f.do(f.browser(), "POST", "/api/host/enroll", map[string]any{"user": "me", "password": testPassword, "name": "PC", "os": "macos", "shows_code": shows})
		var dev struct{ ID string }
		if err := json.NewDecoder(res.Body).Decode(&dev); err != nil {
			t.Fatal(err)
		}
		want := ""
		if shows {
			want = proto.ComputerCode(dev.ID)
		}
		if list := f.pending(); len(list) != 1 || list[0].Code != want {
			t.Errorf("shows %v: approvals %+v, want code %q", shows, list, want)
		}
	}
}

// A computer signing in again names itself: the same account takes it back as it was, any
// other account, or a computer removed meanwhile, gets a new one.
func TestSigningInAgainTakesTheSameComputerBack(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	f.addAccount("other")
	enroll := func(user, back string) (id, token string) {
		t.Helper()
		res := f.do(f.browser(), "POST", "/api/host/enroll", map[string]string{"user": user, "password": testPassword, "name": "PC", "os": "macos", "back": back})
		want(t, "enroll "+user, res.StatusCode, http.StatusOK)
		var dev struct{ ID, Token string }
		if err := json.NewDecoder(res.Body).Decode(&dev); err != nil {
			t.Fatal(err)
		}
		return dev.ID, dev.Token
	}
	id, token := enroll("me", "")
	want(t, "approve", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "device", "id": id, "allow": true}), http.StatusNoContent)

	back, none := enroll("me", id)
	if back != id || none != "" {
		t.Fatalf("taken back as %q with token %q", back, none)
	}
	if !f.s.store.DeviceTokenOK(id, token) || !f.s.store.CanAccess(f.id("me"), id) || len(f.pending()) != 0 {
		t.Fatalf("the computer did not come back as it was")
	}
	if other, tok := enroll("other", id); other == id || tok == "" {
		t.Fatalf("another account took the computer: %q", other)
	}
	if _, err := f.s.store.RemoveDevice(f.id("me"), id); err != nil {
		t.Fatal(err)
	}
	if again, tok := enroll("me", id); again == id || tok == "" {
		t.Fatalf("a removed computer came back")
	}
}

func TestDeniedComputerLosesItsToken(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	res := f.do(f.browser(), "POST", "/api/host/enroll", map[string]string{"user": "me", "password": testPassword, "name": "PC", "os": "windows"})
	var dev struct{ ID, Token string }
	if err := json.NewDecoder(res.Body).Decode(&dev); err != nil {
		t.Fatal(err)
	}
	want(t, "deny", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "device", "id": dev.ID, "allow": false}), http.StatusNoContent)
	req, _ := http.NewRequest("POST", f.srv.URL+"/api/host/leave?id="+dev.ID, nil)
	req.Header.Set("Authorization", "Bearer "+dev.Token)
	r, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Body.Close()
	want(t, "token after deny", r.StatusCode, http.StatusUnauthorized)
}

func TestWaitingComputersAreBounded(t *testing.T) {
	f := newFixture(t)
	last := 0
	for range maxPendingPerUser + 1 {
		last = f.status(f.browser(), "POST", "/api/host/enroll", map[string]string{"user": "me", "password": testPassword, "name": "PC", "os": "windows"})
	}
	want(t, "over the limit", last, http.StatusTooManyRequests)
}

// An account has at most maxComputersPerUser computers, waiting ones included; another
// account's computers do not count.
func TestAnAccountsComputersAreBounded(t *testing.T) {
	f := newFixture(t)
	f.addAccount("friend")
	f.s.store.putDevice("theirs", &DeviceRecord{Owner: f.id("friend"), Name: "x", TokenHash: hashToken("t")})
	for i := range maxComputersPerUser - 1 {
		f.s.store.putDevice(randomHex(4), &DeviceRecord{Owner: f.id("me"), Name: "PC", TokenHash: hashToken(randomHex(4)), Pending: i == 0})
	}
	enroll := func() int {
		return f.status(f.browser(), "POST", "/api/host/enroll", map[string]string{"user": "me", "password": testPassword, "name": "PC", "os": "windows"})
	}
	want(t, "the last one", enroll(), http.StatusOK)
	want(t, "one more", enroll(), http.StatusForbidden)
}

func TestOwnerRenamesAndRemovesComputers(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), Name: "old", TokenHash: hashToken("tok")})
	f.s.store.putDevice("theirs", &DeviceRecord{Owner: "friend", Name: "x", TokenHash: hashToken("tok2")})
	want(t, "rename", f.status(a, "PATCH", "/api/computers/pc1", map[string]string{"name": "  书房电脑 "}), http.StatusNoContent)
	if d := f.s.store.DevicesFor(f.id("me")); len(d) != 1 || d[0].Name != "书房电脑" {
		t.Fatalf("names are %+v", d)
	}
	want(t, "empty name", f.status(a, "PATCH", "/api/computers/pc1", map[string]string{"name": " "}), http.StatusBadRequest)
	want(t, "someone else's", f.status(a, "DELETE", "/api/computers/theirs", nil), http.StatusNotFound)
	f.s.forwardThumb("pc1", "jpeg", "0")
	want(t, "remove", f.status(a, "DELETE", "/api/computers/pc1", nil), http.StatusNoContent)
	if _, approved := f.s.store.DeviceOwner("pc1"); approved {
		t.Fatal("still there")
	}
	f.s.mu.Lock()
	previews := len(f.s.previews["pc1"])
	f.s.mu.Unlock()
	if previews != 0 {
		t.Fatal("the removed computer's previews are kept")
	}
	req, _ := http.NewRequest("GET", f.srv.URL+"/ws/host?id=pc1", nil)
	req.Header.Set("Authorization", "Bearer tok")
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	if res.StatusCode != http.StatusUnauthorized {
		t.Fatalf("removed computer's token still works: %d", res.StatusCode)
	}
}

// The device list tells each browser who holds a computer as it sees them: its own device by the
// credential the host verified, or another device of the account.
func TestTheDeviceListSaysWhoHoldsAComputerAsEachBrowserSeesIt(t *testing.T) {
	f := signalingFixture(t)
	f.s.store.mu.Lock()
	f.s.store.Browsers["b1"] = &BrowserRecord{Owner: f.id("me"), Approved: true, Key: &proto.BrowserKey{Cred: "C1"}}
	f.s.store.Browsers["b2"] = &BrowserRecord{Owner: f.id("me"), Approved: true, Key: &proto.BrowserKey{Cred: "C2"}}
	f.s.store.mu.Unlock()
	setHolder := func(h *proto.Holder) {
		f.s.mu.Lock()
		f.s.hosts["pc1"] = &conn{deviceID: "pc1", holder: h, cancel: func() {}}
		f.s.mu.Unlock()
	}
	controller := func(c *conn) proto.Controller {
		t.Helper()
		l := f.s.deviceList(c)
		if len(l) != 1 || l[0].Controller == nil {
			t.Fatalf("%+v", l)
		}
		return *l[0].Controller
	}
	setHolder(&proto.Holder{Session: "S1", Cred: "C1", Name: "Phone", User: f.id("me")})
	cases := []struct {
		name string
		page *conn
		want proto.Controller
	}{
		{"this device", &conn{user: f.id("me"), browser: "b1"}, proto.Controller{Device: true, Name: "Phone", Connecting: true}},
		{"another device of this account", &conn{user: f.id("me"), browser: "b2"}, proto.Controller{Name: "Phone", Connecting: true}},
	}
	for _, c := range cases {
		if got := controller(c.page); got != c.want {
			t.Errorf("%s: %+v, want %+v", c.name, got, c.want)
		}
	}
	// Established, with device checks off (no credential): never this device by accident.
	setHolder(&proto.Holder{Session: "S1", Name: "Password", User: f.id("me"), Established: true})
	if got := controller(&conn{user: f.id("me"), browser: "b4"}); got != (proto.Controller{Name: "Password"}) {
		t.Errorf("%+v", got)
	}
	// Nobody holds it: no controller.
	setHolder(nil)
	if l := f.s.deviceList(&conn{user: f.id("me")}); l[0].Controller != nil {
		t.Errorf("%+v", l[0].Controller)
	}
}

func TestDetachedTerminalStatusIsVisibleToTheComputersAccount(t *testing.T) {
	f := signalingFixture(t)
	owner := f.id("me")
	f.s.store.mu.Lock()
	f.s.store.Browsers["phone"] = &BrowserRecord{
		Owner: owner, Approved: true, Key: &proto.BrowserKey{Cred: "C1", Name: "Renamed phone"},
	}
	f.s.store.mu.Unlock()
	host := &conn{deviceID: "pc1", cancel: func() {}}
	f.s.mu.Lock()
	f.s.hosts[host.deviceID] = host
	f.s.mu.Unlock()

	for _, tc := range []struct {
		label string
		sent  *proto.KeptTerminal
		want  *proto.KeptTerminal
	}{
		{"left", &proto.KeptTerminal{Since: 123, Name: "Phone", Cred: "C1"}, &proto.KeptTerminal{Since: 123, Name: "Renamed phone"}},
		{"reattached or closed", nil, nil},
		{"no device key", &proto.KeptTerminal{Since: 456, Name: "Browser"}, &proto.KeptTerminal{Since: 456, Name: "Browser"}},
		{"invalid", &proto.KeptTerminal{Name: "Browser"}, nil},
	} {
		t.Run(tc.label, func(t *testing.T) {
			f.s.onStatus(host, proto.Msg{KeptTerminal: tc.sent})
			for _, browser := range []string{"phone", "other"} {
				d := f.s.deviceList(&conn{user: owner, browser: browser})[0]
				if !reflect.DeepEqual(d.KeptTerminal, tc.want) {
					t.Errorf("%s got %+v, want %+v", browser, d.KeptTerminal, tc.want)
				}
			}
			if devices := f.s.deviceList(&conn{user: "another-account"}); len(devices) != 0 {
				t.Fatalf("another account got %v", devices)
			}
			if d := f.s.deviceList(&conn{user: owner, ai: true})[0]; d.KeptTerminal != nil {
				t.Fatalf("AI client got terminal status: %+v", d.KeptTerminal)
			}
		})
	}
}
