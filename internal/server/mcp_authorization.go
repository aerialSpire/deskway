package server

import (
	"encoding/base64"
	"errors"
	"mime"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"time"

	"deskway/internal/proto"
)

const (
	mcpRequestTTL    = 10 * time.Minute
	mcpMaxRequests   = 256
	mcpRequestsPerIP = 16
)

type mcpAuthorization struct {
	Client, Name, ClientHost, RedirectHost, Redirect, State, Challenge string
	Address, Browser                                                   string
	Loopback, Refresh                                                  bool
	Expires                                                            time.Time
	Prepared                                                           mcpPrepared
	timer                                                              *time.Timer
	busy                                                               bool
}

type mcpPrepared struct {
	Profile  string           `json:"-"`
	Existing bool             `json:"-"`
	ID       string           `json:"id"`
	Device   proto.BrowserKey `json:"device"`
	Endorsed int64            `json:"endorsed"`
}

func (s *Server) authorizeMCP(w http.ResponseWriter, r *http.Request) {
	if len(r.URL.RawQuery) > 8192 {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	q, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	m := s.mcp
	if code := mcpAuthorizationError(q, m.resource); code != "" {
		mcpError(w, http.StatusBadRequest, code)
		return
	}
	if len(q["client_id"]) != 1 || len(q["redirect_uri"]) != 1 {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	clientID, redirect := q.Get("client_id"), q.Get("redirect_uri")
	reg, registered := s.store.mcpClient(clientID, time.Now())
	ok := registered
	client := reg.mcpClient
	var clientHost string
	if !ok {
		if u, valid := mcpMetadataURL(clientID); valid && m.metadata != nil {
			client, ok = m.metadata.get(r.Context(), clientID)
			clientHost = u.Host
		}
	}
	if !ok {
		mcpError(w, http.StatusBadRequest, "invalid_client")
		return
	}
	callback, ok := client.allowsRedirect(redirect)
	if !ok {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	a := &mcpAuthorization{Client: clientID, Name: client.Name, ClientHost: clientHost,
		RedirectHost: callback.Host, Redirect: redirect, State: q.Get("state"), Challenge: q.Get("code_challenge"),
		Loopback: mcpLoopback(callback), Refresh: slices.Contains(client.Grants, "refresh_token"),
		Address: clientIP(r), Expires: time.Now().Add(mcpRequestTTL)}
	key := randomHex(32)
	if !s.rememberMCPRequest(key, a) {
		mcpError(w, http.StatusServiceUnavailable, "temporarily_unavailable")
		return
	}
	if registered {
		if err := s.store.mcpUseRegistration(clientID, time.Now()); err != nil {
			s.dropMCPRequest(key)
			mcpError(w, http.StatusServiceUnavailable, "temporarily_unavailable")
			return
		}
	}
	http.Redirect(w, r, "/?mcp="+key, http.StatusSeeOther)
}

func mcpAuthorizationError(q url.Values, resource string) string {
	challenge, err := base64.RawURLEncoding.Strict().DecodeString(q.Get("code_challenge"))
	if mcpDuplicate(q) || len(q.Get("state")) > 1024 || strings.ContainsAny(q.Get("state"), "\r\n") ||
		q.Get("response_type") == "" || q.Get("code_challenge_method") != "S256" ||
		len(q.Get("code_challenge")) != 43 || len(challenge) != 32 || err != nil {
		return "invalid_request"
	}
	if q.Get("response_type") != "code" {
		return "unsupported_response_type"
	}
	if q.Get("resource") != resource {
		return "invalid_target"
	}
	if q.Get("scope") != "" {
		return "invalid_scope"
	}
	return ""
}

func mcpDuplicate(values url.Values) bool {
	for _, vs := range values {
		if len(vs) != 1 {
			return true
		}
	}
	return false
}

func mcpPKCE(s string) bool {
	if len(s) < 43 || len(s) > 128 {
		return false
	}
	const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~"
	return !strings.ContainsFunc(s, func(c rune) bool { return !strings.ContainsRune(alphabet, c) })
}

func (m *remoteMCP) callback(a *mcpAuthorization, name, value string) string {
	values := url.Values{"iss": {m.issuer}, name: {value}}
	if a.State != "" {
		values.Set("state", a.State)
	}
	separator := "?"
	if strings.Contains(a.Redirect, "?") {
		separator = "&"
	}
	return a.Redirect + separator + values.Encode()
}

func (s *Server) rememberMCPRequest(id string, a *mcpAuthorization) bool {
	m := s.mcp
	m.mu.Lock()
	defer m.mu.Unlock()
	count := 0
	for _, req := range m.requests {
		if req.Address == a.Address {
			count++
		}
	}
	if len(m.requests) >= mcpMaxRequests || count >= mcpRequestsPerIP {
		return false
	}
	m.requests[id] = a
	a.timer = time.AfterFunc(time.Until(a.Expires), func() { s.dropMCPRequest(id) })
	return true
}

func (s *Server) dropMCPRequest(id string) {
	m := s.mcp
	m.mu.Lock()
	a := m.requests[id]
	if a == nil || a.busy {
		m.mu.Unlock()
		return
	}
	delete(m.requests, id)
	a.timer.Stop()
	d := a.Prepared
	m.mu.Unlock()
	if !d.Existing {
		s.discardMCPProfile(d.Profile)
	}
}

func (s *Server) mcpApproval(w http.ResponseWriter, r *http.Request) {
	m := s.mcp
	user, browser := s.sessionOf(r)
	trusted := s.store.mcpCanApprove(user, browser)
	if r.Method == http.MethodGet {
		m.mu.Lock()
		a := m.requests[r.URL.Query().Get("request")]
		if a == nil || !time.Now().Before(a.Expires) || a.Browser != "" && a.Browser != browser {
			m.mu.Unlock()
			w.WriteHeader(http.StatusNotFound)
			return
		}
		if trusted && a.Browser == "" {
			a.Browser = browser
		}
		name, callback, client, loopback := a.Name, a.RedirectHost, a.ClientHost, a.Loopback
		m.mu.Unlock()
		writeJSON(w, map[string]any{"name": name, "redirectHost": callback, "clientHost": client, "loopback": loopback,
			"canApprove": trusted})
		return
	}
	if !trusted || r.Header.Get("Origin") != m.issuer {
		w.WriteHeader(http.StatusForbidden)
		return
	}
	var body struct {
		Request, Action string
		Proof           *proto.Proof
	}
	deadline := http.NewResponseController(w)
	_ = deadline.SetReadDeadline(time.Now().Add(5 * time.Second))
	valid := decodeBody(w, r, 16<<10, &body)
	_ = deadline.SetReadDeadline(time.Time{})
	if !valid {
		return
	}
	m.mu.Lock()
	a := m.requests[body.Request]
	if a == nil || !time.Now().Before(a.Expires) || a.Browser != browser {
		m.mu.Unlock()
		mcpError(w, http.StatusNotFound, "restart_required")
		return
	}
	if a.busy {
		m.mu.Unlock()
		w.WriteHeader(http.StatusConflict)
		return
	}
	a.busy = true
	m.mu.Unlock()
	complete := false
	defer func() {
		m.mu.Lock()
		a.busy = false
		remove := complete || !time.Now().Before(a.Expires)
		d := a.Prepared
		if remove {
			a.timer.Stop()
			delete(m.requests, body.Request)
		}
		m.mu.Unlock()
		if remove && !d.Existing {
			s.store.mcpDiscardPending(d.Profile, time.Now())
			s.discardMCPProfile(d.Profile)
		}
	}()
	switch body.Action {
	case "cancel":
		complete = true
		writeJSON(w, map[string]string{"redirect": m.callback(a, "error", "access_denied")})
	case "prepare":
		if a.Prepared.Profile == "" {
			d, err := s.store.mcpExisting(user, a.Client, m.resource)
			if err == nil && d.Profile == "" {
				name := []rune(a.Name)
				d, err = m.prepare(r.Context(), randomHex(12), s.store.Name(user), string(name[:min(60, len(name))]))
			}
			if err != nil {
				complete = true
				mcpApprovalError(w, err)
				return
			}
			a.Prepared = d
		}
		writeJSON(w, map[string]any{"device": a.Prepared.Device, "endorsed": a.Prepared.Endorsed})
	case "approve":
		complete = true
		d := a.Prepared
		if d.Profile == "" {
			mcpApprovalError(w, errMCPEndorse)
			return
		}
		if err := s.store.mcpEndorse(user, browser, m.resource, a, d, body.Proof, time.Now()); err != nil {
			mcpApprovalError(w, err)
			return
		}
		if !d.Existing {
			if err := m.finish(r.Context(), d.Profile); err != nil {
				mcpApprovalError(w, err)
				return
			}
		}
		now := time.Now()
		code, pendingUntil, err := s.store.mcpIssueCode(d.Profile, user, browser, a, body.Proof.TS, now)
		if err != nil {
			mcpApprovalError(w, err)
			return
		}
		a.Prepared = mcpPrepared{} // The profile now belongs to the grant or its unredeemed codes.
		if !pendingUntil.IsZero() {
			time.AfterFunc(time.Until(pendingUntil), func() { s.expireMCPPending(d.Profile, time.Now()) })
		}
		writeJSON(w, map[string]string{"redirect": m.callback(a, "code", code)})
	default:
		w.WriteHeader(http.StatusBadRequest)
	}
}

func (s *Server) expireMCPPending(profile string, now time.Time) {
	if s.store.mcpDiscardPending(profile, now) {
		s.discardMCPProfile(profile)
	}
}

func mcpApprovalError(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, errMCPGrantLimit):
		mcpError(w, http.StatusConflict, "grant_limit")
	case errors.Is(err, errMCPFamilyLimit):
		mcpError(w, http.StatusConflict, "family_limit")
	case errors.Is(err, errMCPEndorse):
		mcpError(w, http.StatusForbidden, "restart_required")
	default:
		mcpError(w, http.StatusServiceUnavailable, "restart_required")
	}
}

func (s *Server) tokenMCP(w http.ResponseWriter, r *http.Request) {
	if content, _, _ := mime.ParseMediaType(r.Header.Get("Content-Type")); content != "application/x-www-form-urlencoded" {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	deadline := http.NewResponseController(w)
	_ = deadline.SetReadDeadline(time.Now().Add(5 * time.Second))
	r.Body = http.MaxBytesReader(w, r.Body, 16<<10)
	err := r.ParseForm()
	_ = deadline.SetReadDeadline(time.Time{})
	if err != nil || mcpDuplicate(r.PostForm) || r.URL.RawQuery != "" {
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	}
	q := r.PostForm
	now := time.Now()
	id, method, secret := q.Get("client_id"), "none", q.Get("client_secret")
	authHeaders := r.Header.Values("Authorization")
	if len(authHeaders) != 0 {
		client, encoded, ok := r.BasicAuth()
		if !ok || len(authHeaders) != 1 || q.Has("client_secret") || q.Has("client_id") {
			mcpError(w, http.StatusBadRequest, "invalid_request")
			return
		}
		id, err = url.QueryUnescape(client)
		var secretErr error
		secret, secretErr = url.QueryUnescape(encoded)
		if err != nil || secretErr != nil {
			mcpError(w, http.StatusBadRequest, "invalid_request")
			return
		}
		method = "client_secret_basic"
	} else if q.Has("client_secret") {
		method = "client_secret_post"
	}
	reg, registered := s.store.mcpClient(id, now)
	_, document := mcpMetadataURL(id)
	if registered && (reg.Method != method || method != "none" && !mcpSecretOK(reg.SecretHash, secret)) ||
		!registered && (!document || method != "none") {
		status := http.StatusBadRequest
		if len(authHeaders) != 0 {
			w.Header().Set("WWW-Authenticate", `Basic realm="Deskway OAuth"`)
			status = http.StatusUnauthorized
		}
		mcpError(w, status, "invalid_client")
		return
	}
	if q.Get("resource") != s.mcp.resource {
		mcpError(w, http.StatusBadRequest, "invalid_target")
		return
	}
	if q.Get("scope") != "" {
		mcpError(w, http.StatusBadRequest, "invalid_scope")
		return
	}
	x := mcpExchange{Client: id, Resource: q.Get("resource")}
	switch q.Get("grant_type") {
	case "":
		mcpError(w, http.StatusBadRequest, "invalid_request")
		return
	case "authorization_code":
		x.Token, x.Redirect, x.Verifier = q.Get("code"), q.Get("redirect_uri"), q.Get("code_verifier")
		if x.Token == "" || x.Redirect == "" || !mcpPKCE(x.Verifier) {
			mcpError(w, http.StatusBadRequest, "invalid_request")
			return
		}
	case "refresh_token":
		x.Token, x.Refresh = q.Get("refresh_token"), true
		if x.Token == "" {
			mcpError(w, http.StatusBadRequest, "invalid_request")
			return
		}
	default:
		mcpError(w, http.StatusBadRequest, "unsupported_grant_type")
		return
	}
	result, user, err := s.store.mcpExchange(x, now)
	if errors.Is(err, errMCPInvalidGrant) {
		mcpError(w, http.StatusBadRequest, "invalid_grant")
	} else if err != nil {
		s.discardMCPProfile(mcpTokenProfile(x.Token))
		mcpError(w, http.StatusInternalServerError, "server_error")
	} else {
		if user != "" {
			s.sendDeviceLog(user)
		}
		w.Header().Set("Pragma", "no-cache")
		writeJSON(w, result)
	}
}
