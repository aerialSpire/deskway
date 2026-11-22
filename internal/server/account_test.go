package server

import (
	"encoding/json"
	"net/http"
	"testing"
)

// The account page: password, super password and the name the account signs in with.

func TestChangePasswordNeedsTheCurrentOne(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	want(t, "wrong current", f.status(a, "POST", "/api/password", map[string]any{"current": "nope", "new": "brand new password"}), http.StatusForbidden)
	want(t, "too short", f.status(a, "POST", "/api/password", map[string]any{"current": testPassword, "new": "short"}), http.StatusBadRequest)
	want(t, "changed", f.status(a, "POST", "/api/password", map[string]any{"current": testPassword, "new": "brand new password"}), http.StatusNoContent)
	if f.s.store.PasswordOK(f.id("me"), testPassword) || !f.s.store.PasswordOK(f.id("me"), "brand new password") {
		t.Fatal("password not changed")
	}
	want(t, "still signed in", f.status(a, "GET", "/api/me", nil), http.StatusOK)
}

func TestChangePasswordCanSignOutOthers(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	b := f.browser()
	want(t, "second in", f.login(b, map[string]string{"super_password": testSuper}).StatusCode, http.StatusNoContent)
	want(t, "changed", f.status(a, "POST", "/api/password", map[string]any{"current": testPassword, "new": "brand new password", "sign_out_others": true}), http.StatusNoContent)
	want(t, "this browser stays", f.status(a, "GET", "/api/me", nil), http.StatusOK)
	want(t, "the other is out", f.status(b, "GET", "/api/me", nil), http.StatusUnauthorized)
}

func TestChangeSuperPassword(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	want(t, "wrong account password", f.status(a, "POST", "/api/super-password", map[string]any{"password": "nope", "new": "another super one"}), http.StatusForbidden)
	want(t, "changed", f.status(a, "POST", "/api/super-password", map[string]any{"password": testPassword, "new": "another super one"}), http.StatusNoContent)
	if f.s.store.CheckSuper(f.id("me"), testSuper) || !f.s.store.CheckSuper(f.id("me"), "another super one") {
		t.Fatal("super password not changed")
	}
}

func TestTheAccountNameChangesAndNothingElse(t *testing.T) {
	f := newFixture(t)
	a := f.signedIn("me")
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), Name: "PC", TokenHash: hashToken("tok")})
	f.addAccount("friend")
	rename := func(name, password string) int {
		return f.status(a, "POST", "/api/me/name", map[string]string{"name": name, "password": password})
	}
	want(t, "wrong password", rename("xiaolin", "nope"), http.StatusForbidden)
	want(t, "not a name", rename("小林", testPassword), http.StatusBadRequest)
	want(t, "taken", rename("friend", testPassword), http.StatusConflict)
	want(t, "renamed", rename("xiaolin", testPassword), http.StatusOK)
	var me map[string]any
	if err := json.NewDecoder(f.do(a, "GET", "/api/me", nil).Body).Decode(&me); err != nil || me["user"] != "xiaolin" || me["display"] != "xiaolin" {
		t.Fatalf("me after renaming: %v %v", me, err)
	}
	want(t, "the old name no longer signs in", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "me", "password": testPassword}), http.StatusUnauthorized)
	want(t, "the new name does", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "xiaolin", "password": testPassword}), http.StatusAccepted)
	if n := len(f.s.deviceList(&conn{user: f.id("me")})); n != 1 {
		t.Fatalf("the account's computers stay with it: %d", n)
	}
	if !f.s.store.CheckSuper(f.id("me"), testSuper) {
		t.Fatal("the super password stays with the account")
	}
	want(t, "still signed in", f.status(a, "GET", "/api/me", nil), http.StatusOK)
	want(t, "the same name again is fine", rename("xiaolin", testPassword), http.StatusOK)
}
