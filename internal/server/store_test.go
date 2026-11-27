package server

import (
	"encoding/json"
	"errors"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"deskway/internal/proto"
)

// While a store is open, a second one cannot open the file: its saves would overwrite each
// other.
func TestTheStateFileHasOneOwner(t *testing.T) {
	f := newFixture(t)
	if _, err := OpenStore(f.data); !errors.Is(err, ErrLocked) {
		t.Fatalf("second open: %v", err)
	}
	if err := f.s.store.Close(); err != nil {
		t.Fatal(err)
	}
	s, err := OpenStore(f.data)
	if err != nil {
		t.Fatalf("open after close: %v", err)
	}
	s.Close()
}

func TestStateReplacementWhileSnapshotIsOpen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	s, err := OpenStore(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()

	snapshot, err := openStateSnapshot(path)
	if err != nil {
		t.Fatal(err)
	}

	s.Users["operator"] = &User{Name: "me", Admin: true}
	var previous, current Store
	snapshotDone := make(chan error)
	time.AfterFunc(50*time.Millisecond, func() {
		err := json.NewDecoder(snapshot).Decode(&previous)
		snapshotDone <- errors.Join(err, snapshot.Close())
	})
	saveErr := s.saveLocked()
	if err := <-snapshotDone; err != nil {
		t.Fatalf("read and close snapshot: %v", err)
	}
	if saveErr != nil {
		t.Fatal(saveErr)
	}
	if err := readState(path, &current); err != nil {
		t.Fatal(err)
	}
	if previous.HasAdmin() || !current.HasAdmin() {
		t.Fatal("expected the open snapshot to keep its original state and a new read to see the replacement")
	}
}

func TestLoadClearsRetiredCredentials(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	const hash = noHash
	state := `{"users":{"admin":{"name":"me","admin":true,"hash":"` + hash + `"},
		"member":{"name":"friend","hash":"` + hash + `","must_change":true,"had_browser":true}},
		"browsers":{"admin-browser":{"owner":"admin","approved":true},
		"member-browser":{"owner":"member","approved":true},"waiting":{"owner":"member"},
		"member-ai":{"owner":"member","approved":true,"role":"` + proto.RoleAI + `"}}}`
	if err := os.WriteFile(path, []byte(state), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path+".super", []byte("old hash file"), 0o600); err != nil {
		t.Fatal(err)
	}
	s, err := OpenStore(path)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	list, err := s.Members()
	if err != nil || len(list) != 1 || !list[0].NeedsSetup || list[0].LinkExpiry != 0 {
		t.Fatalf("members after load: %+v, %v", list, err)
	}
	if s.Users["member"].Hash != "" || s.Users["admin"].Hash != hash {
		t.Fatal("load should clear only the unused password")
	}
	server := &Server{store: s}
	for browser, owner := range map[string]string{"admin-browser": "admin", "member-browser": "", "member-ai": "member"} {
		req := httptest.NewRequest("GET", "/api/me", nil)
		res := httptest.NewRecorder()
		server.issueSession(res, req, browser)
		req.AddCookie(res.Result().Cookies()[0])
		if got, _ := server.sessionOf(req); got != owner {
			t.Errorf("session %s: owner %q, want %q", browser, got, owner)
		}
	}
	if len(s.Browsers) != 2 || s.BrowserRole("member-ai") != proto.RoleAI {
		t.Fatal("load should clear the unfinished account's browsers and preserve its AI client")
	}
	if _, err := os.Stat(path + ".super"); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("retired recovery file: %v", err)
	}
	data, err := os.ReadFile(path)
	if err != nil || strings.Contains(string(data), "must_change") || strings.Contains(string(data), "had_browser") {
		t.Fatalf("retired account fields were kept: %v", err)
	}
}
