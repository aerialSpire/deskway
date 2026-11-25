package server

import (
	"log"
	"net/http"
	"sync"
	"time"

	"deskway/internal/proto"
)

// AI clients join without the account password (docs/architecture.md, "Identity"). A
// signed-in browser opens the account to new AI sign-ins for aiWindowTTL: the Add an AI client
// page does when its prompt is copied. A request in that time waits for a browser's approval like
// any new device; one at any other time is refused, so nobody can fill the list with requests.
// An approved AI client signs in again with its own key, at any time.

const (
	aiWindowTTL = 10 * time.Minute
	// How far an AI client's signed time may be from the server's.
	aiSigSkew = 5 * 60
)

// aiState is what the server keeps in memory for AI sign-ins: until when each account takes
// new ones and the latest signed time each AI client used (a signature is good once).
type aiState struct {
	mu   sync.Mutex
	open map[string]time.Time
	last map[string]int64
}

func newAIState() *aiState {
	return &aiState{open: map[string]time.Time{}, last: map[string]int64{}}
}

// handleAIWindow: GET says until when the account takes new AI sign-ins (0: not now); POST
// opens it for aiWindowTTL from now.
func (s *Server) handleAIWindow(w http.ResponseWriter, r *http.Request) {
	user := userOf(r)
	now := time.Now()
	s.ai.mu.Lock()
	if r.Method == http.MethodPost {
		s.ai.open[user] = now.Add(aiWindowTTL)
	}
	until := s.ai.open[user]
	s.ai.mu.Unlock()
	var at int64
	if now.Before(until) {
		at = until.Unix()
	}
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, map[string]int64{"until": at})
}

func (s *Server) aiOpen(user string) bool {
	s.ai.mu.Lock()
	defer s.ai.mu.Unlock()
	return time.Now().Before(s.ai.open[user])
}

// handleAILogin signs an AI client's deskway in, by account name only. Approved before: with
// its key's signature over the time (proto.AILogin), any time. New: only while the account
// takes AI sign-ins, and then it waits for a browser's approval, with the code to compare. A
// renewal (`renew`, the proxy when its session ended) never asks for approval: only a person
// running `deskway login` does.
func (s *Server) handleAILogin(w http.ResponseWriter, r *http.Request) {
	var body struct {
		User  string `json:"user"`
		TS    int64  `json:"ts"`
		Sig   string `json:"sig"`
		Renew bool   `json:"renew"`
	}
	if !decodeBody(w, r, 4096, &body) {
		return
	}
	user := s.store.ID(body.User)
	id := browserFromCookie(r)
	now := time.Now().Unix()
	key, approved, asked := s.store.AIClient(id, user)
	if approved {
		if !s.attempt(w, r, http.StatusUnauthorized, "bad AI client signature", func() bool { return s.signedNow(id, key, body.User, body.TS, body.Sig, now) }) {
			return
		}
		if _, err := s.store.LoginBrowser(id, user, proto.RoleAI, now); err != nil {
			storeError(w, err)
			return
		}
		s.issueSession(w, r, id)
		w.WriteHeader(http.StatusNoContent)
		return
	}
	if body.Renew {
		http.Error(w, "not an approved AI client here", http.StatusUnauthorized)
		return
	}
	// The same answer for an account that does not exist: names are not told apart.
	if user == "" || !s.aiOpen(user) {
		log.Printf("server: AI sign-in refused, not taking them now, from %s", clientIP(r))
		http.Error(w, "not taking AI sign-ins now", http.StatusForbidden)
		return
	}
	token := ""
	if !asked {
		token = randomHex(32)
		id = hashToken(token)
	}
	s.askApproval(w, r, id, token, user, proto.RoleAI, now)
}

// signedNow checks an approved AI client's signature over the time, near the server's and
// later than any it used before.
func (s *Server) signedNow(id string, key *proto.BrowserKey, name string, ts int64, sig string, now int64) bool {
	if key == nil || ts < now-aiSigSkew || ts > now+aiSigSkew {
		return false
	}
	s.ai.mu.Lock()
	used := s.ai.last[id]
	s.ai.mu.Unlock()
	if ts <= used || !key.Verify(proto.AILogin(name, ts), sig) {
		return false
	}
	s.ai.mu.Lock()
	if ts > s.ai.last[id] {
		s.ai.last[id] = ts
	}
	s.ai.mu.Unlock()
	return true
}
