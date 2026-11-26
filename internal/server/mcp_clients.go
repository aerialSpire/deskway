package server

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"mime"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"
)

const mcpMetadataLimit = 5 << 10

var mcpAuthMethods = []string{"none", "client_secret_basic", "client_secret_post"}

type mcpClient struct {
	Name      string   `json:"client_name"`
	Redirects []string `json:"redirect_uris"`
	Method    string   `json:"token_endpoint_auth_method"`
	Grants    []string `json:"grant_types"`
	Responses []string `json:"response_types"`
}

func (c *mcpClient) validate(document bool) string {
	if len(c.Redirects) == 0 || len(c.Redirects) > 16 {
		return "invalid_redirect_uri"
	}
	for _, redirect := range c.Redirects {
		if _, ok := mcpRedirectURI(redirect); !ok {
			return "invalid_redirect_uri"
		}
	}
	if c.Name == "" {
		if document {
			return "invalid_client_metadata"
		}
		c.Name = "MCP application"
	}
	if len(c.Name) > 120 || !utf8.ValidString(c.Name) || strings.TrimSpace(c.Name) == "" ||
		strings.ContainsFunc(c.Name, func(r rune) bool { return unicode.IsControl(r) || unicode.Is(unicode.Cf, r) }) {
		return "invalid_client_metadata"
	}
	if c.Method == "" {
		c.Method = "client_secret_basic"
		if document {
			c.Method = "none"
		}
	}
	if !slices.Contains(mcpAuthMethods, c.Method) || (document && c.Method != "none") {
		return "invalid_client_metadata"
	}
	if c.Grants == nil {
		c.Grants = []string{"authorization_code"}
	}
	if c.Responses == nil {
		c.Responses = []string{"code"}
	}
	if !slices.Contains(c.Grants, "authorization_code") || len(c.Responses) != 1 || c.Responses[0] != "code" ||
		len(c.Grants) > 2 || len(c.Grants) == 2 && !slices.Contains(c.Grants, "refresh_token") {
		return "invalid_client_metadata"
	}
	return ""
}

func mcpRedirectURI(raw string) (*url.URL, bool) {
	u, err := url.Parse(raw)
	if err != nil || len(raw) > 2048 || u.Hostname() == "" || u.User != nil || strings.ContainsAny(raw, "#\\*") {
		return nil, false
	}
	if u.Scheme != "https" && !mcpLoopback(u) {
		return nil, false
	}
	query, err := url.ParseQuery(u.RawQuery)
	if err != nil || query.Has("code") || query.Has("error") || query.Has("state") || query.Has("iss") {
		return nil, false
	}
	return u, true
}

func mcpLoopback(u *url.URL) bool {
	return u.Scheme == "http" && (u.Hostname() == "127.0.0.1" ||
		u.Host == "[::1]" || strings.HasPrefix(u.Host, "[::1]:"))
}

func (c mcpClient) allowsRedirect(raw string) (*url.URL, bool) {
	u, ok := mcpRedirectURI(raw)
	if !ok {
		return nil, false
	}
	for _, registered := range c.Redirects {
		if raw == registered {
			return u, true
		}
		if mcpLoopback(u) {
			other, ok := mcpRedirectURI(registered)
			if ok && mcpLoopback(other) && u.Hostname() == other.Hostname() &&
				strings.TrimPrefix(raw, "http://"+u.Host) == strings.TrimPrefix(registered, "http://"+other.Host) {
				return u, true
			}
		}
	}
	return nil, false
}

func (s *Server) registerMCPClient(w http.ResponseWriter, r *http.Request) {
	var client mcpClient
	deadline := http.NewResponseController(w)
	_ = deadline.SetReadDeadline(time.Now().Add(5 * time.Second))
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 16<<10))
	_ = deadline.SetReadDeadline(time.Time{})
	if content, _, _ := mime.ParseMediaType(r.Header.Get("Content-Type")); content != "application/json" ||
		err != nil || !strings.HasPrefix(strings.TrimSpace(string(body)), "{") ||
		json.Unmarshal(body, &client) != nil {
		mcpError(w, http.StatusBadRequest, "invalid_client_metadata")
		return
	}
	if code := client.validate(false); code != "" {
		mcpError(w, http.StatusBadRequest, code)
		return
	}
	now := time.Now()
	id, secret, err := s.store.mcpRegister(client, clientIP(r), now)
	if err != nil {
		status := http.StatusInternalServerError
		if errors.Is(err, errMCPFull) {
			status = http.StatusServiceUnavailable
		}
		mcpError(w, status, "temporarily_unavailable")
		return
	}
	response := struct {
		mcpClient
		ID           string `json:"client_id"`
		Issued       int64  `json:"client_id_issued_at"`
		Secret       string `json:"client_secret,omitempty"`
		SecretExpiry *int   `json:"client_secret_expires_at,omitempty"`
	}{mcpClient: client, ID: id, Issued: now.Unix(), Secret: secret}
	if secret != "" {
		response.SecretExpiry = new(int)
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	writeJSON(w, response)
}

type mcpMetadata struct {
	http  *http.Client
	mu    sync.Mutex
	cache map[string]mcpMetadataEntry
}

type mcpMetadataEntry struct {
	client mcpClient
	until  time.Time
}

func newMCPMetadata(ownHost, publicIP string) *mcpMetadata {
	transport := &http.Transport{DisableKeepAlives: true, MaxResponseHeaderBytes: 16 << 10,
		DialContext: func(ctx context.Context, _, address string) (net.Conn, error) {
			return dialMCPMetadata(ctx, address, ownHost, publicIP)
		}}
	return &mcpMetadata{http: &http.Client{Transport: transport, Timeout: 3 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }},
		cache: make(map[string]mcpMetadataEntry)}
}

func mcpMetadataURL(id string) (*url.URL, bool) {
	u, err := url.Parse(id)
	if err != nil || len(id) > 2048 || u.Scheme != "https" || u.Hostname() == "" || u.User != nil ||
		u.Path == "" || strings.ContainsAny(id, "#\\") {
		return nil, false
	}
	for _, part := range strings.Split(u.Path, "/") {
		if part == "." || part == ".." {
			return nil, false
		}
	}
	return u, true
}

func (m *mcpMetadata) get(ctx context.Context, id string) (mcpClient, bool) {
	u, ok := mcpMetadataURL(id)
	if !ok {
		return mcpClient{}, false
	}
	m.mu.Lock()
	entry := m.cache[id]
	m.mu.Unlock()
	if time.Now().Before(entry.until) {
		return entry.client, true
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return mcpClient{}, false
	}
	req.Header.Set("Accept", "application/json")
	res, err := m.http.Do(req)
	if err != nil {
		return mcpClient{}, false
	}
	defer res.Body.Close()
	content, _, _ := mime.ParseMediaType(res.Header.Get("Content-Type"))
	if res.StatusCode != http.StatusOK || content != "application/json" || res.ContentLength > mcpMetadataLimit {
		return mcpClient{}, false
	}
	data, err := io.ReadAll(io.LimitReader(res.Body, mcpMetadataLimit+1))
	if err != nil || len(data) > mcpMetadataLimit {
		return mcpClient{}, false
	}
	var doc struct {
		mcpClient
		ID           string          `json:"client_id"`
		Secret       json.RawMessage `json:"client_secret"`
		SecretExpiry json.RawMessage `json:"client_secret_expires_at"`
	}
	if json.Unmarshal(data, &doc) != nil || doc.ID != id || doc.Secret != nil || doc.SecretExpiry != nil ||
		doc.mcpClient.validate(true) != "" {
		return mcpClient{}, false
	}
	if ttl := mcpMetadataTTL(res.Header); ttl > 0 {
		m.mu.Lock()
		now := time.Now()
		if len(m.cache) >= 256 {
			for id, old := range m.cache {
				if !now.Before(old.until) {
					delete(m.cache, id)
				}
			}
		}
		if len(m.cache) < 256 {
			m.cache[id] = mcpMetadataEntry{doc.mcpClient, now.Add(ttl)}
		}
		m.mu.Unlock()
	}
	return doc.mcpClient, true
}

func mcpMetadataTTL(h http.Header) time.Duration {
	seconds := int64(300)
	for part := range strings.SplitSeq(strings.ToLower(strings.Join(h.Values("Cache-Control"), ",")), ",") {
		name, value, _ := strings.Cut(strings.TrimSpace(part), "=")
		switch name {
		case "no-store", "no-cache":
			return 0
		case "max-age":
			if age, err := strconv.ParseInt(strings.Trim(value, `"`), 10, 64); err == nil {
				seconds = min(seconds, age)
			}
		}
	}
	if age, err := strconv.ParseInt(h.Get("Age"), 10, 64); err == nil && age >= 0 {
		seconds = max(0, seconds-age)
	}
	if expiry, err := http.ParseTime(h.Get("Expires")); err == nil {
		return min(time.Until(expiry), time.Duration(max(0, seconds))*time.Second)
	}
	return time.Duration(max(0, seconds)) * time.Second
}

// IPv4 nonglobal blocks that IP.IsGlobalUnicast does not exclude. IPv6 is
// constrained to globally routed unicast, excluding special-purpose subnets.
var mcpNonPublic = []netip.Prefix{
	netip.MustParsePrefix("0.0.0.0/8"), netip.MustParsePrefix("100.64.0.0/10"),
	netip.MustParsePrefix("192.0.0.0/24"), netip.MustParsePrefix("192.0.2.0/24"),
	netip.MustParsePrefix("192.88.99.0/24"), netip.MustParsePrefix("198.18.0.0/15"),
	netip.MustParsePrefix("198.51.100.0/24"), netip.MustParsePrefix("203.0.113.0/24"),
	netip.MustParsePrefix("240.0.0.0/4"), netip.MustParsePrefix("2001::/23"),
	netip.MustParsePrefix("2001:db8::/32"), netip.MustParsePrefix("2002::/16"),
	netip.MustParsePrefix("3fff::/20"),
}
var mcpIPv6Global = netip.MustParsePrefix("2000::/3")

func mcpPublicIP(ip netip.Addr) bool {
	ip = ip.Unmap()
	if !ip.IsGlobalUnicast() || ip.IsPrivate() || ip.Is6() && !mcpIPv6Global.Contains(ip) {
		return false
	}
	for _, block := range mcpNonPublic {
		if block.Contains(ip) {
			return false
		}
	}
	return true
}

func dialMCPMetadata(ctx context.Context, address, ownHost, publicIP string) (net.Conn, error) {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return nil, err
	}
	refused := errors.New("client metadata address is not public")
	if strings.EqualFold(strings.TrimSuffix(host, "."), strings.TrimSuffix(ownHost, ".")) {
		return nil, refused
	}
	ips, err := net.DefaultResolver.LookupNetIP(ctx, "ip", host)
	if err != nil || len(ips) == 0 {
		return nil, refused
	}
	local, err := net.InterfaceAddrs()
	if err != nil {
		return nil, err
	}
	own, err := net.DefaultResolver.LookupNetIP(ctx, "ip", ownHost)
	if err != nil {
		return nil, err
	}
	if ip, err := netip.ParseAddr(publicIP); err == nil {
		own = append(own, ip.Unmap())
	}
	for _, address := range local {
		if prefix, err := netip.ParsePrefix(address.String()); err == nil {
			own = append(own, prefix.Addr().Unmap())
		}
	}
	for _, ip := range ips {
		if !mcpPublicIP(ip) || slices.ContainsFunc(own, func(a netip.Addr) bool { return a.Unmap() == ip.Unmap() }) {
			return nil, refused
		}
	}
	var dial net.Dialer
	for _, ip := range ips {
		if c, dialErr := dial.DialContext(ctx, "tcp", net.JoinHostPort(ip.String(), port)); dialErr == nil {
			return c, nil
		} else {
			err = dialErr
		}
	}
	return nil, err
}
