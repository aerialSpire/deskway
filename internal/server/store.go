package server

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"os"
	"path/filepath"
	"sync"
	"time"

	"golang.org/x/crypto/bcrypt"

	"deskway/internal/proto"
)

// User is one account, kept under its id: everything that belongs to it (computers, browsers,
// the super password, sessions) names the id, never the name it signs in with, so that name
// can change. Nothing is shared between accounts (docs/accounts.md).
type User struct {
	// Name is what the account signs in with; unique, and the account's own to change.
	Name    string `json:"name"`
	Hash    string `json:"hash"`
	Display string `json:"display"`
	Created int64  `json:"created"`
	// Admin: the server's first account, which makes, resets and removes the others.
	Admin bool `json:"admin,omitempty"`
	// MustChange: the password is an initial one, which the admin was shown; the account
	// chooses its own before anything else.
	MustChange bool `json:"must_change,omitempty"`
	// HadBrowser: a browser of the account has been approved at some time. Until then, while
	// the password is an initial one, signing in with it approves the browser at once.
	HadBrowser bool `json:"had_browser,omitempty"`
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

// Store holds persistent state in one JSON file, written atomically. The process that opened it
// holds the file's lock until Close (statelock.go).
type Store struct {
	mu   sync.Mutex
	path string
	lock *os.File

	CookieKey  string                    `json:"cookie_key"`
	TURNSecret string                    `json:"turn_secret"`
	Users      map[string]*User          `json:"users"` // by account id
	Devices    map[string]*DeviceRecord  `json:"devices"`
	Browsers   map[string]*BrowserRecord `json:"browsers"`
}

// OpenStore takes the state file's lock, then loads path or creates it with fresh secrets. It
// fails with ErrLocked while another process has it open.
func OpenStore(path string) (*Store, error) {
	lock, err := lockState(path)
	if err != nil {
		return nil, err
	}
	s := &Store{path: path, lock: lock, Users: map[string]*User{}, Devices: map[string]*DeviceRecord{}, Browsers: map[string]*BrowserRecord{}}
	raw, err := os.ReadFile(path)
	switch {
	case err == nil:
		err = json.Unmarshal(raw, s)
		if err != nil {
			err = fmt.Errorf("parse %s: %w", path, err)
		}
	case errors.Is(err, os.ErrNotExist):
		err = nil
	}
	if err != nil {
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
	if err := s.saveLocked(); err != nil {
		lock.Close()
		return nil, err
	}
	return s, nil
}

// Close releases the state file for another process. The store must not change after it.
func (s *Store) Close() error {
	return s.lock.Close()
}

/// The store's state is reached only through the methods below and in records.go: s.mu is
// never locked from outside, so every read and change is one consistent step.

// writeFile replaces path with b: written next to it, then renamed over it.
func writeFile(path string, b []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func (s *Store) saveLocked() error {
	b, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	return writeFile(s.path, b)
}

/// ---- the super password ----

// The super password lets a browser in without an approved browser around, after every
// signed-in device is lost. Unlike approval it needs nothing but the account password and this
// password, so it should be long and kept offline. The account chooses it at its first sign-in
// and changes it on its account page.
//
// Hashes live in their own file beside the state file, read each time one is checked.
func superPath(statePath string) string { return statePath + ".super" }

var superMu sync.Mutex

// readSuper reads the super password file; a missing one is empty. On an error the map is
// empty too, so a check fails closed, but a change must not write it back over the file.
func readSuper(path string) (map[string]string, error) {
	hashes := map[string]string{}
	raw, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return hashes, nil
	}
	if err != nil {
		return hashes, err
	}
	if err := json.Unmarshal(raw, &hashes); err != nil {
		return map[string]string{}, err
	}
	return hashes, nil
}

// updateSuper changes the super password file of the state file at statePath.
func updateSuper(statePath string, change func(hashes map[string]string)) error {
	superMu.Lock()
	defer superMu.Unlock()
	path := superPath(statePath)
	hashes, err := readSuper(path)
	if err != nil {
		return err
	}
	change(hashes)
	b, err := json.Marshal(hashes)
	if err != nil {
		return err
	}
	return writeFile(path, b)
}

// SetSuper sets or replaces an account's super password, by the account's id. Only a bcrypt
// hash is kept.
func (s *Store) SetSuper(user, password string) error {
	if !validSecret(password) {
		return errBadSecret
	}
	h, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}
	return updateSuper(s.path, func(hashes map[string]string) { hashes[user] = string(h) })
}

// HasSuper says whether the account has a super password yet.
func (s *Store) HasSuper(user string) bool {
	superMu.Lock()
	defer superMu.Unlock()
	hashes, _ := readSuper(superPath(s.path))
	_, ok := hashes[user]
	return ok
}

// CheckSuper verifies an account's super password, spending the same time when it has none.
func (s *Store) CheckSuper(user, password string) bool {
	superMu.Lock()
	hashes, _ := readSuper(superPath(s.path))
	superMu.Unlock()
	h, ok := hashes[user]
	if !ok {
		h = noHash
	}
	return bcrypt.CompareHashAndPassword([]byte(h), []byte(password)) == nil && ok
}

// ---- accounts and passwords ----

// Accounts are made only with a generated initial password (docs/accounts.md): the admin by
// the admin command, members by the admin.

var errHasAccounts = errors.New("this server has accounts already; its admin makes the others")

// CreateAdmin makes the server's first account, the admin, and returns its initial password.
// It refuses once any account exists.
func (s *Store) CreateAdmin(name string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.Users) > 0 {
		return "", errHasAccounts
	}
	_, password, err := s.addUserLocked(name, name, true)
	return password, err
}

// CreateMember makes a member account and returns its id and initial password.
func (s *Store) CreateMember(name, display string) (id, password string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.addUserLocked(name, display, false)
}

func (s *Store) addUserLocked(name, display string, admin bool) (id, password string, err error) {
	if !ValidAccountName(name) {
		return "", "", errBadName
	}
	if idOf(s.Users, name) != "" {
		return "", "", errNameTaken
	}
	password = initialPassword()
	h, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return "", "", err
	}
	id = randomHex(16)
	s.Users[id] = &User{Name: name, Display: display, Hash: string(h), Created: time.Now().Unix(), Admin: admin, MustChange: true}
	return id, password, s.saveLocked()
}

// ResetPassword gives a member a new initial password and returns it. Its approved browsers
// stay approved.
func (s *Store) ResetPassword(member string) (string, error) {
	password := initialPassword()
	h, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		return "", err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[member]
	if u == nil || u.Admin {
		return "", errNoUser
	}
	u.Hash, u.MustChange = string(h), true
	return password, s.saveLocked()
}

// SetPassword sets a password the account chose; an initial one is gone with it.
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
	u.Hash, u.MustChange = string(h), false
	return s.saveLocked()
}

// RemoveMember deletes a member account, by its id, with its browsers, computers and super
// password. It returns the browsers and computers that went, for their connections to end.
func (s *Store) RemoveMember(member string) (browsers, computers map[string]bool, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if u := s.Users[member]; u == nil || u.Admin {
		return nil, nil, errNoUser
	}
	// The super password goes first: if its file cannot be changed, nothing is removed yet.
	if err := updateSuper(s.path, func(hashes map[string]string) { delete(hashes, member) }); err != nil {
		return nil, nil, err
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
	return browsers, computers, s.saveLocked()
}

// initialAlphabet leaves out what reads alike (0 o, 1 l i) and has no capitals, so an initial
// password is easy to read aloud and to type on a phone.
const initialAlphabet = "23456789abcdefghjkmnpqrstuvwxyz"

// initialPassword is four groups of four from initialAlphabet, joined by '-' (about 79 bits).
func initialPassword() string {
	b := make([]byte, 0, 19)
	for i := range 16 {
		if i > 0 && i%4 == 0 {
			b = append(b, '-')
		}
		n, err := rand.Int(rand.Reader, big.NewInt(int64(len(initialAlphabet))))
		if err != nil {
			panic(err)
		}
		b = append(b, initialAlphabet[n.Int64()])
	}
	return string(b)
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
	if u := s.Users[user]; u != nil {
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
