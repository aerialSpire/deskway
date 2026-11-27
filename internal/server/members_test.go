package server

import (
	"encoding/json"
	"net/http"
	"testing"
	"time"
)

// decode reads a JSON answer into v.
func (f *fixture) decode(res *http.Response, v any) {
	f.t.Helper()
	if err := json.NewDecoder(res.Body).Decode(v); err != nil {
		f.t.Fatal(err)
	}
}

// create makes a member through the admin's page and returns its id and link.
func (f *fixture) create(admin *http.Client, name string) (id, link string) {
	f.t.Helper()
	res := f.do(admin, "POST", "/api/members", map[string]string{"name": name})
	want(f.t, "create "+name, res.StatusCode, http.StatusOK)
	var body struct{ ID, Name, Link string }
	f.decode(res, &body)
	if body.Name != name || body.Link == "" {
		f.t.Fatalf("created %+v", body)
	}
	f.ids[name] = body.ID
	return body.ID, body.Link
}

func TestOnlyTheAdminManagesMembers(t *testing.T) {
	f := newFixture(t)
	member := f.signedIn("friend")
	for _, c := range []struct{ method, path string }{
		{"GET", "/api/members"},
		{"POST", "/api/members"},
		{"POST", "/api/members/" + f.id("me") + "/link"},
		{"DELETE", "/api/members/" + f.id("me") + "/link"},
		{"DELETE", "/api/members/" + f.id("me")},
	} {
		want(t, "a member: "+c.method+" "+c.path, f.status(member, c.method, c.path, map[string]string{"name": "x2"}), http.StatusForbidden)
		want(t, "signed out: "+c.method+" "+c.path, f.status(f.browser(), c.method, c.path, map[string]string{"name": "x2"}), http.StatusUnauthorized)
	}
	var me map[string]any
	f.decode(f.do(member, "GET", "/api/me", nil), &me)
	if me["admin"] != false {
		t.Fatalf("a member is told it is the admin: %v", me)
	}
	admin := f.signedIn("me")
	f.decode(f.do(admin, "GET", "/api/me", nil), &me)
	if me["admin"] != true {
		t.Fatalf("the admin is not told so: %v", me)
	}
	// The admin is managed by the server command.
	want(t, "issue a link for the admin", f.status(admin, "POST", "/api/members/"+f.id("me")+"/link", nil), http.StatusNotFound)
	want(t, "remove the admin", f.status(admin, "DELETE", "/api/members/"+f.id("me"), nil), http.StatusNotFound)
}

func TestTheAdminCreatesAndListsMembers(t *testing.T) {
	f := newFixture(t)
	admin := f.signedIn("me")
	want(t, "not a sign-in name", f.status(admin, "POST", "/api/members", map[string]string{"name": "小王"}), http.StatusBadRequest)
	want(t, "the admin's name", f.status(admin, "POST", "/api/members", map[string]string{"name": "me"}), http.StatusConflict)
	res := f.do(admin, "POST", "/api/members", map[string]string{"name": "wang", "display": " 小王 "})
	want(t, "with a display name", res.StatusCode, http.StatusOK)
	var made struct{ ID, Link string }
	f.decode(res, &made)
	f.create(admin, "li")
	f.s.store.putDevice("pc9", &DeviceRecord{Owner: made.ID, Name: "Wang's PC", TokenHash: hashToken("t9")})
	c := f.browser()
	want(t, "wang sets up their account", f.status(c, "POST", "/api/setup", setupRequest{Code: setupCode(made.Link), Password: testPassword, Device: &setupTestKey}), http.StatusNoContent)

	var list []Member
	f.decode(f.do(admin, "GET", "/api/members", nil), &list)
	if len(list) != 2 || list[0].Name != "li" || list[1].Name != "wang" {
		t.Fatalf("members: %+v", list)
	}
	li, wang := list[0], list[1]
	if wang.Display != "小王" || li.Display != "li" || wang.Computers != 1 || li.Computers != 0 {
		t.Fatalf("members: %+v", list)
	}
	if wang.Created == 0 || wang.LastActive < time.Now().Add(-time.Minute).Unix() || li.LastActive != 0 {
		t.Fatalf("created and last active: %+v", list)
	}
}

// Removing a member takes its browsers (signed out at once), its computers (their tokens stop
// working, their sockets close) and its account link.
func TestRemovingAMemberTakesEverythingOfIt(t *testing.T) {
	f := signalingFixture(t)
	admin := f.signedIn("me")
	member := f.signedIn("friend")
	f.s.store.putDevice("pc2", &DeviceRecord{Owner: f.id("friend"), Name: "Friend's PC", TokenHash: hashToken("tok-pc2")})
	sock := f.browserSocket(f.srv.URL, member)
	sock.next("devices")
	host := f.hostSocket(f.srv.URL, "pc2")
	id := f.id("friend")
	link := f.newMemberLink(admin, id)

	want(t, "a member cannot see the admin's computer", len(f.s.store.DevicesFor(id)), 1)
	if f.s.store.CanAccess(id, "pc1") {
		t.Fatal("a member reaches the admin's computer")
	}
	want(t, "removed", f.status(admin, "DELETE", "/api/members/"+id, nil), http.StatusNoContent)
	want(t, "again", f.status(admin, "DELETE", "/api/members/"+id, nil), http.StatusNotFound)
	want(t, "its browser is signed out", f.status(member, "GET", "/api/me", nil), http.StatusUnauthorized)
	sock.closed()
	host.closed()
	if _, approved := f.s.store.DeviceOwner("pc2"); approved || f.s.store.DeviceTokenOK("pc2", "tok-pc2") {
		t.Fatal("its computer is still there")
	}
	want(t, "its link is revoked", f.status(f.browser(), "POST", "/api/setup", setupRequest{Code: setupCode(link), Password: testPassword}), http.StatusGone)
	want(t, "it cannot sign in", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusUnauthorized)
	if _, approved := f.s.store.DeviceOwner("pc1"); !approved {
		t.Fatal("the admin's computer went too")
	}
}
