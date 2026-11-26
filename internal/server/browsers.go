package server

import (
	"encoding/json"
	"log"
	"net/http"
	"strings"
	"time"
	"unicode/utf8"

	"deskway/internal/proto"
)

// A password alone must not be enough to reach a computer. A new browser or computer waits
// until a browser that is already approved lets it in, so a leaked password gets nowhere.
//
// An approved browser also has a key of its own (proto.BrowserKey). The account's signed log
// of these keys tells its computers which browsers to trust (docs/security.md, "Devices"). The
// HTTP API calls them devices (/api/devices).
const (
	pendingBrowserTTL = 10 * time.Minute
	maxPendingPerUser = 5
)

// sendApprovals gives the account's open pages what waits for their approval.
func (s *Server) sendApprovals(user string) {
	list := s.store.Approvals(user, pendingBrowserTTL)
	s.mu.Lock()
	defer s.mu.Unlock()
	for c := range s.clients {
		if c.user == user {
			c.send(proto.Msg{Type: proto.TypeApprovals, Approvals: list})
		}
	}
}

// sendDeviceLog gives the account's computers its device log, each with its own AI switch, now
// and after every change.
func (s *Server) sendDeviceLog(user string) {
	entries, owned := s.store.DeviceLog(user)
	raw, _ := json.Marshal(entries)
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, c := range s.hosts {
		if sw, ok := owned[id]; ok {
			c.send(proto.Msg{Type: proto.TypeDeviceLog, Auth: raw, AI: sw})
		}
	}
}

// sendAccount tells the account's computers who they belong to (its sign-in name and the name
// others see), for their panels: when they connect and whenever either name changes.
func (s *Server) sendAccount(user string) {
	m := proto.Msg{Type: proto.TypeAccount, User: s.store.Name(user), Display: s.store.Display(user)}
	_, owned := s.store.DeviceLog(user)
	s.mu.Lock()
	defer s.mu.Unlock()
	for id := range owned {
		if c := s.hosts[id]; c != nil {
			c.send(m)
		}
	}
}

// dropBrowsers closes every socket of the given browser records, so a removed browser is out
// at once and not only at its next request.
func (s *Server) dropBrowsers(ids map[string]bool, profiles []string) {
	s.mu.Lock()
	for c := range s.clients {
		if ids[c.browser] {
			c.cancel()
		}
	}
	s.mu.Unlock()
	s.retireMCPProfiles(profiles)
}

// handleApprove answers a waiting browser or computer. A browser approved with the approver's
// signature over its key joins the device log.
func (s *Server) handleApprove(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Kind    string       `json:"kind"`
		ID      string       `json:"id"`
		Allow   bool         `json:"allow"`
		Endorse *proto.Proof `json:"endorse"`
	}
	if !decodeBody(w, r, 16384, &body) {
		return
	}
	found, err := s.store.Approve(user, body.Kind, body.ID, body.Allow, body.Endorse, pendingBrowserTTL)
	if !found {
		http.Error(w, "no such request", http.StatusNotFound)
		s.sendApprovals(user)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s %s request %.8s (allow=%v)", user, body.Kind, body.ID, body.Allow)
	s.sendApprovals(user)
	s.sendDeviceLog(user)
	s.broadcastDevices()
	w.WriteHeader(http.StatusNoContent)
}

// handleBrowserKeyAttach: a browser gives its key, either while it waits for approval (shown to
// the approving browser with a code made from it) or once signed in, if it was approved before
// it had one. A key is set once; a new key means a new browser.
func (s *Server) handleBrowserKeyAttach(w http.ResponseWriter, r *http.Request) {
	var k proto.BrowserKey
	if !decodeBody(w, r, 8192, &k) {
		return
	}
	if !k.Valid() {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	user, _ := s.sessionOf(r)
	owner, ok := s.store.AttachKey(browserFromCookie(r), user, k, time.Now().Unix())
	if !ok {
		http.Error(w, "no such browser", http.StatusNotFound)
		return
	}
	s.sendApprovals(owner)
	s.sendDeviceLog(owner)
	writeJSON(w, map[string]string{"code": k.Code()})
}

// handleBrowserKeys lists the account's approved browsers with their keys, newest use first.
func (s *Server) handleBrowserKeys(w http.ResponseWriter, r *http.Request) {
	current := browserOf(r)
	type row struct {
		ID       string            `json:"id"`
		Label    string            `json:"label"`
		IP       string            `json:"ip"`
		LastSeen int64             `json:"last_seen"`
		Current  bool              `json:"current"`
		Remote   bool              `json:"remote_mcp,omitempty"`
		Key      *proto.BrowserKey `json:"device,omitempty"`
	}
	rows := []row{}
	for _, b := range s.store.ApprovedBrowsers(userOf(r)) {
		rows = append(rows, row{b.ID, b.Label, b.IP, b.LastSeen, b.ID == current, b.Remote, b.Key})
	}
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, rows)
}

// handleBrowserRemove signs a browser out at once and, with a proof from another of the
// account's browsers, tells the computers to stop trusting its key too.
func (s *Server) handleBrowserRemove(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	id := r.PathValue("id")
	var body struct {
		Proof *proto.Proof `json:"proof"`
	}
	// The proof is optional: without one the browser is signed out, and computers keep their
	// trust in its key until they are told otherwise.
	_ = json.NewDecoder(http.MaxBytesReader(w, r.Body, 16384)).Decode(&body)
	found, profiles, err := s.store.RemoveBrowser(user, id, body.Proof)
	if !found {
		http.Error(w, "no such device", http.StatusNotFound)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	s.dropBrowsers(map[string]bool{id: true}, profiles)
	log.Printf("server: %s removed browser %.8s", user, id)
	s.sendDeviceLog(user)
	w.WriteHeader(http.StatusNoContent)
}

// maxDeviceName bounds a device's name: as many characters as a computer's, and as many bytes
// as a device key's name may hold.
const maxDeviceName = 40

// handleBrowserRename gives one of the account's devices a new name: on the website at once,
// and, with a proof from one of its devices (deskway-rename-v1), on its computers too.
func (s *Server) handleBrowserRename(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Name  string       `json:"name"`
		Proof *proto.Proof `json:"proof"`
	}
	if !decodeBody(w, r, 16384, &body) {
		return
	}
	// The name goes as it was signed: one with spaces around it is the page's mistake.
	if body.Name == "" || body.Name != strings.TrimSpace(body.Name) || utf8.RuneCountInString(body.Name) > maxDeviceName || len(body.Name) > 120 {
		http.Error(w, "invalid name", http.StatusBadRequest)
		return
	}
	found, err := s.store.RenameBrowser(user, r.PathValue("id"), body.Name, body.Proof)
	if !found {
		http.Error(w, "no such device", http.StatusNotFound)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s renamed browser %.8s", user, r.PathValue("id"))
	s.sendDeviceLog(user)
	w.WriteHeader(http.StatusNoContent)
}

// handlePolicy records a browser's signed choice to turn device checks off or on.
func (s *Server) handlePolicy(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Keys  bool         `json:"keys"`
		Proof *proto.Proof `json:"proof"`
	}
	if !decodeBody(w, r, 16384, &body) {
		return
	}
	if !body.Proof.Valid() {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	e := body.Proof.Entry(proto.OpPolicy)
	e.Keys = &body.Keys
	if err := s.store.AppendLog(user, e); err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s set device checks to %v", user, body.Keys)
	s.sendDeviceLog(user)
	w.WriteHeader(http.StatusNoContent)
}

// handleAISwitch records a browser's signed choice for one computer: whether AI clients may
// control it, and with administrator rights or not. The computer checks the signature, and
// that it is newer than the one it has.
func (s *Server) handleAISwitch(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		AI    bool         `json:"ai"`
		Admin bool         `json:"admin"`
		Proof *proto.Proof `json:"proof"`
	}
	if !decodeBody(w, r, 16384, &body) {
		return
	}
	if !body.Proof.Valid() {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	found, err := s.store.SetSwitch(user, body.Proof.Switch(r.PathValue("id"), body.AI, body.Admin))
	if !found {
		http.Error(w, "no such computer", http.StatusNotFound)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s set AI on computer %.8s to %v (admin %v)", user, r.PathValue("id"), body.AI, body.Admin)
	s.sendDeviceLog(user)
	s.broadcastDevices()
	w.WriteHeader(http.StatusNoContent)
}

// handleGetPolicy reports the latest choice, for display. Computers check the signatures.
func (s *Server) handleGetPolicy(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, map[string]bool{"keys": s.store.Policy(userOf(r))})
}
