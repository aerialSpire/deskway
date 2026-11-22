package server

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"math/big"
	"net"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"deskway/internal/proto"
)

// Signing in: a session cookie for an approved browser, a long-lived cookie that names the
// browser, and a failure limiter in front of every password check.

const (
	sessionCookie    = "deskway_session"
	sessionTTL       = 30 * 24 * time.Hour
	browserCookie    = "deskway_browser"
	browserCookieTTL = 365 * 24 * time.Hour
)

type userKey struct{}

type browserKey struct{}

// userOf is the signed-in account of a request that passed requireLogin.
func userOf(r *http.Request) string { return r.Context().Value(userKey{}).(string) }

// browserOf is the signed-in browser record of a request that passed requireLogin.
func browserOf(r *http.Request) string { return r.Context().Value(browserKey{}).(string) }

func isHTTPS(r *http.Request) bool {
	return r.TLS != nil || r.Header.Get("X-Forwarded-Proto") == "https"
}

func clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func (s *Server) sign(browser string, expiry int64) string {
	mac := hmac.New(sha256.New, []byte(s.store.CookieKey))
	fmt.Fprintf(mac, "%s|%d", browser, expiry)
	return hex.EncodeToString(mac.Sum(nil))
}

// sessionOf returns the account and browser behind a session cookie. The session is tied to an
// approved browser record, so removing that browser ends its login immediately.
func (s *Server) sessionOf(r *http.Request) (user, browser string) {
	c, err := r.Cookie(sessionCookie)
	if err != nil {
		return "", ""
	}
	parts := strings.Split(c.Value, ".")
	if len(parts) != 3 {
		return "", ""
	}
	expiry, err := strconv.ParseInt(parts[1], 10, 64)
	if err != nil || time.Now().Unix() > expiry {
		return "", ""
	}
	if !hmac.Equal([]byte(parts[2]), []byte(s.sign(parts[0], expiry))) {
		return "", ""
	}
	owner := s.store.SignedInOwner(parts[0])
	if owner == "" {
		return "", ""
	}
	return owner, parts[0]
}

func (s *Server) issueSession(w http.ResponseWriter, r *http.Request, browser string) {
	expiry := time.Now().Add(sessionTTL).Unix()
	http.SetCookie(w, &http.Cookie{
		Name:     sessionCookie,
		Value:    fmt.Sprintf("%s.%d.%s", browser, expiry, s.sign(browser, expiry)),
		Path:     "/",
		MaxAge:   int(sessionTTL.Seconds()),
		HttpOnly: true,
		Secure:   isHTTPS(r),
		SameSite: http.SameSiteStrictMode,
	})
}

// requireLogin lets only signed-in browsers through; userOf and browserOf name them. An account
// whose password is an initial one gets nothing but mustChoose until it has chosen its own.
// An AI client's login is refused: it approves, removes and changes nothing.
func (s *Server) requireLogin(next http.HandlerFunc) http.HandlerFunc {
	return s.signedIn(next, false, false)
}

// requireClient is requireLogin for what an AI client's login may use too: who it is, the
// account's devices, and the signaling socket (docs/architecture.md, "Rules the server
// enforces").
func (s *Server) requireClient(next http.HandlerFunc) http.HandlerFunc {
	return s.signedIn(next, false, true)
}

// requireInitial is requireLogin for the one request an account with an initial password
// makes: choosing its own (handleInitial).
func (s *Server) requireInitial(next http.HandlerFunc) http.HandlerFunc {
	return s.signedIn(next, true, false)
}

func (s *Server) signedIn(next http.HandlerFunc, choosing, ai bool) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		user, browser := s.sessionOf(r)
		if user == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if !ai && s.store.BrowserRole(browser) == proto.RoleAI {
			http.Error(w, "not for an AI client", http.StatusForbidden)
			return
		}
		must := s.store.MustChange(user)
		if must && !choosing {
			s.mustChoose(w, user)
			return
		}
		if !must && choosing {
			http.Error(w, "the password is the account's own already", http.StatusConflict)
			return
		}
		ctx := context.WithValue(context.WithValue(r.Context(), userKey{}, user), browserKey{}, browser)
		next(w, r.WithContext(ctx))
	}
}

// mustChoose answers a request of an account whose password is an initial one: 428, and
// whether it must choose its super password too (it has none yet). The page shows the view
// that chooses them (docs/accounts.md, First sign-in).
func (s *Server) mustChoose(w http.ResponseWriter, user string) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(http.StatusPreconditionRequired)
	_ = json.NewEncoder(w).Encode(map[string]bool{"super": !s.store.HasSuper(user)})
}

func (s *Server) setBrowserCookie(w http.ResponseWriter, r *http.Request, token string) {
	http.SetCookie(w, &http.Cookie{
		Name:     browserCookie,
		Value:    token,
		Path:     "/",
		MaxAge:   int(browserCookieTTL.Seconds()),
		HttpOnly: true,
		Secure:   isHTTPS(r),
		SameSite: http.SameSiteStrictMode,
	})
}

// browserFromCookie returns the record ID for this browser's cookie, or "".
func browserFromCookie(r *http.Request) string {
	c, err := r.Cookie(browserCookie)
	if err != nil || c.Value == "" {
		return ""
	}
	return hashToken(c.Value)
}

// ---- password checks ----

// attempt runs one password check for the request's address under the failure limiter. A
// blocked address gets 429 without a check. A failed check counts toward the lockout and is
// answered with status and msg; a passed one clears the address's failures.
func (s *Server) attempt(w http.ResponseWriter, r *http.Request, status int, msg string, check func() bool) bool {
	ip := clientIP(r)
	if wait := s.login.blocked(ip); wait > 0 {
		w.Header().Set("Retry-After", strconv.Itoa(int(wait.Seconds())+1))
		http.Error(w, "too many attempts", http.StatusTooManyRequests)
		return false
	}
	if !check() {
		s.login.fail(ip)
		log.Printf("server: %s from %s", msg, ip)
		http.Error(w, msg, status)
		return false
	}
	s.login.reset(ip)
	return true
}

// checkCredentials checks a sign-in name and password and returns the account's id, or ""
// after writing the error.
func (s *Server) checkCredentials(w http.ResponseWriter, r *http.Request, name, password string) string {
	user := ""
	s.attempt(w, r, http.StatusUnauthorized, "wrong account or password", func() bool {
		user = s.store.SignIn(name, password)
		return user != ""
	})
	return user
}

// checkCurrent checks the password of a signed-in account (by its id) before a change. A wrong
// one is refused with 403, since 401 would mean the session itself ended.
func (s *Server) checkCurrent(w http.ResponseWriter, r *http.Request, user, password string) bool {
	return s.attempt(w, r, http.StatusForbidden, "wrong password", func() bool {
		return s.store.PasswordOK(user, password)
	})
}

// loginLimiter applies a doubling lockout per source address after repeated failures.
type loginLimiter struct {
	mu    sync.Mutex
	state map[string]*loginState
}

type loginState struct {
	fails int
	until time.Time
}

func newLoginLimiter() *loginLimiter { return &loginLimiter{state: map[string]*loginState{}} }

func (l *loginLimiter) blocked(ip string) time.Duration {
	l.mu.Lock()
	defer l.mu.Unlock()
	if st := l.state[ip]; st != nil {
		return time.Until(st.until)
	}
	return 0
}

func (l *loginLimiter) fail(ip string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.state) > 10000 {
		for k, st := range l.state {
			if time.Since(st.until) > time.Hour {
				delete(l.state, k)
			}
		}
	}
	st := l.state[ip]
	if st == nil {
		st = &loginState{}
		l.state[ip] = st
	}
	st.fails++
	if st.fails >= 5 {
		st.until = time.Now().Add(time.Minute << min(st.fails-5, 6))
	}
}

func (l *loginLimiter) reset(ip string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.state, ip)
}

// ---- signing in and out ----

func compareCode() string {
	n, err := rand.Int(rand.Reader, big.NewInt(1_000_000))
	if err != nil {
		panic(err)
	}
	return fmt.Sprintf("%06d", n)
}

// browserLabel is for display only: it helps a person recognize their own browser.
func browserLabel(ua string) string {
	osName := "Unknown"
	for _, c := range []struct{ needle, name string }{
		{"iPhone", "iPhone"}, {"iPad", "iPad"}, {"Android", "Android"},
		{"Windows", "Windows"}, {"Mac OS", "Mac"}, {"Linux", "Linux"},
	} {
		if strings.Contains(ua, c.needle) {
			osName = c.name
			break
		}
	}
	browser := "Browser"
	for _, c := range []struct{ needle, name string }{
		{"deskway/", "deskway"}, {"Edg/", "Edge"}, {"Firefox/", "Firefox"}, {"Chrome/", "Chrome"}, {"CriOS/", "Chrome"}, {"Safari/", "Safari"},
	} {
		if strings.Contains(ua, c.needle) {
			browser = c.name
			break
		}
	}
	return browser + " · " + osName
}

// handleLogin signs a browser in with the account password. An approved browser gets its
// session at once; so does the account's first browser, signing in with its initial password
// (docs/accounts.md, First sign-in); with the super password a new one is approved on the spot;
// any other waits for approval (browsers.go) and gets the code to compare. An AI client's
// deskway signs in without the password (aiclients.go).
func (s *Server) handleLogin(w http.ResponseWriter, r *http.Request) {
	var body struct {
		User     string `json:"user"`
		Password string `json:"password"`
		Super    string `json:"super_password"`
	}
	if !decodeBody(w, r, 4096, &body) {
		return
	}
	user := s.checkCredentials(w, r, body.User, body.Password)
	if user == "" {
		return
	}
	id := browserFromCookie(r)
	now := time.Now().Unix()

	known, err := s.store.LoginBrowser(id, user, "", now)
	if err != nil {
		storeError(w, err)
		return
	}
	if known == LoginApproved {
		s.issueSession(w, r, id)
		w.WriteHeader(http.StatusNoContent)
		return
	}

	token := ""
	if known == LoginNew {
		token = randomHex(32)
		id = hashToken(token)
	}
	// Let in on the spot: the browser joins the account, gets its cookie and its session.
	letIn := func(how string) {
		log.Printf("server: browser approved %s for %s from %s", how, user, clientIP(r))
		s.sendDeviceLog(user)
		if token != "" {
			s.setBrowserCookie(w, r, token)
		}
		s.issueSession(w, r, id)
		s.sendApprovals(user)
		w.WriteHeader(http.StatusNoContent)
	}
	first, err := s.store.ApproveFirst(id, user, browserLabel(r.UserAgent()), clientIP(r), now)
	if err != nil {
		storeError(w, err)
		return
	}
	if first {
		letIn("as the account's first, with its initial password")
		return
	}
	if body.Super != "" {
		if !s.attempt(w, r, http.StatusForbidden, "wrong super password", func() bool { return s.store.CheckSuper(user, body.Super) }) {
			return
		}
		if err := s.store.ApproveWithSuper(id, user, browserLabel(r.UserAgent()), clientIP(r), now); err != nil {
			storeError(w, err)
			return
		}
		letIn("with the super password")
		return
	}

	s.askApproval(w, r, id, token, user, "", now)
}

// askApproval puts the browser (an AI client's, for RoleAI) on the account's list to approve
// and answers 202 with the code to compare; token is a new browser's cookie, "" for one that
// has asked before.
func (s *Server) askApproval(w http.ResponseWriter, r *http.Request, id, token, user, role string, now int64) {
	code, err := s.store.AskApproval(id, user, role, browserLabel(r.UserAgent()), clientIP(r), compareCode(), now, maxPendingPerUser)
	if err != nil {
		storeError(w, err)
		return
	}
	log.Printf("server: browser waiting for approval for %s from %s", user, clientIP(r))
	if token != "" {
		s.setBrowserCookie(w, r, token)
	}
	s.sendApprovals(user)
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_ = json.NewEncoder(w).Encode(map[string]string{"code": code})
}

// handleLoginStatus lets a waiting browser collect its session once, right after approval.
// An approved browser cannot use this later: its next session needs the password again.
func (s *Server) handleLoginStatus(w http.ResponseWriter, r *http.Request) {
	id := browserFromCookie(r)
	w.Header().Set("Cache-Control", "no-store")
	p, err := s.store.CollectApproval(id, pendingBrowserTTL)
	switch {
	case err != nil:
		storeError(w, err)
	case p == PickupWaiting:
		w.WriteHeader(http.StatusAccepted)
	case p == PickupReady:
		s.issueSession(w, r, id)
		w.WriteHeader(http.StatusNoContent)
	default:
		w.WriteHeader(http.StatusGone)
	}
}

func (s *Server) handleLogout(w http.ResponseWriter, r *http.Request) {
	http.SetCookie(w, &http.Cookie{Name: sessionCookie, Path: "/", MaxAge: -1, HttpOnly: true, Secure: isHTTPS(r), SameSite: http.SameSiteStrictMode})
	w.WriteHeader(http.StatusNoContent)
}
