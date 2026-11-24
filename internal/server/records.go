package server

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"sort"
	"time"

	"deskway/internal/proto"
)

// The Store's operations, each one step under the store's own lock, so server code never
// locks it and never sees a half-changed record. storeError (server.go) maps their errors to
// HTTP answers.

var (
	errTooMany   = errors.New("too many computers waiting for approval")
	errComputers = errors.New("this account has as many computers as it may have")
	errStale     = errors.New("a newer AI switch is already set for this computer")
)

// ---- accounts ----

const (
	minSecret = 8
	maxSecret = 128
)

var (
	errNoUser    = errors.New("no such account")
	errBadName   = errors.New("an account name is 2 to 32 letters, digits, '.', '_' or '-'")
	errNameTaken = errors.New("that account name is taken")
	errBadSecret = fmt.Errorf("a password or super password has %d to %d characters", minSecret, maxSecret)
)

// validSecret: what a password or a super password may be.
func validSecret(p string) bool { return len(p) >= minSecret && len(p) <= maxSecret }

// ValidAccountName: what an account may sign in as. 2 to 32 ASCII letters, digits, '.', '_'
// or '-', so it is typed the same on every keyboard.
func ValidAccountName(name string) bool {
	if len(name) < 2 || len(name) > 32 {
		return false
	}
	for _, c := range name {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '.' || c == '_' || c == '-') {
			return false
		}
	}
	return true
}

// idOf is the id of the account in users signing in as name, or "".
func idOf(users map[string]*User, name string) string {
	for id, u := range users {
		if u.Name == name {
			return id
		}
	}
	return ""
}

// ID is the id of the account signing in as name, or "".
func (s *Store) ID(name string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return idOf(s.Users, name)
}

// Name is what the account signs in with.
func (s *Store) Name(user string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if u := s.Users[user]; u != nil {
		return u.Name
	}
	return ""
}

// Rename changes what the account signs in with. Nothing else changes: everything it has is
// kept under its id. A display name that was the old name follows it.
func (s *Store) Rename(user, name string) error {
	if !ValidAccountName(name) {
		return errBadName
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	if u == nil {
		return errNoUser
	}
	if other := idOf(s.Users, name); other != "" && other != user {
		return errNameTaken
	}
	if u.Display == u.Name {
		u.Display = name
	}
	u.Name = name
	return s.saveLocked()
}

// UserCount is the number of accounts.
func (s *Store) UserCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.Users)
}

// Display is the account's display name, or the name it signs in with.
func (s *Store) Display(user string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	switch {
	case u == nil:
		return ""
	case u.Display != "":
		return u.Display
	default:
		return u.Name
	}
}

// SetDisplay changes the name others see.
func (s *Store) SetDisplay(user, name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	if u == nil {
		return errNoUser
	}
	u.Display = name
	return s.saveLocked()
}

// IsAdmin says whether the account is the server's admin.
func (s *Store) IsAdmin(user string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	return u != nil && u.Admin
}

// MustChange says whether the account's password is an initial one.
func (s *Store) MustChange(user string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	return u != nil && u.MustChange
}

// Member is a member account as the admin sees it: of its computers, only how many.
type Member struct {
	ID         string `json:"id"`
	Name       string `json:"name"`
	Display    string `json:"display"`
	Created    int64  `json:"created"`
	LastActive int64  `json:"last_active,omitempty"` // the latest any of its browsers was seen
	Computers  int    `json:"computers"`
}

// Members lists every account but the admin, by sign-in name.
func (s *Store) Members() []Member {
	s.mu.Lock()
	defer s.mu.Unlock()
	byID := map[string]*Member{}
	for id, u := range s.Users {
		if !u.Admin {
			byID[id] = &Member{ID: id, Name: u.Name, Display: u.Display, Created: u.Created}
		}
	}
	for _, b := range s.Browsers {
		if m := byID[b.Owner]; m != nil && b.LastSeen > m.LastActive {
			m.LastActive = b.LastSeen
		}
	}
	for _, d := range s.Devices {
		if m := byID[d.Owner]; m != nil {
			m.Computers++
		}
	}
	list := make([]Member, 0, len(byID))
	for _, m := range byID {
		list = append(list, *m)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Name < list[j].Name })
	return list
}

// ---- the device log ----

// maxLog is how many entries of an account's device log are kept; older ones go first.
const maxLog = 500

func (s *Store) appendLogLocked(user string, e proto.LogEntry) {
	u := s.Users[user]
	if u == nil {
		return
	}
	u.DeviceLog = append(u.DeviceLog, e)
	if len(u.DeviceLog) > maxLog {
		u.DeviceLog = u.DeviceLog[len(u.DeviceLog)-maxLog:]
	}
}

// AppendLog adds a signed entry to the account's device log.
func (s *Store) AppendLog(user string, e proto.LogEntry) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.appendLogLocked(user, e)
	return s.saveLocked()
}

// DeviceLog is the account's log and the computers it owns, each with its latest AI switch
// (nil: never set).
func (s *Store) DeviceLog(user string) (entries []proto.LogEntry, owned map[string]*proto.AiSwitch) {
	s.mu.Lock()
	defer s.mu.Unlock()
	entries = []proto.LogEntry{}
	if u := s.Users[user]; u != nil {
		entries = append(entries, u.DeviceLog...)
	}
	owned = map[string]*proto.AiSwitch{}
	for id, d := range s.Devices {
		if d.Owner == user {
			owned[id] = d.AI
		}
	}
	return entries, owned
}

// SetSwitch keeps a computer's AI switch, signed by one of the account's browsers. Only a newer
// one replaces it: the computer would ignore an older one anyway (errStale). False if the
// account has no such computer.
func (s *Store) SetSwitch(user string, sw proto.AiSwitch) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[sw.Host]
	if d == nil || d.Pending || d.Owner != user {
		return false, nil
	}
	if d.AI != nil && d.AI.TS >= sw.TS {
		return true, errStale
	}
	d.AI = &sw
	return true, s.saveLocked()
}

// Policy is the latest device-check choice in the account's log (true: checks required).
func (s *Store) Policy(user string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	keys := true
	if u := s.Users[user]; u != nil {
		for _, e := range u.DeviceLog {
			if e.Op == proto.OpPolicy && e.Keys != nil {
				keys = *e.Keys
			}
		}
	}
	return keys
}

// ---- computers ----

// CanAccess reports whether the account may see and use the computer: only its own, once let in.
func (s *Store) CanAccess(user, device string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[device]
	return d != nil && s.Users[user] != nil && !d.Pending && d.Owner == user
}

// DeviceTokenOK checks a computer's token.
func (s *Store) DeviceTokenOK(id, token string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Devices[id]
	return rec != nil && subtle.ConstantTimeCompare([]byte(hashToken(token)), []byte(rec.TokenHash)) == 1
}

// DeviceOwner is the computer's owner and whether it is let in yet.
func (s *Store) DeviceOwner(id string) (owner string, approved bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[id]
	if d == nil {
		return "", false
	}
	return d.Owner, !d.Pending
}

// DevicesFor lists the account's computers that are let in, without what only the live
// connections know (online, controller, displays, version).
func (s *Store) DevicesFor(user string) []proto.Device {
	s.mu.Lock()
	defer s.mu.Unlock()
	list := []proto.Device{}
	for id, d := range s.Devices {
		if !d.Pending && d.Owner == user {
			dev := proto.Device{ID: id, Name: d.Name, OS: d.OS, LastSeen: d.LastSeen, Tools: d.Tools}
			if d.AI != nil {
				dev.AI = &proto.AiView{State: proto.AIOff, Admin: d.AI.Admin, TS: d.AI.TS}
				if d.AI.AI {
					dev.AI.State = proto.AIOn
				}
			}
			list = append(list, dev)
		}
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Name < list[j].Name })
	return list
}

// Enroll adds a computer waiting for approval. An account has at most `most` computers,
// waiting ones included, and at most `waiting` of them wait at once.
func (s *Store) Enroll(id string, rec *DeviceRecord, waiting, most int) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	all, pending := 0, 0
	for _, d := range s.Devices {
		if d.Owner == rec.Owner {
			all++
			if d.Pending {
				pending++
			}
		}
	}
	if all >= most {
		return errComputers
	}
	if pending >= waiting {
		return errTooMany
	}
	s.Devices[id] = rec
	return s.saveLocked()
}

// Touch records that the computer was seen now.
func (s *Store) Touch(device string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if d := s.Devices[device]; d != nil {
		d.LastSeen = time.Now().Unix()
		return s.saveLocked()
	}
	return nil
}

// SetTools keeps the AI tools the computer said it carries out, saved only when they changed.
func (s *Store) SetTools(device string, tools []string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[device]
	if d == nil || slices.Equal(d.Tools, tools) {
		return nil
	}
	d.Tools = slices.Clone(tools)
	return s.saveLocked()
}

// RenameDevice renames a computer the account owns; false if it owns no such computer.
func (s *Store) RenameDevice(user, id, name string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[id]
	if d == nil || d.Owner != user {
		return false, nil
	}
	d.Name = name
	return true, s.saveLocked()
}

// RemoveDevice takes a computer's record off. With a non-empty owner, only if that account
// owns it. False if there was nothing to remove.
func (s *Store) RemoveDevice(owner, id string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := s.Devices[id]
	if d == nil || (owner != "" && d.Owner != owner) {
		return false, nil
	}
	delete(s.Devices, id)
	return true, s.saveLocked()
}

// ---- browsers ----

// BrowserInfo is an approved browser as its account sees it.
type BrowserInfo struct {
	ID       string
	Label    string
	IP       string
	LastSeen int64
	Key      *proto.BrowserKey
}

// BrowserRole is the record's role: proto.RoleAI for an AI client, "" for a browser.
func (s *Store) BrowserRole(browser string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if rec := s.Browsers[browser]; rec != nil {
		return rec.Role
	}
	return ""
}

// SignedInOwner is the account of an approved browser, or "".
func (s *Store) SignedInOwner(browser string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[browser]
	if rec == nil || !rec.Approved || s.Users[rec.Owner] == nil {
		return ""
	}
	return rec.Owner
}

// BrowserCred is the credential of the browser's device key, or "" without one.
func (s *Store) BrowserCred(browser string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[browser]
	if rec == nil || rec.Key == nil {
		return ""
	}
	return rec.Key.Cred
}

// ApprovedBrowsers lists the account's approved browsers.
func (s *Store) ApprovedBrowsers(user string) []BrowserInfo {
	s.mu.Lock()
	defer s.mu.Unlock()
	rows := []BrowserInfo{}
	for id, b := range s.Browsers {
		if b.Owner == user && b.Approved {
			rows = append(rows, BrowserInfo{id, b.Label, b.IP, b.LastSeen, b.Key})
		}
	}
	sort.Slice(rows, func(i, j int) bool { return rows[i].LastSeen > rows[j].LastSeen })
	return rows
}

// BrowserLogin is what a password login finds for this browser.
type BrowserLogin int

const (
	LoginNew      BrowserLogin = iota // no record of this account here
	LoginWaiting                      // asked before, not approved yet
	LoginApproved                     // approved: signed in again
)

// LoginBrowser looks at the browser's record for a password login by user as role (an AI
// client's, or a browser's); an approved one is marked seen.
func (s *Store) LoginBrowser(id, user, role string, now int64) (BrowserLogin, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[id]
	switch {
	case rec == nil || rec.Owner != user || rec.Role != role:
		return LoginNew, nil
	case rec.Approved:
		rec.LastSeen = now
		rec.Pickup = false
		return LoginApproved, s.saveLocked()
	default:
		return LoginWaiting, nil
	}
}

// AIClient tells what the server knows of the browser record as user's AI client: its key, and
// whether it was approved, or has asked before.
func (s *Store) AIClient(id, user string) (key *proto.BrowserKey, approved, asked bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[id]
	if rec == nil || user == "" || rec.Owner != user || rec.Role != proto.RoleAI {
		return nil, false, false
	}
	return rec.Key, rec.Approved, true
}

// ApproveWithSuper lets the browser in with the super password: it joins the device list,
// unsigned, if its key is known.
func (s *Store) ApproveWithSuper(id, user, label, ip string, now int64) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.letInLocked(id, user, label, ip, now)
}

// ApproveFirst lets the account's first browser in with the initial password alone: there is
// no other device to approve it, and the password came from the admin (docs/accounts.md, First
// sign-in). False, changing nothing, once the account has had an approved browser or has its
// own password.
func (s *Store) ApproveFirst(id, user, label, ip string, now int64) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := s.Users[user]
	if u == nil || !u.MustChange || u.HadBrowser {
		return false, nil
	}
	return true, s.letInLocked(id, user, label, ip, now)
}

// letInLocked approves the browser on the spot, with no other device.
func (s *Store) letInLocked(id, user, label, ip string, now int64) error {
	rec := s.Browsers[id]
	if rec == nil || rec.Owner != user {
		rec = &BrowserRecord{Owner: user, Label: label, IP: ip, Created: now}
		s.Browsers[id] = rec
	}
	rec.Approved, rec.Pickup, rec.Code, rec.LastSeen = true, false, "", now
	if u := s.Users[user]; u != nil {
		u.HadBrowser = true
	}
	if rec.Key != nil {
		s.appendLogLocked(user, rec.Key.AddEntry(proto.Proof{TS: now}))
	}
	return s.saveLocked()
}

// AskApproval records that the browser (an AI client for RoleAI) waits for approval and returns
// its code. Someone holding the password cannot flood the list: past max waiting, the oldest
// goes.
func (s *Store) AskApproval(id, user, role, label, ip, code string, now int64, max int) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[id]
	if rec == nil || rec.Owner != user {
		var pending []string
		for bid, b := range s.Browsers {
			if b.Owner == user && !b.Approved {
				pending = append(pending, bid)
			}
		}
		if len(pending) >= max {
			sort.Slice(pending, func(i, j int) bool { return s.Browsers[pending[i]].Created < s.Browsers[pending[j]].Created })
			delete(s.Browsers, pending[0])
		}
		rec = &BrowserRecord{Owner: user, Label: label, IP: ip, Code: code, Created: now, Role: role}
		s.Browsers[id] = rec
	} else {
		rec.Created = now // asking again keeps the same code and restarts the wait
	}
	return rec.Code, s.saveLocked()
}

// Pickup is what a waiting browser finds when it checks.
type Pickup int

const (
	PickupGone    Pickup = iota // no request, or it expired, or was collected
	PickupWaiting               // not approved yet
	PickupReady                 // approved just now: collect the session once
)

// CollectApproval lets a waiting browser collect its session once, right after approval.
func (s *Store) CollectApproval(id string, ttl time.Duration) (Pickup, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[id]
	switch {
	case rec == nil:
		return PickupGone, nil
	case !rec.Approved && time.Since(time.Unix(rec.Created, 0)) > ttl:
		delete(s.Browsers, id)
		return PickupGone, s.saveLocked()
	case !rec.Approved:
		return PickupWaiting, nil
	case rec.Pickup && time.Since(time.Unix(rec.Created, 0)) <= ttl:
		rec.Pickup = false
		rec.LastSeen = time.Now().Unix()
		return PickupReady, s.saveLocked()
	default:
		return PickupGone, nil
	}
}

// Approvals lists what waits for the account's approval, oldest first.
func (s *Store) Approvals(user string, ttl time.Duration) []proto.Approval {
	s.mu.Lock()
	defer s.mu.Unlock()
	list := []proto.Approval{}
	for id, b := range s.Browsers {
		if b.Owner == user && !b.Approved && time.Since(time.Unix(b.Created, 0)) <= ttl {
			list = append(list, proto.Approval{Kind: "browser", ID: id, Label: b.Label, IP: b.IP, Code: b.Code, Created: b.Created, Device: approvalKey(b)})
		}
	}
	for id, d := range s.Devices {
		if d.Owner == user && d.Pending {
			a := proto.Approval{Kind: "device", ID: id, Label: d.Name, IP: d.EnrollIP, Created: d.LastSeen}
			if d.ShowsCode {
				a.Code = proto.ComputerCode(id)
			}
			list = append(list, a)
		}
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Created < list[j].Created })
	return list
}

// approvalKey is the waiting browser's key, which the approving device signs for.
func approvalKey(b *BrowserRecord) json.RawMessage {
	if b.Key == nil {
		return nil
	}
	raw, _ := json.Marshal(b.Key)
	return raw
}

// Approve answers a waiting browser or computer of the account. An approved browser with a
// key and the approver's signature joins the device log. False if there was no such request.
func (s *Store) Approve(user, kind, id string, allow bool, endorse *proto.Proof, ttl time.Duration) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	found := false
	switch kind {
	case "browser":
		if b := s.Browsers[id]; b != nil && b.Owner == user && !b.Approved {
			found = time.Since(time.Unix(b.Created, 0)) <= ttl
			if found && allow {
				b.Approved, b.Pickup, b.Code = true, true, ""
				if u := s.Users[user]; u != nil {
					u.HadBrowser = true
				}
				if b.Key != nil && endorse.Valid() {
					s.appendLogLocked(user, b.Key.AddEntry(*endorse))
				}
			} else {
				delete(s.Browsers, id)
			}
		}
	case "device":
		if d := s.Devices[id]; d != nil && d.Owner == user && d.Pending {
			found = true
			if allow {
				d.Pending = false
			} else {
				delete(s.Devices, id)
			}
		}
	}
	if !found {
		return false, nil
	}
	return true, s.saveLocked()
}

// AttachKey gives a browser its key, once: while it waits (its code comes from the
// key) or after approval without one (then it joins the log, unsigned). Returns its account.
func (s *Store) AttachKey(id, signedIn string, k proto.BrowserKey, now int64) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rec := s.Browsers[id]
	if rec == nil || (rec.Approved && rec.Owner != signedIn) || (rec.Key != nil && rec.Key.Cred != k.Cred) || k.Role != rec.Role {
		return "", false
	}
	first := rec.Key == nil
	// The name is the account's once set (renamed on another device, perhaps): a browser
	// telling its key again does not undo that.
	if !first {
		k.Name = rec.Key.Name
	}
	rec.Key = &k
	if !rec.Approved {
		rec.Code = k.Code()
	} else if first {
		s.appendLogLocked(rec.Owner, k.AddEntry(proto.Proof{TS: now}))
	}
	_ = s.saveLocked()
	return rec.Owner, true
}

// RemoveBrowser removes one of the account's browsers; with a proof from another of its
// devices, the computers are told to stop trusting its key. False if there is no such browser.
func (s *Store) RemoveBrowser(user, id string, proof *proto.Proof) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	b := s.Browsers[id]
	if b == nil || b.Owner != user {
		return false, nil
	}
	delete(s.Browsers, id)
	if b.Key != nil && proof.Valid() {
		e := proof.Entry(proto.OpRemove)
		e.Cred = b.Key.Cred
		s.appendLogLocked(user, e)
	}
	return true, s.saveLocked()
}

// RenameBrowser names one of the account's approved devices. With a proof, the log carries the
// name to its computers: one rename entry per device, the newest (older ones for it go, so
// renaming never fills the log). False if there is no such browser.
func (s *Store) RenameBrowser(user, id, name string, proof *proto.Proof) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	b := s.Browsers[id]
	if b == nil || b.Owner != user || !b.Approved {
		return false, nil
	}
	if b.Key == nil {
		b.Label = name
		return true, s.saveLocked()
	}
	b.Key.Name = name
	if u := s.Users[user]; u != nil && proof.Valid() {
		kept := u.DeviceLog[:0]
		for _, e := range u.DeviceLog {
			if e.Op != proto.OpRename || e.Cred != b.Key.Cred {
				kept = append(kept, e)
			}
		}
		u.DeviceLog = kept
		e := proof.Entry(proto.OpRename)
		e.Cred, e.Name = b.Key.Cred, name
		s.appendLogLocked(user, e)
	}
	return true, s.saveLocked()
}

// RemoveBrowsersExcept removes every browser of the account but one; returns the removed ids.
func (s *Store) RemoveBrowsersExcept(user, keep string) map[string]bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	removed := map[string]bool{}
	for id, b := range s.Browsers {
		if b.Owner == user && id != keep {
			delete(s.Browsers, id)
			removed[id] = true
		}
	}
	_ = s.saveLocked()
	return removed
}
