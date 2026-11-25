package server

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"maps"
	"net"
	"net/http"
	"net/netip"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

const (
	relayJoinTTL        = 10 * time.Minute
	relayReportInterval = 30 * time.Second
	relayOnlineTTL      = 3 * relayReportInterval
	maxOfferedRelays    = 3
)

// Only presence and pending tokens live here. The store owns the relay records and secrets.
type relayState struct {
	mu      sync.Mutex
	now     func() time.Time
	tokens  map[string]time.Time
	seen    map[string]relayPresence
	changed chan struct{}
}

type relayPresence struct {
	received time.Time
	ts       int64
}

func (p relayPresence) online(now time.Time) bool {
	return !p.received.IsZero() && now.Before(p.received.Add(relayOnlineTTL))
}

func newRelayState() relayState {
	return relayState{now: time.Now, tokens: map[string]time.Time{}, seen: map[string]relayPresence{}, changed: make(chan struct{})}
}

func (rs *relayState) notifyLocked() {
	close(rs.changed)
	rs.changed = make(chan struct{})
}

type relayJoin struct {
	Token    string `json:"token"`
	PublicIP string `json:"public_ip,omitempty"`
	Port     int    `json:"port"`
}

type relayJoined struct {
	ID          string `json:"id"`
	Secret      string `json:"secret"`
	PublicIP    string `json:"public_ip"`
	AccountMbps int    `json:"account_mbps"`
}

type relayReport struct {
	ID  string `json:"id"`
	TS  int64  `json:"ts"`
	MAC string `json:"mac"`
}

func relayReportMAC(secret, id string, ts int64) string {
	mac := hmac.New(sha256.New, []byte(secret))
	fmt.Fprintf(mac, "deskway-relay-report-v1|%s|%d", id, ts)
	return hex.EncodeToString(mac.Sum(nil))
}

func (s *Server) handleRelayToken(w http.ResponseWriter, _ *http.Request) {
	rs := &s.relays
	token := randomHex(32)
	rs.mu.Lock()
	now := rs.now()
	maps.DeleteFunc(rs.tokens, func(_ string, until time.Time) bool { return !now.Before(until) })
	rs.tokens[token] = now.Add(relayJoinTTL)
	rs.mu.Unlock()
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, map[string]string{"token": token})
}

// Special-purpose IPv4 blocks that IsGlobalUnicast and IsPrivate do not exclude.
var nonPublicRelayPrefixes = [...]netip.Prefix{
	netip.MustParsePrefix("0.0.0.0/8"),
	netip.MustParsePrefix("100.64.0.0/10"),
	netip.MustParsePrefix("192.0.0.0/24"),
	netip.MustParsePrefix("192.0.2.0/24"),
	netip.MustParsePrefix("192.31.196.0/24"),
	netip.MustParsePrefix("192.52.193.0/24"),
	netip.MustParsePrefix("192.88.99.0/24"),
	netip.MustParsePrefix("192.175.48.0/24"),
	netip.MustParsePrefix("198.18.0.0/15"),
	netip.MustParsePrefix("198.51.100.0/24"),
	netip.MustParsePrefix("203.0.113.0/24"),
	netip.MustParsePrefix("240.0.0.0/4"),
}

func publicRelayIPv4(ip net.IP) bool {
	if !ip.IsGlobalUnicast() || ip.IsPrivate() {
		return false
	}
	addr, _ := netip.AddrFromSlice(ip)
	for _, prefix := range nonPublicRelayPrefixes {
		if prefix.Contains(addr) {
			return false
		}
	}
	return true
}

func (s *Server) handleRelayJoin(w http.ResponseWriter, r *http.Request) {
	if !isHTTPS(r) {
		http.Error(w, "HTTPS required", http.StatusForbidden)
		return
	}
	var body relayJoin
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	address := body.PublicIP
	if address == "" {
		address = clientIP(r)
	}
	ip := net.ParseIP(address).To4()
	if body.PublicIP == "" && !publicRelayIPv4(ip) {
		http.Error(w, "the observed address is not a public IPv4 address; pass -public-ip with the relay's public IPv4 address", http.StatusBadRequest)
		return
	}
	if ip == nil || ip.IsUnspecified() || body.Port < 1 || body.Port > 65535 {
		http.Error(w, "a relay needs an IPv4 address and a valid TURN port", http.StatusBadRequest)
		return
	}
	rs := &s.relays
	rs.mu.Lock()
	defer rs.mu.Unlock()
	if !rs.now().Before(rs.tokens[body.Token]) {
		http.Error(w, "invalid relay join token", http.StatusForbidden)
		return
	}
	relay := RelayRecord{
		ID: randomHex(16), Name: ip.String(), PublicIP: ip.String(), Port: body.Port,
		Secret: randomHex(32),
	}
	if err := s.store.addRelay(relay); err != nil {
		storeError(w, err)
		return
	}
	delete(rs.tokens, body.Token)
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, relayJoined{ID: relay.ID, Secret: relay.Secret, PublicIP: relay.PublicIP, AccountMbps: s.turn.AccountMbps})
}

func (s *Server) handleRelayReport(w http.ResponseWriter, r *http.Request) {
	if !isHTTPS(r) {
		http.Error(w, "HTTPS required", http.StatusForbidden)
		return
	}
	var body relayReport
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	rs := &s.relays
	rs.mu.Lock()
	defer rs.mu.Unlock()
	now := rs.now()
	prev := rs.seen[body.ID]
	secret := s.store.relaySecret(body.ID)
	if body.TS <= prev.ts || time.Unix(body.TS, 0).Sub(now).Abs() > relayReportInterval ||
		secret == "" || !hmac.Equal([]byte(body.MAC), []byte(relayReportMAC(secret, body.ID, body.TS))) {
		http.Error(w, "invalid relay report", http.StatusUnauthorized)
		return
	}
	rs.seen[body.ID] = relayPresence{received: now, ts: body.TS}
	if !prev.online(now) {
		rs.notifyLocked()
	}
	w.WriteHeader(http.StatusNoContent)
}

type relayView struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	PublicIP string `json:"public_ip"`
	Port     int    `json:"port"`
	Online   bool   `json:"online"`
	LastSeen int64  `json:"last_seen,omitempty"`
}

func (s *Server) handleRelays(w http.ResponseWriter, _ *http.Request) {
	rs := &s.relays
	rs.mu.Lock()
	records := s.store.relayRecords()
	list := make([]relayView, 0, len(records))
	now := rs.now()
	for _, relay := range slices.Backward(records) {
		p := rs.seen[relay.ID]
		item := relayView{
			ID: relay.ID, Name: relay.Name, PublicIP: relay.PublicIP, Port: relay.Port,
			Online: p.online(now),
		}
		if !p.received.IsZero() {
			item.LastSeen = p.received.Unix()
		}
		list = append(list, item)
	}
	rs.mu.Unlock()
	w.Header().Set("Cache-Control", "no-store")
	writeJSON(w, list)
}

func (s *Server) handleRelayRename(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name string `json:"name"`
	}
	if !decodeBody(w, r, 1024, &body) {
		return
	}
	name := strings.TrimSpace(body.Name)
	if name == "" || utf8.RuneCountInString(name) > 40 {
		http.Error(w, "invalid relay name", http.StatusBadRequest)
		return
	}
	if err := s.store.renameRelay(r.PathValue("id"), name); err != nil {
		storeError(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleRelayRemove(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	rs := &s.relays
	rs.mu.Lock()
	defer rs.mu.Unlock()
	if err := s.store.removeRelay(id); err != nil {
		storeError(w, err)
		return
	}
	delete(rs.seen, id)
	rs.notifyLocked()
	w.WriteHeader(http.StatusNoContent)
}

// offeredRelays picks the newest online nodes. The deadline and change signal let open
// browser sockets refresh their ICE list when a node expires, comes online or is removed.
func (s *Server) offeredRelays() (nodes []TURNConfig, untilExpiry time.Duration, changed <-chan struct{}) {
	rs := &s.relays
	rs.mu.Lock()
	defer rs.mu.Unlock()
	now := rs.now()
	for _, relay := range slices.Backward(s.store.relayRecords()) {
		p := rs.seen[relay.ID]
		if !p.online(now) {
			continue
		}
		left := p.received.Add(relayOnlineTTL).Sub(now)
		if untilExpiry == 0 || left < untilExpiry {
			untilExpiry = left
		}
		nodes = append(nodes, TURNConfig{PublicIP: relay.PublicIP, Port: relay.Port, Secret: relay.Secret})
		if len(nodes) == maxOfferedRelays {
			break
		}
	}
	return nodes, untilExpiry, rs.changed
}
