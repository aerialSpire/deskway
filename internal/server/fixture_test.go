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

const testPassword = "correct horse"

type fixture struct {
	t    *testing.T
	srv  *httptest.Server
	s    *Server
	data string
	ids  map[string]string // account name -> its id
}

// id is the id of the test account that signs in as name.
func (f *fixture) id(name string) string { return f.ids[name] }

// seedAccount gives tests a user with a password and no browser records.
func (f *fixture) seedAccount(name string, admin bool) string {
	f.t.Helper()
	id := randomHex(16)
	f.s.store.mu.Lock()
	f.s.store.Users[id] = &User{Name: name, Display: name, Admin: admin, Created: time.Now().Unix()}
	f.s.store.mu.Unlock()
	if err := f.s.store.SetPassword(id, testPassword); err != nil {
		f.t.Fatal(err)
	}
	f.ids[name] = id
	return id
}

func (f *fixture) addAccount(name string) string {
	return f.seedAccount(name, false)
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	return newFixtureWithServer(t, nil)
}

func newFixtureWithServer(t *testing.T, configure func(*Server)) *fixture {
	t.Helper()
	return newHTTPFixture(t, configure, httptest.NewServer)
}

func newHTTPFixture(t *testing.T, configure func(*Server), listen func(http.Handler) *httptest.Server) *fixture {
	t.Helper()
	f := emptyFixture(t, configure, listen)
	f.seedAccount("me", true)
	return f
}

func emptyFixture(t *testing.T, configure func(*Server), listen func(http.Handler) *httptest.Server) *fixture {
	t.Helper()
	data := filepath.Join(t.TempDir(), "state.json")
	store, err := OpenStore(data)
	if err != nil {
		t.Fatal(err)
	}
	// Cleanups run last-registered first: the server stops (its WebSocket handlers still save
	// state as they end), then the store lets go of the file, then the folder is removed.
	t.Cleanup(func() { store.Close() })
	s := New(store, TURNConfig{}, nil, fstest.MapFS{})
	if configure != nil {
		configure(s)
	}
	srv := listen(s.Handler())
	t.Cleanup(func() {
		srv.Close()
		s.Shutdown()
	})
	return &fixture{t: t, srv: srv, s: s, data: data, ids: map[string]string{}}
}

// browser is an HTTP client with its own cookies, like a separate browser.
func (f *fixture) browser() *http.Client {
	jar, _ := cookiejar.New(nil)
	return &http.Client{Jar: jar, Transport: f.srv.Client().Transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}

// signedIn seeds a signed-in browser. Tests can attach their own key afterwards.
func (f *fixture) signedIn(user string) *http.Client {
	f.t.Helper()
	if f.id(user) == "" {
		f.addAccount(user)
	}
	token := randomHex(32)
	browser := hashToken(token)
	s := f.s.store
	s.mu.Lock()
	s.Browsers[browser] = &BrowserRecord{Owner: f.id(user), Approved: true, Created: time.Now().Unix()}
	err := s.saveLocked()
	s.mu.Unlock()
	if err != nil {
		f.t.Fatal(err)
	}
	req := httptest.NewRequest("GET", f.srv.URL, nil)
	res := httptest.NewRecorder()
	f.s.setBrowserCookie(res, req, token)
	f.s.issueSession(res, req, browser)
	c := f.browser()
	c.Jar.SetCookies(req.URL, res.Result().Cookies())
	return c
}

func setupCode(link string) string {
	return mustURL(link).Fragment[len("c="):]
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
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
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

// login signs in as "me" with the test password.
func (f *fixture) login(c *http.Client) *http.Response {
	f.t.Helper()
	return f.do(c, "POST", "/api/login", map[string]string{"user": "me", "password": testPassword})
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
