package server

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"deskway/internal/proto"
)

// Approving browsers, removing them, and the signed device log of their keys.

func TestApprovalGivesOneSession(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	second := f.browser()
	want(t, "second login", f.login(second).StatusCode, http.StatusAccepted)
	list := f.pending()
	if len(list) != 1 || list[0].Kind != "browser" || list[0].Code == "" {
		t.Fatalf("approvals: %+v", list)
	}
	want(t, "approve", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": list[0].ID, "allow": true}), http.StatusNoContent)
	want(t, "pickup", f.status(second, "GET", "/api/login/status", nil), http.StatusNoContent)
	want(t, "session works", f.status(second, "GET", "/api/me", nil), http.StatusOK)
	if n := len(f.pending()); n != 0 {
		t.Errorf("%d requests left", n)
	}

	// Signing out must not leave a way back in without the password.
	want(t, "logout", f.status(second, "POST", "/api/logout", nil), http.StatusNoContent)
	want(t, "status after pickup", f.status(second, "GET", "/api/login/status", nil), http.StatusGone)
	want(t, "signed out", f.status(second, "GET", "/api/me", nil), http.StatusUnauthorized)

	// With the password it comes back without another approval.
	want(t, "trusted login", f.login(second).StatusCode, http.StatusNoContent)
	want(t, "wrong password on trusted browser", f.status(second, "POST", "/api/login", map[string]string{"user": "me", "password": "nope"}), http.StatusUnauthorized)
}

func TestWaitingBrowserCannotApproveItself(t *testing.T) {
	f := newFixture(t)
	f.signedIn("me")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	want(t, "self approval", f.status(second, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": true}), http.StatusUnauthorized)
}

func TestDenied(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	want(t, "deny", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": false}), http.StatusNoContent)
	want(t, "status after deny", f.status(second, "GET", "/api/login/status", nil), http.StatusGone)
	want(t, "no session", f.status(second, "GET", "/api/me", nil), http.StatusUnauthorized)
}

func TestRequestExpires(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	f.s.store.ageBrowser(id, pendingBrowserTTL+time.Minute)

	if n := len(f.pending()); n != 0 {
		t.Errorf("expired request still listed")
	}
	want(t, "approve expired", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": true}), http.StatusNotFound)
	want(t, "status expired", f.status(second, "GET", "/api/login/status", nil), http.StatusGone)
}

func TestApprovedThenNotCollectedExpires(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": true})
	f.s.store.ageBrowser(id, pendingBrowserTTL+time.Minute)

	want(t, "late pickup", f.status(second, "GET", "/api/login/status", nil), http.StatusGone)
}

func TestPendingRequestsAreBounded(t *testing.T) {
	f := newFixture(t)
	for range maxPendingPerUser * 3 {
		f.login(f.browser())
	}
	if n := len(f.pending()); n > maxPendingPerUser {
		t.Errorf("%d waiting requests", n)
	}
}

func TestRemovingABrowserEndsLoginAndSocket(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": true})
	f.status(second, "GET", "/api/login/status", nil)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ws, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(f.srv.URL, "http")+"/ws/client", &websocket.DialOptions{HTTPClient: second})
	if err != nil {
		t.Fatal(err)
	}
	defer ws.CloseNow()

	var rows []struct {
		ID      string `json:"id"`
		Current bool   `json:"current"`
	}
	res := f.do(first, "GET", "/api/devices", nil)
	if err := json.NewDecoder(res.Body).Decode(&rows); err != nil || len(rows) != 2 {
		t.Fatalf("devices: %v %+v", err, rows)
	}
	want(t, "remove", f.status(first, "DELETE", "/api/browsers/"+id, nil), http.StatusNoContent)
	want(t, "session gone", f.status(second, "GET", "/api/me", nil), http.StatusUnauthorized)
	for {
		if _, _, err := ws.Read(ctx); err != nil {
			if ctx.Err() != nil {
				t.Fatal("socket stayed open after removal")
			}
			break
		}
	}
	// Coming back takes approval again, even with the password.
	want(t, "login after removal", f.login(second).StatusCode, http.StatusAccepted)
}

func TestOtherAccountsCannotActOnRequests(t *testing.T) {
	f := newFixture(t)
	f.signedIn("me")
	friend := f.signedIn("friend")
	second := f.browser()
	f.login(second)
	id := f.pending()[0].ID
	want(t, "friend approves", f.status(friend, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": id, "allow": true}), http.StatusNotFound)
	want(t, "friend removes", f.status(friend, "DELETE", "/api/browsers/"+id, nil), http.StatusNotFound)
	want(t, "still waiting", f.status(second, "GET", "/api/login/status", nil), http.StatusAccepted)
}

func TestPolicyIsLoggedAndReported(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	get := func() bool {
		res := f.do(a, "GET", "/api/policy", nil)
		var v struct{ Keys bool }
		_ = json.NewDecoder(res.Body).Decode(&v)
		return v.Keys
	}
	if !get() {
		t.Fatal("checks should be on by default")
	}
	want(t, "no proof", f.status(a, "POST", "/api/policy", map[string]any{"keys": false}), http.StatusBadRequest)
	want(t, "off", f.status(a, "POST", "/api/policy", map[string]any{"keys": false, "proof": map[string]any{"ts": 1, "by": "ZA", "sig": "cw"}}), http.StatusNoContent)
	if get() {
		t.Fatal("still on")
	}
}

func TestBrowserKeyApprovalAndRemovalAreLogged(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	firstKey := map[string]any{"cred": "Y3JlZDE", "alg": -7, "key": "a2V5MQ==", "name": "iPhone"}
	want(t, "first attaches its key", f.status(first, "POST", "/api/login/device", firstKey), http.StatusOK)
	second := f.browser()
	f.login(second)
	newKey := map[string]any{"cred": "Y3JlZDI", "alg": -7, "key": "a2V5Mg==", "name": "iPad"}
	want(t, "waiting browser attaches its key", f.status(second, "POST", "/api/login/device", newKey), http.StatusOK)
	reqs := f.pending()
	if len(reqs) != 1 || reqs[0].Code != (proto.BrowserKey{Key: "a2V5Mg=="}).Code() || len(reqs[0].Device) == 0 {
		t.Fatalf("approval does not carry the key and its code: %+v", reqs)
	}
	proof := map[string]any{"ts": 1, "by": "Y3JlZDE", "ad": "YQ", "cd": "Yw", "sig": "cw"}
	want(t, "approve with proof", f.status(first, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": reqs[0].ID, "allow": true, "endorse": proof}), http.StatusNoContent)
	logOf := func() []proto.LogEntry {
		l, _ := f.s.store.DeviceLog(f.id("me"))
		return l
	}
	l := logOf()
	if len(l) != 2 || l[0].By != "" || l[1].Op != "add" || l[1].Cred != "Y3JlZDI" || l[1].By != "Y3JlZDE" {
		t.Fatalf("unexpected log: %+v", l)
	}
	want(t, "second collects its session", f.status(second, "GET", "/api/login/status", nil), http.StatusNoContent)
	var id string
	for _, b := range f.s.store.ApprovedBrowsers(f.id("me")) {
		if b.Key != nil && b.Key.Cred == "Y3JlZDI" {
			id = b.ID
		}
	}
	// Renamed on another device: the website shows it at once, the log carries it, signed, to
	// the computers, and only the newest rename of a device stays in the log.
	want(t, "untrimmed name", f.status(first, "PATCH", "/api/browsers/"+id, map[string]any{"name": " 小明的 iPad", "proof": proof}), http.StatusBadRequest)
	want(t, "rename", f.status(first, "PATCH", "/api/browsers/"+id, map[string]any{"name": "旧名字", "proof": proof}), http.StatusNoContent)
	want(t, "rename again", f.status(first, "PATCH", "/api/browsers/"+id, map[string]any{"name": "小明的 iPad", "proof": proof}), http.StatusNoContent)
	if l := logOf(); len(l) != 3 || l[2].Op != "rename" || l[2].Cred != "Y3JlZDI" || l[2].Name != "小明的 iPad" || l[2].By != "Y3JlZDE" {
		t.Fatalf("rename not logged once, newest: %+v", l)
	}
	want(t, "the device telling its key again", f.status(second, "POST", "/api/login/device", newKey), http.StatusOK)
	for _, b := range f.s.store.ApprovedBrowsers(f.id("me")) {
		if b.ID == id && (b.Key == nil || b.Key.Name != "小明的 iPad") {
			t.Fatalf("the name went back: %+v", b.Key)
		}
	}
	want(t, "remove with proof", f.status(first, "DELETE", "/api/browsers/"+id, map[string]any{"proof": proof}), http.StatusNoContent)
	if l := logOf(); len(l) != 4 || l[3].Op != "remove" || l[3].Cred != "Y3JlZDI" {
		t.Fatalf("removal not logged: %+v", l)
	}
	want(t, "removed device is signed out", f.status(second, "GET", "/api/me", nil), http.StatusUnauthorized)
}
