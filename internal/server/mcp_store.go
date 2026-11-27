package server

import (
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"slices"
	"strings"
	"time"

	"deskway/internal/proto"
)

const (
	mcpAccessTTL        = 10 * time.Minute
	mcpRefreshTTL       = 30 * 24 * time.Hour
	mcpCodeTTL          = 2 * time.Minute
	mcpRegistrationTTL  = 24 * time.Hour
	mcpMaxRegistrations = 1024
	mcpRegistrationsIP  = 32
	mcpGrantsPerAccount = 32
	mcpFamiliesPerGrant = 8
	mcpMaxRotations     = 8192
)

type mcpRegistration struct {
	mcpClient
	SecretHash string `json:"secret_hash,omitempty"`
	Address    string `json:"address"`
	LastUsed   int64  `json:"last_used"`
}

type mcpGrant struct {
	User, Browser, Client, Resource string
	Endorsed                        int64
	Families                        []mcpFamily
}

// A new device is endorsed before its CLI signs in. Only code redemption
// publishes the device, its signed log entry, and its grant.
type mcpPendingGrant struct {
	mcpGrant
	Approver string
	Device   proto.BrowserKey
	Proof    proto.Proof
}

type mcpCode struct {
	Hash, Redirect, Challenge string
	Until                     int64
	Refresh                   bool
}

type mcpFamily struct {
	Access, Refresh           string
	AccessUntil, RefreshUntil int64
	Code                      mcpCode
	Spent                     map[string]bool `json:",omitempty"`
}

func (f mcpFamily) live(now int64) bool {
	return now < f.AccessUntil || now < f.RefreshUntil || f.Access == "" && now < f.Code.Until
}

var (
	errMCPFull         = errors.New("MCP client registration limit reached")
	errMCPEndorse      = errors.New("AI client endorsement was refused")
	errMCPGrantLimit   = errors.New("remote application limit reached")
	errMCPFamilyLimit  = errors.New("application sign-in limit reached")
	errMCPInvalidGrant = errors.New("invalid MCP grant")
)

func (s *Store) mcpRegister(c mcpClient, addr string, now time.Time) (id, secret string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	referenced := make(map[string]bool)
	for _, g := range s.MCPGrants {
		referenced[g.Client] = true
	}
	count := 0
	for id, reg := range s.MCPClients {
		if referenced[id] {
			continue
		}
		if now.Unix()-reg.LastUsed >= int64(mcpRegistrationTTL.Seconds()) {
			delete(s.MCPClients, id)
		} else if reg.Address == addr {
			count++
		}
	}
	if len(s.MCPClients) >= mcpMaxRegistrations || count >= mcpRegistrationsIP {
		return "", "", errMCPFull
	}
	if s.MCPClients == nil {
		s.MCPClients = make(map[string]mcpRegistration)
	}
	id = randomHex(24)
	reg := mcpRegistration{mcpClient: c, Address: addr, LastUsed: now.Unix()}
	if c.Method != "none" {
		secret = randomHex(32)
		reg.SecretHash = hashToken(secret)
	}
	s.MCPClients[id] = reg
	if err := s.saveLocked(); err != nil {
		delete(s.MCPClients, id)
		return "", "", err
	}
	return id, secret, nil
}

func (s *Store) mcpClient(id string, now time.Time) (mcpRegistration, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mcpClientLocked(id, now)
}

func (s *Store) mcpClientLocked(id string, now time.Time) (mcpRegistration, bool) {
	reg, ok := s.MCPClients[id]
	return reg, ok && (now.Unix()-reg.LastUsed < int64(mcpRegistrationTTL.Seconds()) || s.mcpClientInUseLocked(id))
}

func (s *Store) mcpClientInUseLocked(id string) bool {
	for _, g := range s.MCPGrants {
		if g.Client == id {
			return true
		}
	}
	return false
}

func (s *Store) mcpUseRegistration(id string, now time.Time) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	reg, ok := s.mcpClientLocked(id, now)
	if !ok {
		return errMCPInvalidGrant
	}
	if reg.LastUsed == now.Unix() || s.mcpClientInUseLocked(id) {
		return nil
	}
	used := reg
	used.LastUsed = now.Unix()
	s.MCPClients[id] = used
	if err := s.saveLocked(); err != nil {
		s.MCPClients[id] = reg
		return err
	}
	return nil
}

func (s *Store) mcpApproverLocked(user, browser string) *proto.BrowserKey {
	u, b := s.Users[user], s.Browsers[browser]
	if u == nil || b == nil || b.Owner != user || !b.Approved || b.Role != "" ||
		b.Key == nil || b.Key.Alg != -7 || b.Key.Role != "" {
		return nil
	}
	return b.Key
}

func (s *Store) mcpCanApprove(user, browser string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mcpApproverLocked(user, browser) != nil
}

func (s *Store) mcpExisting(user, client, resource string) (mcpPrepared, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mcpExistingLocked(user, client, resource)
}

func (s *Store) mcpExistingLocked(user, client, resource string) (mcpPrepared, error) {
	count := 0
	for id, g := range s.MCPGrants {
		if g.User == user {
			count++
			if g.Client == client && g.Resource == resource {
				return mcpPrepared{Profile: id, Existing: true, ID: g.Browser, Device: *s.Browsers[g.Browser].Key, Endorsed: g.Endorsed}, nil
			}
		}
	}
	for id, g := range s.mcpPending {
		if g.User == user {
			count++
			if g.Client == client && g.Resource == resource {
				if len(g.Families) == 0 {
					return mcpPrepared{}, errMCPEndorse // The first CLI sign-in is in progress.
				}
				return mcpPrepared{Profile: id, Existing: true, ID: g.Browser, Device: g.Device, Endorsed: g.Endorsed}, nil
			}
		}
	}
	if count >= mcpGrantsPerAccount {
		return mcpPrepared{}, errMCPGrantLimit
	}
	return mcpPrepared{}, nil
}

func (s *Store) mcpRemoteOwner(browser string) string {
	if browser == "" {
		return ""
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, g := range s.MCPGrants {
		if g.Browser == browser {
			return g.User
		}
	}
	return ""
}

// mcpPreparedLogin is used only for the CLI's signed renewal. Its session
// cookie becomes usable when code redemption creates the approved device.
func (s *Store) mcpPreparedLogin(browser string) *proto.BrowserKey {
	if browser == "" {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, g := range s.mcpPending {
		if g.Browser == browser && len(g.Families) == 0 && s.mcpApproverLocked(g.User, g.Approver) != nil {
			key := g.Device
			return &key
		}
	}
	return nil
}

func (s *Store) mcpEndorse(user, browser, resource string, a *mcpAuthorization, d mcpPrepared, p *proto.Proof, now time.Time) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	k := s.mcpApproverLocked(user, browser)
	if k == nil || d.Device.Role != proto.RoleAI || !p.Valid() || p.By != k.Cred || p.AD != "" || p.CD != "" ||
		p.TS < now.Unix()-aiSigSkew || p.TS > now.Unix()+aiSigSkew ||
		!k.Verify(d.Device.AIEndorsement(p.TS), p.Sig) {
		return errMCPEndorse
	}
	current, err := s.mcpExistingLocked(user, a.Client, resource)
	if err != nil {
		return err
	}
	if d.Existing {
		if current.Profile != d.Profile || current.Device != d.Device || p.TS <= current.Endorsed {
			return errMCPEndorse
		}
		return nil
	}
	if current.Profile != "" || s.Browsers[d.ID] != nil {
		return errMCPEndorse
	}
	if s.mcpPending == nil {
		s.mcpPending = make(map[string]*mcpPendingGrant)
	}
	s.mcpPending[d.Profile] = &mcpPendingGrant{
		mcpGrant: mcpGrant{User: user, Browser: d.ID, Client: a.Client, Resource: resource},
		Approver: browser, Device: d.Device, Proof: *p,
	}
	return nil
}

func (s *Store) mcpIssueCode(profile, user, browser string, a *mcpAuthorization, ts int64, now time.Time) (string, time.Time, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	g := s.MCPGrants[profile]
	pending := s.mcpPending[profile]
	if pending != nil {
		g = &pending.mcpGrant
	}
	if g == nil || g.User != user || s.mcpApproverLocked(user, browser) == nil ||
		!now.Before(a.Expires) || ts <= g.Endorsed {
		return "", time.Time{}, errMCPEndorse
	}
	families := slices.DeleteFunc(slices.Clone(g.Families), func(f mcpFamily) bool { return !f.live(now.Unix()) })
	if len(families) == mcpFamiliesPerGrant {
		oldest := slices.IndexFunc(families, func(f mcpFamily) bool { return f.Access == "" })
		if oldest < 0 {
			return "", time.Time{}, errMCPFamilyLimit
		}
		families = slices.Delete(families, oldest, oldest+1)
	}
	code := profile + "." + randomHex(32)
	until := now.Add(mcpCodeTTL).Truncate(time.Second)
	families = append(families, mcpFamily{Code: mcpCode{
		Hash: hashToken(code), Redirect: a.Redirect, Challenge: a.Challenge, Until: until.Unix(), Refresh: a.Refresh,
	}})
	old := *g
	g.Families, g.Endorsed = families, ts
	if pending != nil {
		return code, until, nil
	}
	if err := s.saveLocked(); err != nil {
		*g = old
		return "", time.Time{}, err
	}
	return code, time.Time{}, nil
}

func (s *Store) mcpDiscardPending(profile string, now time.Time) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	g := s.mcpPending[profile]
	if g == nil || slices.ContainsFunc(g.Families, func(f mcpFamily) bool { return f.live(now.Unix()) }) {
		return false
	}
	delete(s.mcpPending, profile)
	return true
}

func validMCPID(id string, length int) bool {
	if len(id) != length {
		return false
	}
	for _, c := range id {
		if !(c >= 'a' && c <= 'f' || c >= '0' && c <= '9') {
			return false
		}
	}
	return true
}

func mcpTokenProfile(token string) string {
	profile, random, _ := strings.Cut(token, ".")
	if validMCPID(profile, 24) && validMCPID(random, 64) {
		return profile
	}
	return ""
}

func (s *Store) mcpAccess(token, resource string, now time.Time) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	profile := mcpTokenProfile(token)
	g := s.MCPGrants[profile]
	if g != nil && g.Resource == resource {
		hash := hashToken(token)
		for _, f := range g.Families {
			if now.Unix() < f.AccessUntil && f.Access == hash {
				return profile
			}
		}
	}
	return ""
}

type mcpTokenResponse struct {
	Access  string `json:"access_token"`
	Type    string `json:"token_type"`
	Expires int64  `json:"expires_in"`
	Refresh string `json:"refresh_token,omitempty"`
}

type mcpExchange struct {
	Client, Resource, Token, Redirect, Verifier string
	Refresh                                     bool
}

func (s *Store) mcpExchange(x mcpExchange, now time.Time) (mcpTokenResponse, string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	profile := mcpTokenProfile(x.Token)
	g := s.MCPGrants[profile]
	pending := s.mcpPending[profile]
	if pending != nil && s.mcpApproverLocked(pending.User, pending.Approver) != nil {
		g = &pending.mcpGrant
	}
	if g == nil || g.Client != x.Client || g.Resource != x.Resource {
		return mcpTokenResponse{}, "", errMCPInvalidGrant
	}
	hash := hashToken(x.Token)
	digest := sha256.Sum256([]byte(x.Verifier))
	challenge := base64.RawURLEncoding.EncodeToString(digest[:])
	for i, f := range g.Families {
		if x.Refresh {
			if now.Unix() >= f.RefreshUntil || f.Refresh != hash && !f.Spent[hash] {
				continue
			}
			if f.Spent[hash] || len(f.Spent) >= mcpMaxRotations {
				return mcpTokenResponse{}, "", s.mcpRevokeFamilyLocked(g, i)
			}
		} else {
			if f.Code.Hash != hash || f.Code.Redirect != x.Redirect ||
				subtle.ConstantTimeCompare([]byte(f.Code.Challenge), []byte(challenge)) != 1 {
				continue
			}
			if f.Access != "" && f.live(now.Unix()) {
				return mcpTokenResponse{}, "", s.mcpRevokeFamilyLocked(g, i)
			}
			if now.Unix() >= f.Code.Until {
				break
			}
		}
		return s.mcpMintLocked(profile, g, pending, i, x.Refresh, now)
	}
	return mcpTokenResponse{}, "", errMCPInvalidGrant
}

func (s *Store) mcpMintLocked(profile string, g *mcpGrant, pending *mcpPendingGrant, index int, rotate bool, now time.Time) (mcpTokenResponse, string, error) {
	old := g.Families[index]
	f := old
	access := profile + "." + randomHex(32)
	response := mcpTokenResponse{Access: access, Type: "Bearer", Expires: int64(mcpAccessTTL.Seconds())}
	f.Access, f.AccessUntil = hashToken(access), now.Add(mcpAccessTTL).Unix()
	if f.Code.Refresh {
		if rotate {
			if f.Spent == nil {
				f.Spent = make(map[string]bool)
			}
			f.Spent[old.Refresh] = true
		} else {
			f.RefreshUntil = now.Add(mcpRefreshTTL).Unix()
		}
		response.Refresh = profile + "." + randomHex(32)
		f.Refresh = hashToken(response.Refresh)
	}
	g.Families[index] = f
	var previous []proto.LogEntry
	if pending != nil {
		if s.MCPGrants == nil {
			s.MCPGrants = make(map[string]*mcpGrant)
		}
		s.MCPGrants[profile] = g
		key := pending.Device
		s.Browsers[g.Browser] = &BrowserRecord{Owner: g.User, Label: key.Name, Role: proto.RoleAI, Key: &key,
			Approved: true, Created: now.Unix(), LastSeen: now.Unix()}
		previous = s.Users[g.User].DeviceLog
		s.Users[g.User].DeviceLog = slices.Clone(previous)
		s.appendLogLocked(g.User, key.AddEntry(pending.Proof))
	}
	if err := s.saveLocked(); err != nil {
		delete(old.Spent, old.Refresh)
		g.Families[index] = old
		if pending != nil {
			delete(s.MCPGrants, profile)
			delete(s.Browsers, g.Browser)
			s.Users[g.User].DeviceLog = previous
			delete(s.mcpPending, profile)
		}
		return mcpTokenResponse{}, "", err
	}
	if pending != nil {
		delete(s.mcpPending, profile)
	}
	if !rotate {
		return response, g.User, nil
	}
	return response, "", nil
}

func (s *Store) mcpRevokeFamilyLocked(g *mcpGrant, index int) error {
	old := g.Families
	g.Families = slices.Delete(slices.Clone(old), index, index+1)
	if err := s.saveLocked(); err != nil {
		g.Families = old
		return err
	}
	return errMCPInvalidGrant
}

func mcpSecretOK(hash, secret string) bool {
	return subtle.ConstantTimeCompare([]byte(hash), []byte(hashToken(secret))) == 1
}

func (s *Store) mcpHasProfile(id string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.MCPGrants[id] != nil || s.mcpPending[id] != nil
}

// mcpRemoveLocked joins the caller's device-removal write.
func (s *Store) mcpRemoveLocked(browsers map[string]bool) []string {
	var profiles []string
	for id, g := range s.MCPGrants {
		if browsers[g.Browser] {
			s.mcpRetireLocked(id)
			profiles = append(profiles, id)
		}
	}
	return profiles
}

func (s *Store) mcpRetireLocked(id string) {
	g := s.MCPGrants[id]
	delete(s.MCPGrants, id)
	if reg, ok := s.MCPClients[g.Client]; ok {
		reg.LastUsed = time.Now().Unix()
		s.MCPClients[g.Client] = reg
	}
}

func (s *Store) mcpReconcileProfiles(present map[string]bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	changed := false
	for id, g := range s.MCPGrants {
		b := s.Browsers[g.Browser]
		approved := b != nil && b.Owner == g.User && b.Approved && b.Role == proto.RoleAI && b.Key != nil
		if present[id] && approved && s.Users[g.User] != nil {
			continue
		}
		if approved {
			delete(s.Browsers, g.Browser)
		}
		s.mcpRetireLocked(id)
		changed = true
	}
	if changed {
		return s.saveLocked()
	}
	return nil
}

func (s *Store) mcpLoseSignIn(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if g := s.MCPGrants[id]; g != nil {
		delete(s.Browsers, g.Browser)
		s.mcpRetireLocked(id)
		return s.saveLocked()
	}
	return nil
}
