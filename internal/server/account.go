package server

import (
	"log"
	"net/http"
	"strings"
)

// The account page: the names and the password. A change to a name or a
// password asks for the current password even though the browser is signed in, so a borrowed
// unlocked browser cannot take the account over.

// me is what a browser is told about its account: the name it signs in with, the display name,
// and whether it is the admin (who manages the members, members.go).
func (s *Server) me(user string) map[string]any {
	me := map[string]any{"user": s.store.Name(user), "display": s.store.Display(user), "admin": s.store.IsAdmin(user)}
	me["local_mode"] = s.local != nil
	if s.local != nil {
		me["local_ca_fingerprint"] = s.local.Fingerprint()
	}
	if s.mcp != nil {
		me["remote_mcp"] = s.mcp.resource
	}
	return me
}

func (s *Server) handleMe(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, s.me(userOf(r)))
}

// handleAccountName changes what the account signs in with, after its password.
func (s *Server) handleAccountName(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Name     string `json:"name"`
		Password string `json:"password"`
	}
	if !decodeBody(w, r, 1024, &body) || !s.checkCurrent(w, r, user, body.Password) {
		return
	}
	name := strings.TrimSpace(body.Name)
	if err := s.store.Rename(user, name); err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: account %s now signs in as %q", user, name)
	s.sendAccount(user)
	writeJSON(w, s.me(user))
}

// handleDisplay changes the name others see.
func (s *Server) handleDisplay(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Display string `json:"display"`
	}
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	name := strings.TrimSpace(body.Display)
	if name == "" || len([]rune(name)) > 20 {
		http.Error(w, "invalid name", http.StatusBadRequest)
		return
	}
	if err := s.store.SetDisplay(user, name); err != nil {
		storeError(w, err)
		return
	}
	s.sendAccount(user)
	writeJSON(w, s.me(user))
}

// handlePassword changes the account password. Computers keep working: they sign in with their
// own tokens. Optionally every other browser is signed out at once.
func (s *Server) handlePassword(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	var body struct {
		Current string `json:"current"`
		New     string `json:"new"`
		Others  bool   `json:"sign_out_others"`
	}
	if !decodeBody(w, r, 4096, &body) || !s.checkCurrent(w, r, user, body.Current) {
		return
	}
	if err := s.store.SetPassword(user, body.New); err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: %s changed the password from %s", user, clientIP(r))
	if body.Others {
		removed, profiles, err := s.store.RemoveBrowsersExcept(user, browserOf(r))
		s.dropBrowsers(removed, profiles)
		if err != nil {
			storeError(w, err)
			return
		}
		log.Printf("server: %s signed out %d other browsers", user, len(removed))
	}
	w.WriteHeader(http.StatusNoContent)
}
