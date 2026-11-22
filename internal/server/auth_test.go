package server

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

// Signing in: a password alone gets no session, the super password, the session cookie.

func TestPasswordAloneGetsNoSession(t *testing.T) {
	f := newFixture(t)
	c := f.browser()
	res := f.login(c, nil)
	want(t, "first login", res.StatusCode, http.StatusAccepted)
	var body struct{ Code string }
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil || len(body.Code) != 6 {
		t.Fatalf("no compare code: %v %q", err, body.Code)
	}
	want(t, "me while waiting", f.status(c, "GET", "/api/me", nil), http.StatusUnauthorized)
	want(t, "status while waiting", f.status(c, "GET", "/api/login/status", nil), http.StatusAccepted)
	want(t, "socket while waiting", f.status(c, "GET", "/ws/client", nil), http.StatusUnauthorized)
}

func TestWrongPasswordCreatesNoRequest(t *testing.T) {
	f := newFixture(t)
	c := f.browser()
	want(t, "wrong password", f.status(c, "POST", "/api/login", map[string]string{"user": "me", "password": "nope"}), http.StatusUnauthorized)
	if n := len(f.pending()); n != 0 {
		t.Errorf("%d requests after a wrong password", n)
	}
}

func TestSuperPasswordLetsBrowsersIn(t *testing.T) {
	f := newFixture(t)
	wrong := f.browser()
	want(t, "no super password set", f.login(wrong, map[string]string{"super_password": testSuper}).StatusCode, http.StatusForbidden)
	if err := f.s.store.SetSuper(f.id("me"), testSuper); err != nil {
		t.Fatal(err)
	}
	want(t, "wrong super password", f.login(wrong, map[string]string{"super_password": "not it at all"}).StatusCode, http.StatusForbidden)
	a := f.browser()
	want(t, "right super password", f.login(a, map[string]string{"super_password": testSuper}).StatusCode, http.StatusNoContent)
	want(t, "session works", f.status(a, "GET", "/api/me", nil), http.StatusOK)
	// Not used up: it is for every time all devices are lost.
	b := f.browser()
	want(t, "works again", f.login(b, map[string]string{"super_password": testSuper}).StatusCode, http.StatusNoContent)
}

func TestSuperPasswordNeedsThePassword(t *testing.T) {
	f := newFixture(t)
	_ = f.s.store.SetSuper(f.id("me"), testSuper)
	c := f.browser()
	want(t, "super password without password", f.status(c, "POST", "/api/login", map[string]string{"user": "me", "password": "nope", "super_password": testSuper}), http.StatusUnauthorized)
}

func TestForgedSessionCookieIsRejected(t *testing.T) {
	f := newFixture(t)
	first := f.signedIn("me")
	u := first.Jar.Cookies(mustURL(f.srv.URL))
	var browser string
	for _, c := range u {
		if c.Name == sessionCookie {
			browser = strings.Split(c.Value, ".")[0]
		}
	}
	req, _ := http.NewRequest("GET", f.srv.URL+"/api/me", nil)
	req.AddCookie(&http.Cookie{Name: sessionCookie, Value: browser + ".9999999999.deadbeef"})
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	want(t, "forged cookie", res.StatusCode, http.StatusUnauthorized)
}

// Labels are English: browser and system names are brand names, and the fallbacks match them.
func TestBrowserLabel(t *testing.T) {
	for _, c := range []struct{ ua, want string }{
		{"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 Edg/130.0", "Edge · Windows"},
		{"Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1", "Safari · iPhone"},
		{"curl/8.0", "Browser · Unknown"},
		{"", "Browser · Unknown"},
	} {
		if got := browserLabel(c.ua); got != c.want {
			t.Errorf("browserLabel(%q) = %q, want %q", c.ua, got, c.want)
		}
	}
}
