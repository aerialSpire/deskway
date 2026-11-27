package server

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"golang.org/x/crypto/bcrypt"

	"deskway/internal/proto"
)

// User is one account, kept under its id: everything that belongs to it (computers, browsers,
// account links, sessions) names the id, never the name it signs in with, so that name
// can change. Nothing is shared between accounts (docs/accounts.md).
type User struct {
	// Name is what the account signs in with; unique, and the account's own to change.
	Name    string `json:"name"`
	Hash    string `json:"hash"`
	Display string `json:"display"`
	Created int64  `json:"created"`
	// Admin: the server's first account, which makes, resets and removes the others.
	Admin bool `json:"admin,omitempty"`
	// DeviceLog is the signed history of the account's browser keys, replayed by its computers.
	DeviceLog []proto.LogEntry `json:"device_log,omitempty"`
}

// DeviceRecord is one enrolled computer. Only a hash of its token is stored.
type DeviceRecord struct {
	Owner     string `json:"owner"`
	Name      string `json:"name"`
	OS        string `json:"os"`
	TokenHash string `json:"token_hash"`
	LastSeen  int64  `json:"last_seen,omitempty"`
	Pending   bool   `json:"pending,omitempty"` // enrolled, not yet let in by an approved browser
	EnrollIP  string `json:"enroll_ip,omitempty"`
	// ShowsCode: it said at enrollment that it shows its six digits while waiting
	// (proto.ComputerCode); an older one does not, and its approval shows none.
	ShowsCode bool `json:"shows_code,omitempty"`
	// AI is the latest signed AI switch for this computer: kept apart from the account's
	// log, whose oldest entries go once it is full, and passed on with it.
	AI *proto.AiSwitch `json:"ai,omitempty"`
	// Tools are the AI tools it carried out when it last connected: whether it takes AI
	// control shows while it is offline too.
	Tools []string `json:"tools,omitempty"`
	// Version is the last protocol version the computer reported; 0 means unknown.
	Version int `json:"version,omitempty"`
}

// BrowserRecord is one browser an account signs in from. Its key is the hash of the
// long-lived cookie that only that browser holds.
type BrowserRecord struct {
	Owner    string `json:"owner"`
	Label    string `json:"label"`
	IP       string `json:"ip"`
	Code     string `json:"code,omitempty"`
	Approved bool   `json:"approved,omitempty"`
	Pickup   bool   `json:"pickup,omitempty"` // approved, may collect one session without a new password check
	Created  int64  `json:"created"`
	LastSeen int64  `json:"last_seen,omitempty"`
	// Key is the key this browser signs with, once it has told the server.
	Key *proto.BrowserKey `json:"device,omitempty"`
	// Role is proto.RoleAI for an AI client's deskway command, which signs in like a browser
	// and is approved like one, but reaches only what an AI client needs (auth.go).
	Role string `json:"role,omitempty"`
}

type RelayRecord struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	PublicIP string `json:"public_ip"`
	Port     int    `json:"port"`
	Secret   string `json:"secret"`
}

// Store holds persistent state in one JSON file, written atomically. The process that opened it
// holds the file's lock until Close (statelock.go).
type Store struct {
	mu         sync.Mutex
	path       string
	lock       *os.File
	mcpPending map[string]*mcpPendingGrant

	CookieKey  string                     `json:"cookie_key"`
	TURNSecret string                     `json:"turn_secret"`
	Users      map[string]*User           `json:"users"` // by account id
	Devices    map[string]*DeviceRecord   `json:"devices"`
	Browsers   map[string]*BrowserRecord  `json:"browsers"`
	Relays     []RelayRecord              `json:"relays,omitempty"` // in join order
	MCPClients map[string]mcpRegistration `json:"mcp_clients,omitempty"`
	MCPGrants  map[string]*mcpGrant       `json:"mcp_grants,omitempty"`
}

// OpenStore takes the state file's lock, then loads path or creates it with fresh secrets. It
// fails with ErrLocked while another process has it open.
func OpenStore(path string) (*Store, error) {
	lock, err := lockState(path)
	if err != nil {
		return nil, err
	}
	s := &Store{path: path, lock: lock, Users: map[string]*User{}, Devices: map[string]*DeviceRecord{}, Browsers: map[string]*BrowserRecord{}}
	if err := s.load(); err != nil {
		lock.Close()
		return nil, err
	}
	if s.Browsers == nil {
		s.Browsers = map[string]*BrowserRecord{} // state written before browser approval existed
	}
	if s.CookieKey == "" {
		s.CookieKey = randomHex(32)
	}
	if s.TURNSecret == "" {
		s.TURNSecret = randomHex(32)
	}
	// Remove the retired super password file so its hashes do not remain on disk.
	if err := os.Remove(path + ".super"); err != nil && !errors.Is(err, os.ErrNotExist) {
		s.Close()
		return nil, err
	}
	if err := s.saveLocked(); err != nil {
		s.Close()
		return nil, err
	}
	return s, nil
}

func (s *Store) load() error {
	state := struct {
		*Store
		Users map[string]struct {
			User
			Unused bool `json:"must_change"`
		} `json:"users"`
	}{Store: s}
	if err := readState(s.path, &state); errors.Is(err, os.ErrNotExist) {
		return nil
	} else if err != nil {
		return err
	}
	// Discard unused initial passwords and the browsers they let in before setup finished.
	for id, u := range state.Users {
		if u.Unused {
			u.Hash = ""
		}
		s.Users[id] = &u.User
	}
	for id, b := range s.Browsers {
		if b.Role == "" && state.Users[b.Owner].Unused {
			delete(s.Browsers, id)
		}
	}
	return nil
}

func readState(path string, state any) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	if err := json.Unmarshal(raw, state); err != nil {
		return fmt.Errorf("parse %s: %w", path, err)
	}
	return nil
}

// Close releases the state file for another process. The store must not change after it.
func (s *Store) Close() error {
	return s.lock.Close()
}

/// The store's state is reached only through the methods below and in records.go: s.mu is
// never locked from outside, so every read and change is one consistent step.

// writeFile replaces path with b: written next to it, then renamed over it.
func writeFile(path string, b []byte) error {
	return writeFileWithRename(path, b, os.Rename)
}

func writeFileWithRename(path string, b []byte, rename func(string, string) error) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return err
	}
	return rename(tmp, path)
}

func (s *Store) saveLocked() error {
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	return writeFileWithRename(s.path, b, replaceStateFile)
}

// SetPassword sets a password the account chose.
func (s *Store) SetPassword(user, password string) error {
	if !validSecret(password) {
		return errBadSecret
	}
	h, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	if u == nil {
		return errNoUser
	}
	u.Hash = string(h)
	return s.saveLocked()
}

// RemoveMember deletes a member and its account link, browsers and computers. It returns the
// browsers and computers that went, for their connections to end.
func (s *Store) RemoveMember(member string) (browsers, computers map[string]bool, profiles []string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if u := s.Users[member]; u == nil || u.Admin {
		return nil, nil, nil, errNoUser
	}
	if err := s.revokeMemberLinkLocked(member); err != nil {
		return nil, nil, nil, err
	}
	delete(s.Users, member)
	browsers, computers = map[string]bool{}, map[string]bool{}
	for id, b := range s.Browsers {
		if b.Owner == member {
			delete(s.Browsers, id)
			browsers[id] = true
		}
	}
	for id, d := range s.Devices {
		if d.Owner == member {
			delete(s.Devices, id)
			computers[id] = true
		}
	}
	profiles = s.mcpRemoveLocked(browsers)
	return browsers, computers, profiles, s.saveLocked()
}

// A hash for no account, so a check against an unknown account takes as long as a real one.
const noHash = "$2a$10$7EqJtq98hPqEX7fNZaFWoOa2h3h0w6j9s3gkp1tq9Mz0m7v1Q0Y1a"

// SignIn checks a sign-in name and password and returns the account's id, or "" (spending the
// same time for unknown names).
func (s *Store) SignIn(name, password string) string {
	s.mu.Lock()
	id := idOf(s.Users, name)
	s.mu.Unlock()
	if !s.PasswordOK(id, password) {
		return ""
	}
	return id
}

// PasswordOK checks an account's password, by its id.
func (s *Store) PasswordOK(user, password string) bool {
	s.mu.Lock()
	hash, ok := noHash, false
	if u := s.Users[user]; u != nil && u.Hash != "" {
		hash, ok = u.Hash, true
	}
	s.mu.Unlock()
	return bcrypt.CompareHashAndPassword([]byte(hash), []byte(password)) == nil && ok
}

func randomHex(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	return hex.EncodeToString(b)
}

func hashToken(t string) string {
	h := sha256.Sum256([]byte(t))
	return hex.EncodeToString(h[:])
}
