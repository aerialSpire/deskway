package server

import (
	"encoding/json"
	"net/http"
	"testing"
	"time"

	"deskway/internal/proto"
)

// The admin's members (docs/accounts.md): only the admin manages them; a member starts with an
// initial password, its first browser is let in with it, and nothing opens until it has chosen
// its own.

// decode reads a JSON answer into v.
func (f *fixture) decode(res *http.Response, v any) {
	f.t.Helper()
	if err := json.NewDecoder(res.Body).Decode(v); err != nil {
		f.t.Fatal(err)
	}
}

// create makes a member through the admin's page and returns its id and initial password.
func (f *fixture) create(admin *http.Client, name string) (id, password string) {
	f.t.Helper()
	res := f.do(admin, "POST", "/api/members", map[string]string{"name": name})
	want(f.t, "create "+name, res.StatusCode, http.StatusOK)
	var body struct{ ID, Name, Password string }
	f.decode(res, &body)
	if body.Name != name || body.Password == "" {
		f.t.Fatalf("created %+v", body)
	}
	f.ids[name] = body.ID
	return body.ID, body.Password
}

func TestOnlyTheAdminManagesMembers(t *testing.T) {
	f := newFixture(t)
	member := f.signedIn("friend")
	for _, c := range []struct{ method, path string }{
		{"GET", "/api/members"},
		{"POST", "/api/members"},
		{"POST", "/api/members/" + f.id("me") + "/password"},
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
	// The admin is no member: it cannot be reset or removed, by itself either.
	want(t, "reset the admin", f.status(admin, "POST", "/api/members/"+f.id("me")+"/password", nil), http.StatusNotFound)
	want(t, "remove the admin", f.status(admin, "DELETE", "/api/members/"+f.id("me"), nil), http.StatusNotFound)
}

func TestTheAdminCreatesAndListsMembers(t *testing.T) {
	f := newFixture(t)
	admin := f.signedIn("me")
	want(t, "not a sign-in name", f.status(admin, "POST", "/api/members", map[string]string{"name": "小王"}), http.StatusBadRequest)
	want(t, "the admin's name", f.status(admin, "POST", "/api/members", map[string]string{"name": "me"}), http.StatusConflict)
	res := f.do(admin, "POST", "/api/members", map[string]string{"name": "wang", "display": " 小王 "})
	want(t, "with a display name", res.StatusCode, http.StatusOK)
	var made struct{ ID, Password string }
	f.decode(res, &made)
	f.create(admin, "li")
	f.s.store.putDevice("pc9", &DeviceRecord{Owner: made.ID, Name: "Wang's PC", TokenHash: hashToken("t9")})
	c := f.browser()
	want(t, "wang signs in", f.status(c, "POST", "/api/login", map[string]string{"user": "wang", "password": made.Password}), http.StatusNoContent)

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

// The first browser of a new account is let in by its initial password; until the account has
// chosen its own password (and, the first time, its super password) its only other action
// is registering its key.
func TestAFirstSignInChoosesItsOwnPasswords(t *testing.T) {
	f := newFixture(t)
	admin := f.signedIn("me")
	id, initial := f.create(admin, "wang")
	first := f.browser()
	want(t, "the first browser is let in", f.status(first, "POST", "/api/login", map[string]string{"user": "wang", "password": initial}), http.StatusNoContent)

	res := f.do(first, "GET", "/api/me", nil)
	want(t, "me before choosing", res.StatusCode, http.StatusPreconditionRequired)
	var must struct{ Super bool }
	f.decode(res, &must)
	if !must.Super {
		t.Fatal("the first time the super password is to be chosen too")
	}
	key := proto.BrowserKey{Cred: "Y3JlZDE", Alg: -7, Key: "a2V5MQ==", Name: "iPhone"}
	want(t, "the first browser reports its key before choosing", f.status(first, "POST", "/api/login/device", key), http.StatusOK)
	checkKey := func() {
		t.Helper()
		l, _ := f.s.store.DeviceLog(id)
		if len(l) != 1 || l[0].Op != proto.OpAdd || l[0].Cred != key.Cred || l[0].By != "" {
			t.Fatalf("expected one unsigned add for the first browser: %+v", l)
		}
	}
	checkKey()
	for _, c := range []struct{ method, path string }{
		{"GET", "/api/devices"}, {"POST", "/api/password"}, {"POST", "/api/super-password"},
		{"POST", "/api/approvals"}, {"PATCH", "/api/me"}, {"GET", "/ws/client"},
	} {
		want(t, "before choosing: "+c.method+" "+c.path, f.status(first, c.method, c.path, map[string]string{}), http.StatusPreconditionRequired)
	}
	// A second browser with the same initial password is no first browser.
	want(t, "a second browser waits", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "wang", "password": initial}), http.StatusAccepted)

	choose := func(c *http.Client, body map[string]string) int {
		return f.status(c, "POST", "/api/initial-password", body)
	}
	want(t, "without a super password", choose(first, map[string]string{"new": "my own password"}), http.StatusBadRequest)
	want(t, "too short", choose(first, map[string]string{"new": "short", "super_password": "my super password"}), http.StatusBadRequest)
	want(t, "the initial one again", choose(first, map[string]string{"new": initial, "super_password": "my super password"}), http.StatusConflict)
	want(t, "chosen", choose(first, map[string]string{"new": "my own password", "super_password": "my super password"}), http.StatusOK)
	want(t, "me after choosing", f.status(first, "GET", "/api/me", nil), http.StatusOK)
	want(t, "the signed-in browser reports its key again", f.status(first, "POST", "/api/login/device", key), http.StatusOK)
	checkKey()
	want(t, "choosing again", choose(first, map[string]string{"new": "another password"}), http.StatusConflict)
	if f.s.store.MustChange(id) || !f.s.store.PasswordOK(id, "my own password") || !f.s.store.CheckSuper(id, "my super password") {
		t.Fatal("the choice was not kept")
	}
	want(t, "the initial password is gone", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "wang", "password": initial}), http.StatusUnauthorized)
}

// A reset gives a new initial password; browsers already approved stay so but must choose a
// new password (not a super password: it has one), and a new browser is no first browser.
func TestAResetAsksForANewPasswordOnly(t *testing.T) {
	f := newFixture(t)
	admin := f.signedIn("me")
	member := f.signedIn("friend")
	res := f.do(admin, "POST", "/api/members/"+f.id("friend")+"/password", nil)
	want(t, "reset", res.StatusCode, http.StatusOK)
	var reset struct{ Name, Password string }
	f.decode(res, &reset)
	if reset.Name != "friend" || reset.Password == "" {
		t.Fatalf("%+v", reset)
	}
	res = f.do(member, "GET", "/api/me", nil)
	want(t, "the approved browser stays, choosing", res.StatusCode, http.StatusPreconditionRequired)
	var must struct{ Super bool }
	f.decode(res, &must)
	if must.Super {
		t.Fatal("asked for a super password it has")
	}
	want(t, "the old password is gone", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusUnauthorized)
	want(t, "a new browser waits for approval", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "friend", "password": reset.Password}), http.StatusAccepted)
	want(t, "chosen", f.status(member, "POST", "/api/initial-password", map[string]string{"new": "my new password"}), http.StatusOK)
	want(t, "open again", f.status(member, "GET", "/api/devices", nil), http.StatusOK)

	// A member reset before it ever signed in still lets its first browser in.
	_, _ = f.create(admin, "late")
	res = f.do(admin, "POST", "/api/members/"+f.id("late")+"/password", nil)
	f.decode(res, &reset)
	want(t, "first browser after a reset", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "late", "password": reset.Password}), http.StatusNoContent)
}

// Removing a member takes its browsers (signed out at once), its computers (their tokens stop
// working, their sockets close) and its super password; another member's things stay.
func TestRemovingAMemberTakesEverythingOfIt(t *testing.T) {
	f := signalingFixture(t)
	admin := f.signedIn("me")
	member := f.signedIn("friend")
	f.s.store.putDevice("pc2", &DeviceRecord{Owner: f.id("friend"), Name: "Friend's PC", TokenHash: hashToken("tok-pc2")})
	sock := f.browserSocket(f.srv.URL, member)
	sock.next("devices")
	host := f.hostSocket(f.srv.URL, "pc2")
	id := f.id("friend")

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
	if f.s.store.HasSuper(id) {
		t.Fatal("its super password is kept")
	}
	want(t, "it cannot sign in", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusUnauthorized)
	if _, approved := f.s.store.DeviceOwner("pc1"); !approved {
		t.Fatal("the admin's computer went too")
	}
}
