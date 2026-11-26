package server

import (
	"log"
	"net/http"
	"strings"
	"time"

	"deskway/internal/proto"
)

// Computers: enrolling with the account password, leaving with their own token, and what their
// owner changes from a browser. Every browser of an account sees the list of its computers
// (deviceList), sent again on every change (broadcastDevices).

// maxComputersPerUser is how many computers an account may have, waiting ones included
// (docs/accounts.md, Limits). An enrollment beyond it is refused with 403, which the host
// tells as such.
const maxComputersPerUser = 10

func (s *Server) deviceTokenOK(id string, r *http.Request) bool {
	return s.store.DeviceTokenOK(id, strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer "))
}

// handleEnroll signs a computer in with the account password. A new computer gets its id and
// token, and waits until an approved browser lets it in. One that signed out of this account
// names itself (`back`, its id) and is taken back as it is: approved, with its AI switch and
// the token it already holds. The answer then carries only the id, so the password and an id
// give nothing a new computer would not; only removing a computer makes it new.
func (s *Server) handleEnroll(w http.ResponseWriter, r *http.Request) {
	var body struct {
		User     string `json:"user"`
		Password string `json:"password"`
		Name     string `json:"name"`
		OS       string `json:"os"`
		Back     string `json:"back"`
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
	user := s.checkCredentials(w, r, body.User, body.Password)
	if user == "" {
		return
	}
	if owner, _ := s.store.DeviceOwner(body.Back); body.Back != "" && owner == user {
		log.Printf("server: computer %.8s signed in again for %s", body.Back, user)
		writeJSON(w, map[string]string{"id": body.Back})
		return
	}
	id, token := randomHex(16), randomHex(32)
	// It stays invisible until an approved browser lets it in, so a stolen password
	// cannot plant a fake computer in the list for the owner to type into.
	err := s.store.Enroll(id, &DeviceRecord{Owner: user, Name: body.Name, OS: body.OS, TokenHash: hashToken(token), LastSeen: time.Now().Unix(), Pending: true, EnrollIP: clientIP(r), ShowsCode: body.ShowsCode}, maxPendingPerUser, maxComputersPerUser)
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
	delete(s.asked, id)
}

// dropComputer ends everything live of a computer that was removed: its socket, its state and
// its previews. Every browser is sent the list without it.
func (s *Server) dropComputer(id string) {
	s.mu.Lock()
	host := s.hosts[id]
	s.forgetHostLocked(id)
	delete(s.previews, id)
	s.mu.Unlock()
	if host != nil {
		host.cancel()
	}
	s.broadcastDevices()
}

// deviceList is the computers of browser c's account with what the live connections add:
// online, who holds them (as this browser sees it), their displays and their current hello.
func (s *Server) deviceList(c *conn) []proto.Device {
	s.mu.Lock()
	list := s.store.DevicesFor(c.user)
	holders := make([]*proto.Holder, len(list))
	for i := range list {
		d := &list[i]
		if h := s.hosts[d.ID]; h != nil {
			d.Online, d.Version, d.Caps = true, h.version, h.caps
			holders[i] = h.holder
			if !c.ai {
				d.KeptTerminal = h.keptTerminal
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
			name := s.store.BrowserName(c.user, kept.Cred)
			if name == "" {
				name = kept.Name
			}
			list[i].KeptTerminal = &proto.KeptTerminal{Since: kept.Since, Name: name}
		}
	}
	return list
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
	s.mu.Lock()
	clients := make([]*conn, 0, len(s.clients))
	for c := range s.clients {
		clients = append(clients, c)
	}
	s.mu.Unlock()
	for _, c := range clients {
		c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
	}
}
