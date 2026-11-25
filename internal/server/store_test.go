package server

import (
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"testing"
)

// The state file, the super password file and the accounts in them, outside any server.

func TestShortSuperPasswordIsRefused(t *testing.T) {
	f := newFixture(t)
	if f.s.store.SetSuper(f.id("me"), "short") == nil {
		t.Fatal("a 5-character super password was accepted")
	}
}

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

func TestFailedAdminCreationLeavesNoStateFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	if _, err := CreateAdmin(path, "x"); !errors.Is(err, errBadName) {
		t.Fatalf("invalid admin name: %v", err)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("state file after failed admin creation: %v", err)
	}
}

// The admin is the first account and only the first; it starts with an initial password.
func TestTheAdminIsTheFirstAccount(t *testing.T) {
	f := newFixture(t)
	if _, err := f.s.store.CreateAdmin("other"); !errors.Is(err, errHasAccounts) {
		t.Fatalf("a second admin: %v", err)
	}
	if !f.s.store.IsAdmin(f.id("me")) {
		t.Fatal("the first account is not the admin")
	}
	id, password, err := f.s.store.CreateMember("friend", "Friend")
	if err != nil {
		t.Fatal(err)
	}
	if f.s.store.IsAdmin(id) || !f.s.store.MustChange(id) || f.s.store.SignIn("friend", password) != id {
		t.Fatal("a member starts as a member, with its initial password")
	}
}

// Initial passwords: four groups of four, nothing that reads alike, never the same twice.
func TestInitialPasswordsAreEasyToReadAndUnpredictable(t *testing.T) {
	shape := regexp.MustCompile(`^[2-9a-hjkmnp-z]{4}(-[2-9a-hjkmnp-z]{4}){3}$`)
	seen := map[string]bool{}
	for range 200 {
		p := initialPassword()
		if !shape.MatchString(p) || seen[p] || !validSecret(p) {
			t.Fatalf("initial password %q", p)
		}
		seen[p] = true
	}
}
