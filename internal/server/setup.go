package server

import (
	"errors"
	"log"
	"mime"
	"net/http"
	"strings"
)

func (s *Server) handleSetupDetails(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	// Only the digest is needed to look up the link's details. Redemption requires the code.
	details, err := s.store.setupDetails(r.URL.Query().Get("c"))
	if errors.Is(err, errAccountLink) {
		http.Error(w, err.Error(), http.StatusGone)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	writeJSON(w, details)
}

func (s *Server) handleSetup(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	if !sameOriginJSON(w, r) {
		return
	}
	var body setupRequest
	if !decodeBody(w, r, 4096, &body) {
		return
	}
	body.Name = strings.TrimSpace(body.Name)
	body.Display = strings.TrimSpace(body.Display)
	var user, browser, token string
	var err error
	if !s.attempt(w, r, http.StatusGone, errAccountLink.Error(), func() bool {
		user, browser, token, err = s.store.redeemLink(body, browserFromCookie(r), browserLabel(r.UserAgent()), clientIP(r))
		return !errors.Is(err, errAccountLink)
	}) {
		return
	}
	if errors.Is(err, errSetupKey) {
		http.Error(w, err.Error(), http.StatusUnprocessableEntity)
		return
	}
	if errors.Is(err, errLoginFields) {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: browser approved with an account link for %s from %s", user, clientIP(r))
	s.sendDeviceLog(user)
	if token != "" {
		s.setBrowserCookie(w, r, token)
	}
	s.issueSession(w, r, browser)
	s.sendApprovals(user)
	if body.Display != "" {
		s.sendAccount(user)
	}
	w.WriteHeader(http.StatusNoContent)
}

// Requiring application/json forces cross-origin browser POSTs through a CORS preflight, which this server never approves.
func sameOriginJSON(w http.ResponseWriter, r *http.Request) bool {
	for _, site := range r.Header.Values("Sec-Fetch-Site") {
		if site != "same-origin" && site != "none" {
			http.Error(w, "same-origin request required", http.StatusForbidden)
			return false
		}
	}
	if content, _, _ := mime.ParseMediaType(r.Header.Get("Content-Type")); content != "application/json" {
		http.Error(w, "application/json required", http.StatusUnsupportedMediaType)
		return false
	}
	return true
}
