package server

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"time"

	"golang.org/x/crypto/bcrypt"

	"deskway/internal/proto"
)

const (
	linkSetup     = "setup"
	linkReset     = "reset"
	linkLogin     = "login"
	memberLinkTTL = 7 * 24 * time.Hour
	adminLinkTTL  = time.Hour
	loginLinkTTL  = 2 * time.Minute
	adminLinkID   = "admin"
	localIssuer   = "admin-link"
)

var (
	errAccountLink = errors.New("invalid or expired account link")
	errSetupKey    = errors.New("a valid browser key is required")
	errLoginFields = errors.New("a login link accepts only a browser key")
)

type accountLink struct {
	Hash    string `json:"hash,omitempty"`
	Kind    string `json:"kind,omitempty"`
	Issuer  string `json:"issued_by,omitempty"`
	Expires int64  `json:"expires"`
}

// linksFile is held for an operation, never for the lifetime of the server. Server operations
// take the Store mutex first. The CLI takes only this lock.
type linksFile struct {
	path string
	*os.File
	records map[string]accountLink // by member account id, or adminLinkID
}

func openLinks(statePath string) (*linksFile, error) {
	f, err := lockLinks(statePath)
	if err != nil {
		return nil, err
	}
	if err := f.load(); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func lockLinks(statePath string) (*linksFile, error) {
	path := statePath + ".links"
	deadline := time.Now().Add(3 * time.Second)
	lock, err := lockState(path)
	for errors.Is(err, ErrLocked) && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
		lock, err = lockState(path)
	}
	if err != nil {
		return nil, err
	}
	return &linksFile{path: path, File: lock, records: map[string]accountLink{}}, nil
}

func (f *linksFile) load() error {
	data, err := os.ReadFile(f.path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if err := json.Unmarshal(data, &f.records); err != nil {
		return err
	}
	if f.records == nil {
		f.records = map[string]accountLink{}
	}
	now := time.Now().Unix()
	pruned := false
	for id, link := range f.records {
		if link.Hash != "" && link.Expires <= now {
			delete(f.records, id)
			if link.Issuer != localIssuer {
				// Keep only the expiry for the Members page. The credential and issuer are gone.
				f.records[id] = accountLink{Expires: link.Expires}
			}
			pruned = true
		}
	}
	if pruned {
		return f.save()
	}
	return nil
}

func (f *linksFile) save() error {
	data, err := json.Marshal(f.records)
	if err != nil {
		return err
	}
	return writeFile(f.path, data)
}

func (f *linksFile) find(hash string) (string, accountLink, bool) {
	now := time.Now().Unix()
	for id, link := range f.records {
		if link.Hash != "" && subtle.ConstantTimeCompare([]byte(link.Hash), []byte(hash)) == 1 && link.Expires > now {
			return id, link, true
		}
	}
	return "", accountLink{}, false
}

func (f *linksFile) issue(id, kind, issuer string, ttl time.Duration) (string, error) {
	var codeBytes [32]byte
	if _, err := rand.Read(codeBytes[:]); err != nil {
		return "", err
	}
	code := base64.RawURLEncoding.EncodeToString(codeBytes[:])
	f.records[id] = accountLink{Hash: hashToken(code), Kind: kind, Issuer: issuer, Expires: time.Now().Add(ttl).Unix()}
	return "/setup#c=" + code, f.save()
}

func adminID(users map[string]*User) string {
	for id, u := range users {
		if u.Admin {
			return id
		}
	}
	return ""
}

// IssueAdminLink creates the one-time admin link without opening the Store.
func IssueAdminLink(path string) (string, error) {
	return issueAdminLink(path, false, "")
}

// IssueAdminLinkIfNeeded returns an empty link if the admin already has a password.
func IssueAdminLinkIfNeeded(path string) (string, error) {
	return issueAdminLink(path, true, "")
}

// IssueAdminLoginLink creates a two-minute sign-in link for an existing admin.
func IssueAdminLoginLink(path string) (string, error) {
	return issueAdminLink(path, false, linkLogin)
}

func issueAdminLink(path string, ifNeeded bool, kind string) (string, error) {
	f, err := lockLinks(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	ttl := adminLinkTTL
	if ifNeeded || kind == linkLogin {
		// Redeeming the initial link also holds this lock through the state save.
		admin, err := stateAdmin(path)
		if kind == linkLogin {
			if errors.Is(err, os.ErrNotExist) || err == nil && admin == nil {
				return "", errors.New("no admin yet. Run admin-link without -login to set one up")
			}
			ttl = loginLinkTTL
		}
		if err != nil || ifNeeded && admin != nil && admin.Hash != "" {
			return "", err
		}
	}
	if err := f.load(); err != nil {
		return "", err
	}
	return f.issue(adminLinkID, kind, localIssuer, ttl)
}

type adminLinkAccount struct {
	Admin bool   `json:"admin"`
	Hash  string `json:"hash"`
}

func stateAdmin(path string) (*adminLinkAccount, error) {
	f, err := openStateSnapshot(path)
	if err != nil {
		return nil, err
	}
	raw, err := io.ReadAll(f)
	f.Close()
	if err != nil {
		return nil, err
	}
	var state struct {
		Users map[string]*adminLinkAccount `json:"users"`
	}
	if err := json.Unmarshal(raw, &state); err != nil {
		return nil, fmt.Errorf("parse %s: %w", path, err)
	}
	for _, user := range state.Users {
		if user != nil && user.Admin {
			return user, nil
		}
	}
	return nil, nil
}

func (s *Store) createMember(name, display, issuer string) (id, link string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !ValidAccountName(name) {
		return "", "", errBadName
	}
	if idOf(s.Users, name) != "" {
		return "", "", errNameTaken
	}
	f, err := openLinks(s.path)
	if err != nil {
		return "", "", err
	}
	defer f.Close()
	id = randomHex(16)
	link, err = f.issue(id, linkSetup, issuer, memberLinkTTL)
	if err != nil {
		return "", "", err
	}
	s.Users[id] = &User{Name: name, Display: display, Created: time.Now().Unix()}
	if err := s.saveLocked(); err != nil {
		delete(s.Users, id)
		delete(f.records, id)
		return "", "", errors.Join(err, f.save())
	}
	return id, link, nil
}

func (s *Store) issueMemberLink(id, issuer string) (name, link string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[id]
	if u == nil || u.Admin {
		return "", "", errNoUser
	}
	f, err := openLinks(s.path)
	if err != nil {
		return "", "", err
	}
	defer f.Close()
	kind := linkReset
	if u.Hash == "" {
		kind = linkSetup
	}
	link, err = f.issue(id, kind, issuer, memberLinkTTL)
	return u.Name, link, err
}

func (s *Store) revokeMemberLink(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if u := s.Users[id]; u == nil || u.Admin {
		return errNoUser
	}
	return s.revokeMemberLinkLocked(id)
}

func (s *Store) revokeMemberLinkLocked(id string) error {
	f, err := openLinks(s.path)
	if err != nil {
		return err
	}
	defer f.Close()
	if _, ok := f.records[id]; !ok {
		return nil
	}
	delete(f.records, id)
	return f.save()
}

type setupDetails struct {
	Kind string `json:"kind"`
	Name string `json:"name,omitempty"`
}

// setupTargetLocked verifies a link's account still exists, or can still be created.
func (s *Store) setupTargetLocked(f *linksFile, hash string) (record, user, kind string, err error) {
	record, link, ok := f.find(hash)
	if ok && record == adminLinkID {
		id := adminID(s.Users)
		if link.Kind == linkLogin {
			if id == "" {
				return "", "", "", errAccountLink
			}
			return record, id, linkLogin, nil
		}
		if id != "" {
			return record, id, linkReset, nil
		}
		return record, "", linkSetup, nil
	}
	if !ok || s.Users[record] == nil || s.Users[record].Admin {
		return "", "", "", errAccountLink
	}
	return record, record, link.Kind, nil
}

func (s *Store) setupDetails(hash string) (setupDetails, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	f, err := openLinks(s.path)
	if err != nil {
		return setupDetails{}, err
	}
	defer f.Close()
	_, id, kind, err := s.setupTargetLocked(f, hash)
	if err != nil {
		return setupDetails{}, err
	}
	details := setupDetails{Kind: kind}
	if id != "" {
		details.Name = s.Users[id].Name
	}
	return details, nil
}

type setupRequest struct {
	Code     string            `json:"code"`
	Password string            `json:"password"`
	Name     string            `json:"name"`
	Display  string            `json:"display"`
	Device   *proto.BrowserKey `json:"device"`
}

// redeemLink consumes the link under its file lock, then commits the browser and any password
// change together. If the state write fails, the old records and link can be used again. A process
// interrupted between writes fails closed: the operator can issue another link.
func (s *Store) redeemLink(body setupRequest, current, label, ip string) (user, browser, token string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	f, err := openLinks(s.path)
	if err != nil {
		return "", "", "", err
	}
	defer f.Close()
	record, id, kind, err := s.setupTargetLocked(f, hashToken(body.Code))
	if err != nil {
		return "", "", "", err
	}
	if !body.Device.Valid() || body.Device.Role != "" {
		return "", "", "", errSetupKey
	}
	var password []byte
	if kind == linkLogin {
		if body.Name != "" || body.Display != "" || body.Password != "" {
			return "", "", "", errLoginFields
		}
	} else {
		if !validSecret(body.Password) {
			return "", "", "", errBadSecret
		}
		if (id == "" && !ValidAccountName(body.Name)) || (id != "" && body.Name != "") || len([]rune(body.Display)) > 20 {
			return "", "", "", errBadName
		}
		if id == "" && idOf(s.Users, body.Name) != "" {
			return "", "", "", errNameTaken
		}
		password, err = bcrypt.GenerateFromPassword([]byte(body.Password), bcrypt.DefaultCost)
		if err != nil {
			return "", "", "", err
		}
	}

	link := f.records[record]
	delete(f.records, record)
	if err := f.save(); err != nil {
		return "", "", "", err
	}

	user = id
	previousUser := s.Users[id]
	if previousUser == nil {
		user = randomHex(16)
		now := time.Now().Unix()
		s.Users[user] = &User{Name: body.Name, Display: body.Name, Admin: true, Created: now}
	} else {
		next := *previousUser
		s.Users[user] = &next
	}
	if password != nil {
		u := s.Users[user]
		u.Hash = string(password)
		if body.Display != "" {
			u.Display = body.Display
		}
	}

	browser = current
	previousBrowser := s.Browsers[current]
	now := time.Now().Unix()
	rec := BrowserRecord{Owner: user, Label: label, IP: ip, Created: now}
	if previousBrowser == nil || previousBrowser.Owner != user || previousBrowser.Role != "" ||
		(previousBrowser.Key != nil && (previousBrowser.Key.Cred != body.Device.Cred || previousBrowser.Key.Alg != body.Device.Alg || previousBrowser.Key.Key != body.Device.Key)) {
		token = randomHex(32)
		browser = hashToken(token)
		previousBrowser = nil
	} else {
		rec = *previousBrowser
	}
	newKey := rec.Key == nil
	if newKey {
		rec.Key = body.Device
	}
	s.Browsers[browser] = &rec
	s.letInLocked(&rec, newKey, now)
	if err := s.saveLocked(); err != nil {
		if previousUser == nil {
			delete(s.Users, user)
		} else {
			s.Users[user] = previousUser
		}
		if previousBrowser == nil {
			delete(s.Browsers, browser)
		} else {
			s.Browsers[browser] = previousBrowser
		}
		f.records[record] = link
		return "", "", "", errors.Join(err, f.save())
	}
	return user, browser, token, nil
}
