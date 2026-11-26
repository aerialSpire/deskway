package server

import (
	"context"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const (
	mcpResourcePath     = "/mcp"
	mcpResourceMetadata = "/.well-known/oauth-protected-resource/mcp"
	mcpIssuerMetadata   = "/.well-known/oauth-authorization-server"
	mcpRegisterPath     = "/oauth/register"
	mcpAuthorizePath    = "/oauth/authorize"
	mcpTokenPath        = "/oauth/token"
	mcpApprovalPath     = "/api/mcp/approval"
)

type mcpBackend interface {
	close()
	stop(string)
	forward(http.ResponseWriter, *http.Request, string, func() bool)
}

// StopMCP drains the processes before HTTP shutdown waits for active calls.
func (s *Server) StopMCP() {
	if s.mcp != nil {
		s.mcp.gateways.close()
	}
}

// mcpProfileRoot holds one directory per remote application.
func (s *Server) mcpProfileRoot() string {
	path, _ := filepath.Abs(filepath.Join(filepath.Dir(s.store.path), "mcp"))
	return path
}

// mcpProfilePath is the directory of profile id, or "" when id is not a profile id: the one
// place a name from a request or the state file becomes a path.
func (s *Server) mcpProfilePath(id string) string {
	if !validMCPID(id, 24) || !filepath.IsLocal(id) {
		return ""
	}
	return filepath.Join(s.mcpProfileRoot(), id)
}

func (s *Server) discardMCPProfile(id string) {
	path := s.mcpProfilePath(id)
	if path == "" || s.store.mcpHasProfile(id) {
		return
	}
	root, err := os.Lstat(s.mcpProfileRoot())
	if err != nil || !root.IsDir() || root.Mode()&os.ModeSymlink != 0 {
		return
	}
	if err := removeMCPProfileDir(path); err != nil {
		log.Print("server: cannot remove a remote MCP profile: ", err)
	}
}

func (s *Server) retireMCPProfiles(ids []string) {
	for _, id := range ids {
		if s.mcp != nil {
			s.mcp.gateways.stop(s.mcpProfilePath(id))
		}
		s.discardMCPProfile(id)
	}
}

type remoteMCP struct {
	issuer, resource string
	prepare          func(context.Context, string, string, string) (mcpPrepared, error)
	finish           func(context.Context, string) error
	gateways         mcpBackend
	metadata         *mcpMetadata
	mu               sync.Mutex
	requests         map[string]*mcpAuthorization
}

func newRemoteMCP(origin string) (*remoteMCP, error) {
	parsed, err := url.Parse(origin)
	if err != nil || parsed.Scheme != "https" || parsed.Hostname() == "" || parsed.User != nil ||
		strings.ContainsAny(origin, "?#\\") || (parsed.Path != "" && parsed.Path != "/") || parsed.RawPath != "" {
		return nil, errors.New("set -mcp-public-url to the public HTTPS origin (for example https://deskway.example)")
	}
	host := strings.ToLower(parsed.Hostname())
	if port := parsed.Port(); port != "" && port != "443" {
		host = net.JoinHostPort(host, port)
	} else if strings.Contains(host, ":") {
		host = "[" + host + "]"
	}
	origin = "https://" + host
	return &remoteMCP{issuer: origin, resource: origin + mcpResourcePath, requests: make(map[string]*mcpAuthorization)}, nil
}

func (s *Server) registerMCP(mux *http.ServeMux) {
	handle := func(pattern string, next http.HandlerFunc) {
		mux.HandleFunc(pattern, func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Cache-Control", "no-store")
			w.Header().Set("Referrer-Policy", "no-referrer")
			limit := 60
			if pattern == "POST "+mcpRegisterPath {
				limit = 12
			}
			if r.URL.Path == mcpResourcePath || s.mcpRate(w, clientIP(r), pattern, limit) {
				next(w, r)
			}
		})
	}
	m := s.mcp
	for _, method := range []string{"GET", "POST", "DELETE"} {
		handle(method+" "+mcpResourcePath, s.serveMCP)
	}
	prm := func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, map[string]any{"resource": m.resource, "authorization_servers": []string{m.issuer},
			"bearer_methods_supported": []string{"header"}})
	}
	handle("GET "+mcpResourceMetadata, prm)
	handle("GET /.well-known/oauth-protected-resource", prm)
	handle("GET "+mcpIssuerMetadata, func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, map[string]any{
			"issuer": m.issuer, "authorization_endpoint": m.issuer + mcpAuthorizePath,
			"token_endpoint": m.issuer + mcpTokenPath, "registration_endpoint": m.issuer + mcpRegisterPath,
			"response_types_supported": []string{"code"}, "grant_types_supported": []string{"authorization_code", "refresh_token"},
			"code_challenge_methods_supported": []string{"S256"}, "token_endpoint_auth_methods_supported": mcpAuthMethods,
			"authorization_response_iss_parameter_supported": true, "client_id_metadata_document_supported": true,
		})
	})
	handle("POST "+mcpRegisterPath, s.registerMCPClient)
	handle("GET "+mcpAuthorizePath, s.authorizeMCP)
	handle("POST "+mcpTokenPath, s.tokenMCP)
	handle("GET "+mcpApprovalPath, s.mcpApproval)
	handle("POST "+mcpApprovalPath, s.mcpApproval)
}

func (s *Server) mcpRate(w http.ResponseWriter, key, endpoint string, limit int) bool {
	if s.login.allow(key+" "+endpoint, limit) {
		return true
	}
	w.Header().Set("Retry-After", "60")
	mcpError(w, http.StatusTooManyRequests, "temporarily_unavailable")
	return false
}

func mcpError(w http.ResponseWriter, status int, code string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = io.WriteString(w, `{"error":"`+code+`"}`)
}

func (s *Server) serveMCP(w http.ResponseWriter, r *http.Request) {
	values := r.Header.Values("Authorization")
	var token string
	if len(values) == 1 {
		method, value, _ := strings.Cut(values[0], " ")
		if strings.EqualFold(method, "Bearer") {
			token = strings.TrimLeft(value, " ")
		}
	}
	profile := s.store.mcpAccess(token, s.mcp.resource, time.Now())
	key := clientIP(r)
	if profile != "" {
		key = profile
	}
	if !s.mcpRate(w, key, mcpResourcePath, 240) {
		return
	}
	if profile == "" {
		s.mcpChallenge(w, len(values) != 0)
		return
	}
	if origins := r.Header.Values("Origin"); len(origins) != 0 && (len(origins) != 1 || origins[0] != s.mcp.issuer) {
		w.WriteHeader(http.StatusForbidden)
		return
	}
	s.mcp.gateways.forward(w, r, s.mcpProfilePath(profile), func() bool {
		if s.store.mcpAccess(token, s.mcp.resource, time.Now()) != "" {
			return true
		}
		s.mcpChallenge(w, true)
		return false
	})
}

func (s *Server) mcpChallenge(w http.ResponseWriter, invalid bool) {
	challenge := `Bearer resource_metadata="` + s.mcp.issuer + mcpResourceMetadata + `"`
	if invalid {
		challenge += `, error="invalid_token"`
	}
	w.Header().Set("WWW-Authenticate", challenge)
	w.WriteHeader(http.StatusUnauthorized)
}
