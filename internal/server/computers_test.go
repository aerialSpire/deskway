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

func TestAdminEnrollmentLink(t *testing.T) {
	f := newFixture(t)
	browser := f.signedIn("me")
	issue := func() string {
		t.Helper()
		link, err := IssueAdminEnrollLink(f.data)
		if err != nil {
			t.Fatal(err)
		}
		code, ok := strings.CutPrefix(link, "/api/computers/enroll#c=")
		if !ok || code == "" {
			t.Fatal("expected an enrollment token")
		}
		return code
	}
	old, token := issue(), issue()
	links, err := openLinks(f.data)
	if err != nil {
		t.Fatal(err)
	}
	expiry := time.Until(time.Unix(links.records[enrollLinkID].Expires, 0))
	links.Close()
	if expiry > 10*time.Minute || expiry < 9*time.Minute {
		t.Fatalf("enrollment token lifetime: %v", expiry)
	}
	for _, tc := range []struct {
		label, token, user string
		status             int
	}{
		{"superseded", old, "", http.StatusUnauthorized},
		{"mixed credential", token, "me", http.StatusBadRequest},
		{"enroll", token, "", http.StatusOK},
		{"replay", token, "", http.StatusUnauthorized},
	} {
		res := f.do(browser, "POST", "/api/computers/enroll", map[string]string{
			"name": "Local hub", "os": "windows", "enroll_token": tc.token, "user": tc.user,
		})
		want(t, tc.label, res.StatusCode, tc.status)
		if tc.status != http.StatusOK {
			res.Body.Close()
			continue
		}
		var got struct{ ID, Token, User string }
		f.decode(res, &got)
		if got.User != "me" || !f.s.store.DeviceTokenOK(got.ID, got.Token) || !f.s.store.CanAccess(f.id("me"), got.ID) {
			t.Fatal("enrollment must return an approved computer and its credential")
		}
	}
	if got := f.s.deviceList(&conn{user: f.id("me")}); len(got) != 1 || got[0].Name != "Local hub" {
		t.Fatal("the approved hub must appear in the admin's list")
	}
	expired := issue()
	links, err = openLinks(f.data)
	if err != nil {
		t.Fatal(err)
	}
	record := links.records[enrollLinkID]
	record.Expires = time.Now().Add(-time.Second).Unix()
	links.records[enrollLinkID] = record
	err = links.save()
	links.Close()
	if err != nil {
		t.Fatal(err)
	}
	want(t, "expired enrollment link", f.status(browser, "POST", "/api/computers/enroll", map[string]string{
		"name": "Local hub", "enroll_token": expired,
	}), http.StatusUnauthorized)
}

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
	host := fakeConn("", "")
	host.deviceID = "pc1"
	f.s.mu.Lock()
	f.s.hosts[host.deviceID] = host
	f.s.mu.Unlock()
	f.s.forwardThumb(host, "jpeg", "0")
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

func TestTerminalStatusIsVisibleToTheComputersAccount(t *testing.T) {
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

	fromPhone := proto.KeptTerminal{Since: 123, Name: "Phone", Cred: "C1"}
	renamed := proto.KeptTerminal{Since: 123, Name: "Renamed phone"}
	other := proto.KeptTerminal{Since: 456, Name: "Browser"}
	invalid := proto.KeptTerminal{Name: "Browser"}
	for _, tc := range []struct {
		label     string
		kept      *proto.KeptTerminal
		wantKept  *proto.KeptTerminal
		terminals []proto.TerminalStatus
		want      []proto.TerminalStatus
	}{
		{"left", &fromPhone, &renamed, nil, nil},
		{"reattached or closed", nil, nil, nil, nil},
		{"no device key", &other, &other, nil, nil},
		{"invalid", &invalid, nil, nil, nil},
		{"multiple", &fromPhone, &renamed, []proto.TerminalStatus{
			{KeptTerminal: fromPhone, ID: 1, Left: 700},
			{KeptTerminal: other, ID: 2, Attached: true},
			{KeptTerminal: invalid, ID: 3},
		}, []proto.TerminalStatus{
			{KeptTerminal: renamed, ID: 1, Left: 700},
			{KeptTerminal: other, ID: 2, Attached: true},
		}},
		{"all closed", nil, nil, nil, nil},
	} {
		t.Run(tc.label, func(t *testing.T) {
			f.s.onStatus(host, proto.Msg{KeptTerminal: tc.kept, Terminals: tc.terminals})
			for _, c := range []*conn{
				{user: owner, browser: "phone", caps: []string{proto.CapTerminals}},
				{user: owner, browser: "other"},
				{user: owner, ai: true, caps: []string{proto.CapTerminals}},
			} {
				wantKept, want := tc.wantKept, tc.want
				if c.ai {
					wantKept = nil
				}
				if c.ai || len(c.caps) == 0 {
					want = nil
				}
				d := f.s.deviceList(c)[0]
				if !reflect.DeepEqual(d.KeptTerminal, wantKept) || !reflect.DeepEqual(d.Terminals, want) {
					t.Errorf("%s (AI: %v) got kept %+v and list %+v, want kept %+v and list %+v", c.browser, c.ai, d.KeptTerminal, d.Terminals, wantKept, want)
				}
			}
			if devices := f.s.deviceList(&conn{user: "another-account"}); len(devices) != 0 {
				t.Fatalf("another account got %v", devices)
			}
		})
	}
}

func TestTerminalHereRequiresTakeoverOnBothSignalingHops(t *testing.T) {
	f := signalingFixture(t)
	for _, hostCap := range []bool{false, true} {
		host := &conn{deviceID: "pc1", caps: []string{proto.CapTerminals}, cancel: func() {}}
		if hostCap {
			host.caps = append(host.caps, proto.CapTerminalTakeover)
		}
		f.s.mu.Lock()
		f.s.hosts[host.deviceID] = host
		f.s.mu.Unlock()
		f.s.onStatus(host, proto.Msg{Terminals: []proto.TerminalStatus{{
			KeptTerminal: proto.KeptTerminal{Since: 123, Name: "This computer"},
			ID:           7, Attached: true, Here: true,
		}}})
		for _, browserCap := range []bool{false, true} {
			c := &conn{user: f.id("me"), caps: []string{proto.CapTerminals}}
			if browserCap {
				c.caps = append(c.caps, proto.CapTerminalTakeover)
			}
			got := f.s.deviceList(c)[0].Terminals
			if len(got) != 1 || got[0].Here != (hostCap && browserCap) || got[0].Name != "This computer" || got[0].Cred != "" {
				t.Fatalf("host %t, browser %t: %+v", hostCap, browserCap, got)
			}
		}
	}
}
