package server

import (
	"log"
	"net/http"
	"strings"
)

// Members: the admin makes, resets and removes the other accounts (docs/accounts.md). A new or
// reset password is generated here and answered once; only its hash is kept.

// requireAdmin lets only the admin's signed-in browsers through.
func (s *Server) requireAdmin(next http.HandlerFunc) http.HandlerFunc {
	return s.requireLogin(func(w http.ResponseWriter, r *http.Request) {
		if !s.store.IsAdmin(userOf(r)) {
			http.Error(w, "admin access required", http.StatusForbidden)
			return
		}
		next(w, r)
	})
}

// handleMembers lists the members: names, when made, when last active, how many computers.
func (s *Server) handleMembers(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, s.store.Members())
}

// handleMemberCreate makes a member with a sign-in name and a display name (the sign-in name
// when empty) and answers its initial password.
func (s *Server) handleMemberCreate(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name    string `json:"name"`
		Display string `json:"display"`
	}
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	name, display := strings.TrimSpace(body.Name), strings.TrimSpace(body.Display)
	if display == "" {
		display = name
	}
	if len([]rune(display)) > 20 {
		http.Error(w, "invalid name", http.StatusBadRequest)
		return
	}
	id, password, err := s.store.CreateMember(name, display)
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: the admin made member %s (%q)", id, name)
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, map[string]string{"id": id, "name": name, "password": password})
}

// handleMemberReset gives a member a new initial password and answers it.
func (s *Server) handleMemberReset(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	password, err := s.store.ResetPassword(id)
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: the admin reset the password of member %s", id)
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, map[string]string{"name": s.store.Name(id), "password": password})
}

// handleMemberRemove removes a member with its browsers and computers: its pages are signed
// out at once, its sessions end with its computers' sockets, and its computers, finding their
// token gone when they connect again, know they were removed.
func (s *Server) handleMemberRemove(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	browsers, computers, err := s.store.RemoveMember(id)
	if err != nil {
		storeError(w, err)
		return
	}
	s.dropBrowsers(browsers)
	for c := range computers {
		s.dropComputer(c)
	}
	log.Printf("server: the admin removed member %s with %d browsers and %d computers", id, len(browsers), len(computers))
	w.WriteHeader(http.StatusNoContent)
}
