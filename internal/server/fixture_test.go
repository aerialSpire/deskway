package server

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"testing"
	"testing/fstest"
	"time"

	"deskway/internal/proto"
)

// The fixture every server test runs on: a server on a fresh state file, the admin "me" (with
// a password of its own, the test password), and browsers as HTTP clients with their own cookies.

const (
	testPassword = "correct horse"
	testSuper    = "super secret 1"
)

type fixture struct {
	t    *testing.T
	srv  *httptest.Server
	s    *Server
	data string
	ids  map[string]string // account name -> its id
}

// id is the id of the test account that signs in as name.
func (f *fixture) id(name string) string { return f.ids[name] }

// addAccount creates a member signing in as name that has chosen the test password.
func (f *fixture) addAccount(name string) string {
	f.t.Helper()
	id, _, err := f.s.store.CreateMember(name, name)
	if err != nil {
		f.t.Fatal(err)
	}
	f.ids[name] = id
	f.choose(id)
	return id
}

// choose gives the account the test password, as if it had chosen it.
func (f *fixture) choose(id string) {
	f.t.Helper()
	if err := f.s.store.SetPassword(id, testPassword); err != nil {
		f.t.Fatal(err)
	}
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	data := filepath.Join(t.TempDir(), "state.json")
	store, err := OpenStore(data)
	if err != nil {
		t.Fatal(err)
	}
	// Cleanups run last-registered first: the server stops (its WebSocket handlers still save
	// state as they end), then the store lets go of the file, then the folder is removed.
	t.Cleanup(func() { store.Close() })
	s := New(store, func(string) []proto.ICEServer { return nil }, fstest.MapFS{})
	srv := httptest.NewServer(s.Handler())
	t.Cleanup(func() {
		srv.Close()
		s.Shutdown()
	})
	f := &fixture{t: t, srv: srv, s: s, data: data, ids: map[string]string{}}
	if _, err := store.CreateAdmin("me"); err != nil {
		t.Fatal(err)
	}
	f.ids["me"] = store.ID("me")
	f.choose(f.id("me"))
	return f
}

// browser is an HTTP client with its own cookies, like a separate browser.
func (f *fixture) browser() *http.Client {
	jar, _ := cookiejar.New(nil)
	return &http.Client{Jar: jar, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}

// signedIn is a browser of `user` (created if needed), let in with that account's super
// password.
func (f *fixture) signedIn(user string) *http.Client {
	f.t.Helper()
	if f.id(user) == "" {
		f.addAccount(user)
	}
	if err := f.s.store.SetSuper(f.id(user), testSuper); err != nil {
		f.t.Fatal(err)
	}
	c := f.browser()
	if got := f.do(c, "POST", "/api/login", map[string]string{"user": user, "password": testPassword, "super_password": testSuper}).StatusCode; got != http.StatusNoContent {
		f.t.Fatalf("login %s: %d", user, got)
	}
	return c
}

func (f *fixture) do(c *http.Client, method, path string, body any) *http.Response {
	f.t.Helper()
	var r *bytes.Reader = bytes.NewReader(nil)
	if body != nil {
		b, _ := json.Marshal(body)
		r = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, f.srv.URL+path, r)
	if err != nil {
		f.t.Fatal(err)
	}
	res, err := c.Do(req)
	if err != nil {
		f.t.Fatal(err)
	}
	f.t.Cleanup(func() { res.Body.Close() })
	return res
}

func (f *fixture) status(c *http.Client, method, path string, body any) int {
	f.t.Helper()
	return f.do(c, method, path, body).StatusCode
}

// login signs in as "me" with the test password and any extra fields.
func (f *fixture) login(c *http.Client, extra map[string]string) *http.Response {
	f.t.Helper()
	body := map[string]string{"user": "me", "password": testPassword}
	for k, v := range extra {
		body[k] = v
	}
	return f.do(c, "POST", "/api/login", body)
}

// pending is what waits for the approval of "me".
func (f *fixture) pending() []proto.Approval {
	return f.s.store.Approvals(f.id("me"), pendingBrowserTTL)
}

func want(t *testing.T, name string, got, expected int) {
	t.Helper()
	if got != expected {
		t.Errorf("%s: got %d, want %d", name, got, expected)
	}
}

func mustURL(raw string) *url.URL {
	u, err := url.Parse(raw)
	if err != nil {
		panic(err)
	}
	return u
}

// Test helpers that change records directly, as only a test may.

func (s *Store) ageBrowser(id string, by time.Duration) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Browsers[id].Created = time.Now().Add(-by).Unix()
}

func (s *Store) putDevice(id string, d *DeviceRecord) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Devices[id] = d
}
