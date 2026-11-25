package server

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const releaseProgram = "deskway-windows-x64.exe"
const releaseBody = "the release program"

func clientProgramDigest(body string) string {
	return fmt.Sprintf("%x", sha256.Sum256([]byte(body)))
}

func testReleasePrograms(t *testing.T, dir string, serve http.HandlerFunc) *clientPrograms {
	t.Helper()
	assets := httptest.NewServer(serve)
	t.Cleanup(assets.Close)
	stamp := base64.StdEncoding.EncodeToString(fmt.Appendf(nil, "owner/project\nv1\n%s *%s\n", clientProgramDigest(releaseBody), releaseProgram))
	p, err := newClientPrograms(dir, stamp)
	if err != nil {
		t.Fatal(err)
	}
	p.releaseURL = assets.URL + strings.TrimPrefix(p.releaseURL, "https://github.com")
	return p
}

func awaitRelease(t *testing.T, event <-chan struct{}, what string) {
	t.Helper()
	select {
	case <-event:
	case <-time.After(5 * time.Second):
		t.Fatal(what)
	}
}

func waitForProgramFetch(t *testing.T, s *Server) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		s.active.Wait()
		close(done)
	}()
	awaitRelease(t, done, "the program fetcher did not finish")
}

func TestReleaseProgramDistribution(t *testing.T) {
	for _, c := range []struct {
		label    string
		fetch    bool
		cached   string
		manual   string
		requests int32
		body     string
	}{
		{label: "startup fetch", fetch: true, requests: 1, body: releaseBody},
		{label: "operator file wins", fetch: true, cached: releaseBody, manual: "operator program", body: "operator program"},
		{label: "fetching off"},
		{label: "cached with fetching off", cached: releaseBody, body: releaseBody},
		{label: "stale with fetching off", cached: "old program"},
		{label: "stale replaced", fetch: true, cached: "old program", requests: 1, body: releaseBody},
	} {
		t.Run(c.label, func(t *testing.T) {
			dir := t.TempDir()
			var requests atomic.Int32
			p := testReleasePrograms(t, dir, func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				if r.URL.Path != "/owner/project/releases/download/v1/"+releaseProgram {
					t.Errorf("unexpected asset request %s", r.URL.Path)
					http.NotFound(w, r)
					return
				}
				_, _ = io.WriteString(w, releaseBody)
			})
			path := filepath.Join(dir, ".release", releaseProgram)
			if c.cached != "" {
				if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, []byte(c.cached), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			if c.manual != "" {
				if err := os.WriteFile(filepath.Join(dir, releaseProgram), []byte(c.manual), 0o644); err != nil {
					t.Fatal(err)
				}
			}

			f := newFixtureWithServer(t, func(s *Server) {
				s.serveClients(p, c.fetch, clientFetchRetry)
			})
			waitForProgramFetch(t, f.s)
			res := f.do(f.browser(), "GET", "/download/"+releaseProgram, nil)
			if c.body == "" {
				want(t, "download response", res.StatusCode, http.StatusNotFound)
			} else {
				want(t, "download response", res.StatusCode, http.StatusOK)
				got, err := io.ReadAll(res.Body)
				if err != nil || string(got) != c.body || res.Header.Get("ETag") != `"`+clientProgramDigest(c.body)+`"` {
					t.Fatalf("download: body %q, ETag %q, error %v", got, res.Header.Get("ETag"), err)
				}
			}
			want(t, "release requests", int(requests.Load()), int(c.requests))
			if c.requests > 0 {
				if got, err := os.ReadFile(path); err != nil || string(got) != releaseBody {
					t.Fatalf("installed program: %q, %v", got, err)
				}
			}
		})
	}
}

func TestReleaseProgramFailuresAndRetry(t *testing.T) {
	for _, c := range []struct {
		reason releaseFailure
		status int
		body   string
	}{
		{releaseDownloadFailed, http.StatusBadGateway, "upstream error"},
		{releaseChecksumFailed, http.StatusOK, "wrong binary"},
		{releaseStorageFailed, http.StatusOK, releaseBody},
	} {
		t.Run(string(c.reason), func(t *testing.T) {
			dir := t.TempDir()
			var attempts atomic.Int32
			retrying, resume := make(chan struct{}), make(chan struct{})
			p := testReleasePrograms(t, dir, func(w http.ResponseWriter, r *http.Request) {
				switch attempts.Add(1) {
				case 1:
					w.WriteHeader(c.status)
					_, _ = io.WriteString(w, c.body)
				case 2:
					close(retrying)
					select {
					case <-r.Context().Done():
					case <-resume:
						_, _ = io.WriteString(w, releaseBody)
					}
				default:
					t.Error("unexpected third asset request")
				}
			})
			path := p.releasePath(releaseProgram)
			if c.reason == releaseStorageFailed {
				// A directory at the destination makes the atomic installation fail.
				if err := os.MkdirAll(path, 0o755); err != nil {
					t.Fatal(err)
				}
			}
			f := newFixtureWithServer(t, func(s *Server) {
				s.serveClients(p, true, time.Millisecond)
			})
			awaitRelease(t, retrying, "the server did not retry the failed download")
			me := f.signedIn("me")
			me.Timeout = 5 * time.Second
			res := f.do(me, "GET", "/api/ai/clients?details=1", nil)
			var details struct {
				Files  []ClientFile
				Errors map[string]releaseFailure
			}
			if err := json.NewDecoder(res.Body).Decode(&details); err != nil || details.Files == nil || len(details.Files) != 0 ||
				len(details.Errors) != 1 || details.Errors[releaseProgram] != c.reason {
				t.Fatalf("program details: %+v, %v", details, err)
			}
			want(t, "the failed program is unavailable", f.status(me, "GET", "/download/"+releaseProgram, nil), http.StatusNotFound)
			if c.reason == releaseStorageFailed {
				if err := os.Rename(path, filepath.Join(dir, "blocked")); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := os.Stat(path); !os.IsNotExist(err) {
				t.Fatalf("failed program was published: %v", err)
			}
			close(resume)
			waitForProgramFetch(t, f.s)
			if got, err := os.ReadFile(path); err != nil || string(got) != releaseBody {
				t.Fatalf("installed program: %q, %v", got, err)
			}
		})
	}
}

func TestShutdownCancelsReleaseProgramDownload(t *testing.T) {
	dir := t.TempDir()
	canceled := make(chan struct{})
	p := testReleasePrograms(t, dir, func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "partial download")
		w.(http.Flusher).Flush()
		<-r.Context().Done()
		close(canceled)
	})
	path := p.releasePath(releaseProgram)
	cache := filepath.Dir(path)
	// The fixture first shuts down the server, then this check runs.
	t.Cleanup(func() {
		awaitRelease(t, canceled, "shutdown did not cancel the asset request")
		if entries, err := os.ReadDir(cache); err != nil || len(entries) != 0 {
			t.Errorf("cache after shutdown: %v, %v", entries, err)
		}
	})
	f := newFixtureWithServer(t, func(s *Server) {
		s.serveClients(p, true, clientFetchRetry)
	})
	deadline := time.Now().Add(5 * time.Second)
	for {
		partial, err := filepath.Glob(filepath.Join(cache, ".download-*"))
		if err != nil {
			t.Fatal(err)
		}
		if len(partial) == 1 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the download never created a temporary file")
		}
		time.Sleep(time.Millisecond)
	}
	me := f.signedIn("me")
	me.Timeout = 5 * time.Second
	var list []ClientFile
	res := f.do(me, "GET", "/api/ai/clients", nil)
	if err := json.NewDecoder(res.Body).Decode(&list); err != nil || list == nil || len(list) != 0 {
		t.Fatalf("list during a stalled download: %+v, %v", list, err)
	}
}

// The AI client programs: listed with their SHA-256 for the account, handed out to anyone by
// name, nothing else from the folder.
func TestTheServerHandsOutTheAIClientPrograms(t *testing.T) {
	dir := t.TempDir()
	files := map[string]string{"deskway-macos-arm64": "mac program", "deskway-linux-x64": "not one of them", "deskway-windows-x64.exe": "windows program", "notes.txt": "not a program"}
	for name, body := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	f := newFixtureWithServer(t, func(s *Server) {
		s.ServeClients(dir, false)
	})
	me := f.signedIn("me")
	want(t, "the list needs a signed-in browser", f.status(f.browser(), "GET", "/api/ai/clients", nil), http.StatusUnauthorized)
	var list []ClientFile
	_ = json.NewDecoder(f.do(me, "GET", "/api/ai/clients", nil).Body).Decode(&list)
	if len(list) != 2 || list[0].File != "deskway-windows-x64.exe" || list[0].SHA256 != clientProgramDigest("windows program") || list[1].File != "deskway-macos-arm64" || list[1].OS != "macos" || list[1].SHA256 != clientProgramDigest("mac program") {
		t.Fatalf("listed %+v", list)
	}
	for _, c := range []struct {
		file   string
		status int
	}{
		{"deskway-macos-arm64", http.StatusOK},
		{"deskway-macos-x64", http.StatusNotFound},
		{"deskway-linux-x64", http.StatusNotFound},
		{"notes.txt", http.StatusNotFound},
	} {
		want(t, "download "+c.file, f.status(f.browser(), "GET", "/download/"+c.file, nil), c.status)
	}
}
