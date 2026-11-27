package server

import (
	"errors"
	"log"
	"net/http"
	"slices"
	"strings"
	"time"

	"deskway/internal/proto"
)

// Computers: enrolling with the account password or an admin link, leaving with their own token, and what their
// owner changes from a browser. Every browser of an account sees the list of its computers
// (deviceList), sent again on every change (broadcastDevices).

// maxComputersPerUser is how many computers an account may have, waiting ones included
// (docs/accounts.md, Limits). An enrollment beyond it is refused with 403, which the host
// tells as such.
const maxComputersPerUser = 10

func (s *Server) deviceTokenOK(id string, r *http.Request) bool {
	return s.store.DeviceTokenOK(id, strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer "))
}

// handleEnroll signs a computer in with the account password or a one-time admin token.
// The token enrolls it already approved. With a password, a new computer gets its id and
// token, and waits until an approved browser lets it in. One that signed out of this account
// names itself (`back`, its id) and is taken back as it is: approved, with its AI switch and
// the token it already holds. The answer then carries only the id, so the password and an id
// give nothing a new computer would not; only removing a computer makes it new.
func (s *Server) handleEnroll(w http.ResponseWriter, r *http.Request) {
	var body struct {
		User        string `json:"user"`
		Password    string `json:"password"`
		Name        string `json:"name"`
		OS          string `json:"os"`
		Back        string `json:"back"`
		EnrollToken string `json:"enroll_token"`
		// It shows its six digits while waiting, so its approval may ask to compare them.
		ShowsCode bool `json:"shows_code"`
	}
	if !decodeBody(w, r, 4096, &body) {
		return
	}
	body.Name = strings.TrimSpace(body.Name)
	if body.Name == "" || len(body.Name) > 128 || len(body.OS) > 16 {
		http.Error(w, "bad device", http.StatusBadRequest)
		return
	}
	id, token := randomHex(16), randomHex(32)
	rec := &DeviceRecord{Name: body.Name, OS: body.OS, TokenHash: hashToken(token), LastSeen: time.Now().Unix(), EnrollIP: clientIP(r)}
	if r.URL.Path == "/api/computers/enroll" || body.EnrollToken != "" {
		if r.URL.Path != "/api/computers/enroll" || body.EnrollToken == "" || body.User != "" || body.Password != "" || body.Back != "" {
			http.Error(w, "use one enrollment credential", http.StatusBadRequest)
			return
		}
		var user string
		var err error
		if !s.attempt(w, r, http.StatusUnauthorized, errAccountLink.Error(), func() bool {
			user, err = s.store.enrollAdmin(body.EnrollToken, id, rec, maxComputersPerUser)
			return !errors.Is(err, errAccountLink)
		}) {
			return
		}
		if err != nil {
			storeError(w, err)
			return
		}
		log.Printf("server: %q enrolled by admin-link for %s", body.Name, user)
		s.broadcastDevices()
		writeJSON(w, map[string]string{"id": id, "token": token, "user": user})
		return
	}
	user := s.checkCredentials(w, r, body.User, body.Password)
	if user == "" {
		return
	}
	if owner, _ := s.store.DeviceOwner(body.Back); body.Back != "" && owner == user {
		log.Printf("server: computer %.8s signed in again for %s", body.Back, user)
		writeJSON(w, map[string]string{"id": body.Back})
		return
	}
	// It stays invisible until an approved browser lets it in, so a stolen password
	// cannot plant a fake computer in the list for the owner to type into.
	rec.Owner, rec.Pending, rec.ShowsCode = user, true, body.ShowsCode
	err := s.store.Enroll(id, rec, maxPendingPerUser, maxComputersPerUser)
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %q waiting for approval for %s", body.Name, user)
	s.sendApprovals(user)
	writeJSON(w, map[string]string{"id": id, "token": token})
}

// handleLeave withdraws a computer's sign-in that was never put to use (its elevated step was
// declined); only its own token can do this. Signing out keeps a computer.
func (s *Server) handleLeave(w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	if !s.deviceTokenOK(id, r) {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	if _, err := s.store.RemoveDevice("", id); err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: computer %.8s left", id)
	s.dropComputer(id)
	w.WriteHeader(http.StatusNoContent)
}

// handleComputerRename renames a computer the account owns. The new name shows everywhere at once.
func (s *Server) handleComputerRename(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name string `json:"name"`
	}
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	name := strings.TrimSpace(body.Name)
	if name == "" || len([]rune(name)) > 40 {
		http.Error(w, "invalid name", http.StatusBadRequest)
		return
	}
	found, err := s.store.RenameDevice(userOf(r), r.PathValue("id"), name)
	if !found {
		http.Error(w, "no such computer", http.StatusNotFound)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	s.broadcastDevices()
	w.WriteHeader(http.StatusNoContent)
}

// handleComputerRemove takes a computer off the account: its token stops working, its
// connection closes, and it disappears from every list. The program on it is not touched.
func (s *Server) handleComputerRemove(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	id := r.PathValue("id")
	found, err := s.store.RemoveDevice(user, id)
	if !found {
		http.Error(w, "no such computer", http.StatusNotFound)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s removed computer %.8s", user, id)
	s.dropComputer(id)
	w.WriteHeader(http.StatusNoContent)
}

// forgetHostLocked drops what the server knows about a computer only while it is connected.
func (s *Server) forgetHostLocked(id string) {
	delete(s.hosts, id)
	delete(s.displays, id)
	s.clearPreviewsLocked(id)
}

func (s *Server) clearPreviewsLocked(id string) {
	delete(s.previews, id)
	delete(s.asked, id)
}

// dropComputer ends everything live of a computer that was removed: its socket, its state and
// its previews. Every browser is sent the list without it.
func (s *Server) dropComputer(id string) {
	s.mu.Lock()
	host := s.hosts[id]
	s.forgetHostLocked(id)
	s.mu.Unlock()
	if host != nil {
		host.cancel()
	}
	s.broadcastDevices()
}

// deviceList is the computers of browser c's account with what the live connections add:
// online, who holds them (as this browser sees it), their displays, hello and vitals.
func (s *Server) deviceList(c *conn) []proto.Device {
	s.mu.Lock()
	list := s.store.DevicesFor(c.user)
	showTerminals := slices.Contains(c.caps, proto.CapTerminals)
	holders := make([]*proto.Holder, len(list))
	for i := range list {
		d := &list[i]
		if h := s.hosts[d.ID]; h != nil {
			d.Online, d.Version, d.Caps = true, h.version, h.caps
			d.Machine, d.Vitals = h.machine, h.vitals
			holders[i] = h.holder
			if !c.ai {
				d.KeptTerminal = h.keptTerminal
				if showTerminals {
					d.Terminals = h.terminals
				}
				d.WorkspaceAccess = h.workspaceAccess
			}
			d.AI = aiView(d.AI, h.aiState, h.aiRefused, h.aiHolder)
		}
		d.Displays = s.displays[d.ID]
		d.Here = c.here[d.ID]
	}
	s.mu.Unlock()
	for i, h := range holders {
		if h != nil {
			list[i].Controller = s.controllerFor(c, h)
		}
		if kept := list[i].KeptTerminal; kept != nil {
			terminal := s.terminalFor(c.user, *kept)
			list[i].KeptTerminal = &terminal
		}
		list[i].Terminals = slices.Clone(list[i].Terminals)
		for j := range list[i].Terminals {
			terminal := &list[i].Terminals[j].KeptTerminal
			*terminal = s.terminalFor(c.user, *terminal)
		}
	}
	return list
}

func (s *Server) terminalFor(user string, terminal proto.KeptTerminal) proto.KeptTerminal {
	if name := s.store.BrowserName(user, terminal.Cred); name != "" {
		terminal.Name = name
	}
	terminal.Cred = ""
	return terminal
}

// aiView is AI clients on an online computer as its account sees it: the switch the server
// keeps (signed), with what the computer itself says it allows now (stopped there, or a switch
// it has not taken, or refused: `refused`, the time of the newest switch it refused) and the AI
// client using it. Separate from the controller: an AI client is
// beside the person, never someone to take over from.
func aiView(sw *proto.AiView, state string, refused int64, holder *proto.Holder) *proto.AiView {
	if sw == nil && state == "" && holder == nil {
		return nil
	}
	v := proto.AiView{State: proto.AIOff}
	if sw != nil {
		v = *sw
	}
	if state != "" {
		v.State = state
	}
	v.Refused = sw != nil && refused != 0 && refused == sw.TS
	if holder != nil {
		v.Name, v.Since = holder.Name, holder.Since
	}
	return &v
}

// controllerFor is who holds a computer as browser c sees it (docs/session-lifecycle.md, "Who
// controls"): its own device is told by the device credential the host verified, never by a
// name. Only the computer's account reaches it, so the holder is always of c's account.
func (s *Server) controllerFor(c *conn, h *proto.Holder) *proto.Controller {
	return &proto.Controller{
		Device:     h.Cred != "" && h.Cred == s.store.BrowserCred(c.browser),
		Name:       h.Name,
		Connecting: !h.Established,
	}
}

func (s *Server) broadcastDevices() {
	s.broadcastDevicesTo(nil)
}

// include, when supplied, checks each client while Server.mu is held.
func (s *Server) broadcastDevicesTo(include func(*conn) bool) {
	s.mu.Lock()
	clients := make([]*conn, 0, len(s.clients))
	for c := range s.clients {
		if include == nil || include(c) {
			clients = append(clients, c)
		}
	}
	s.mu.Unlock()
	for _, c := range clients {
		c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
	}
}
