package server

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"deskway/internal/proto"
)

const testMCPIssuer = "https://desk.example"
const testMCPRedirect = "http://127.0.0.1:3000/callback"
const testMCPVerifier = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"

type testMCPGateway struct {
	calls, stopped []string
}

func (*testMCPGateway) close()                {}
func (g *testMCPGateway) stop(profile string) { g.stopped = append(g.stopped, profile) }
func (g *testMCPGateway) forward(w http.ResponseWriter, r *http.Request, profile string, allowed func() bool) {
	if !allowed() {
		return
	}
	if r.Method == http.MethodGet {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	var message struct {
		ID     json.RawMessage
		Method string
	}
	_ = json.NewDecoder(r.Body).Decode(&message)
	g.calls = append(g.calls, profile)
	result := json.RawMessage(`{"tools":[]}`)
	switch message.Method {
	case "initialize":
		result = json.RawMessage(`{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}`)
	case "notifications/initialized":
		w.WriteHeader(http.StatusAccepted)
		return
	case "tools/call":
		result = json.RawMessage(`{"content":[{"type":"text","text":"hello from Deskway"}]}`)
	}
	writeJSON(w, map[string]any{"jsonrpc": "2.0", "id": message.ID, "result": result})
}

func testMCPFixture(t *testing.T) (*fixture, *testMCPGateway) {
	t.Helper()
	gateway := &testMCPGateway{}
	f := newHTTPFixture(t, func(s *Server) {
		m, err := newRemoteMCP(testMCPIssuer)
		if err != nil {
			t.Fatal(err)
		}
		m.gateways = gateway
		m.prepare = func(_ context.Context, profile, _, name string) (mcpPrepared, error) {
			return mcpPrepared{Profile: profile, ID: randomHex(32),
				Device: proto.BrowserKey{Cred: randomHex(16), Alg: -7, Key: "application-key", Name: name, Role: proto.RoleAI}}, nil
		}
		m.finish = func(context.Context, string) error { return nil }
		s.mcp = m
	}, httptest.NewTLSServer)
	return f, gateway
}

func (f *fixture) mcpRequest(c *http.Client, method, path, content, body string, headers map[string]string) *http.Response {
	f.t.Helper()
	req, err := http.NewRequest(method, f.srv.URL+path, strings.NewReader(body))
	if err != nil {
		f.t.Fatal(err)
	}
	if content != "" {
		req.Header.Set("Content-Type", content)
	}
	for name, value := range headers {
		req.Header.Set(name, value)
	}
	res, err := c.Do(req)
	if err != nil {
		f.t.Fatal(err)
	}
	f.t.Cleanup(func() { res.Body.Close() })
	return res
}

func mcpJSON[T any](t *testing.T, res *http.Response, status int) T {
	t.Helper()
	defer res.Body.Close()
	want(t, res.Request.URL.Path, res.StatusCode, status)
	var v T
	if err := json.NewDecoder(res.Body).Decode(&v); err != nil {
		t.Fatal(err)
	}
	return v
}

func mcpRegistrationBody(method string) string {
	b, _ := json.Marshal(mcpClient{Name: "Example application", Redirects: []string{testMCPRedirect},
		Method: method, Grants: []string{"authorization_code", "refresh_token"}})
	return string(b)
}

func (f *fixture) registerMCPForTest(method string) map[string]any {
	return mcpJSON[map[string]any](f.t, f.mcpRequest(f.browser(), "POST", mcpRegisterPath, "application/json", mcpRegistrationBody(method), nil), 201)
}

func mcpAuthorizationQuery(client, resource string) url.Values {
	digest := sha256.Sum256([]byte(testMCPVerifier))
	return url.Values{"client_id": {client}, "redirect_uri": {testMCPRedirect}, "state": {"caller state"},
		"response_type": {"code"}, "code_challenge_method": {"S256"}, "resource": {resource},
		"code_challenge": {base64.RawURLEncoding.EncodeToString(digest[:])}}
}

func TestMCPDiscoveryAndRateLimit(t *testing.T) {
	f, _ := testMCPFixture(t)
	c := f.browser()
	for _, test := range []struct{ path, field, value string }{
		{mcpResourceMetadata, "resource", testMCPIssuer + "/mcp"},
		{"/.well-known/oauth-protected-resource", "resource", testMCPIssuer + "/mcp"},
		{mcpIssuerMetadata, "issuer", testMCPIssuer},
	} {
		t.Run(test.path, func(t *testing.T) {
			doc := mcpJSON[map[string]any](t, f.do(c, "GET", test.path, nil), 200)
			if doc[test.field] != test.value {
				t.Fatal(doc)
			}
			if test.field == "resource" {
				servers := doc["authorization_servers"].([]any)
				if len(servers) != 1 || servers[0] != testMCPIssuer {
					t.Fatal(doc)
				}
			} else if doc["authorization_response_iss_parameter_supported"] != true ||
				doc["client_id_metadata_document_supported"] != true || doc["token_endpoint"] != testMCPIssuer+mcpTokenPath {
				t.Fatal(doc)
			}
		})
	}
	for _, test := range []struct{ token, suffix string }{{"", ""}, {"invalid", `, error="invalid_token"`}} {
		headers := map[string]string{}
		if test.token != "" {
			headers["Authorization"] = "Bearer " + test.token
		}
		r := f.mcpRequest(c, "POST", "/mcp", "", "", headers)
		want(t, "protected resource", r.StatusCode, 401)
		if got := r.Header.Get("WWW-Authenticate"); got != `Bearer resource_metadata="`+testMCPIssuer+mcpResourceMetadata+`"`+test.suffix {
			t.Fatal(got)
		}
	}
	for i := range 13 {
		r := f.mcpRequest(c, "POST", mcpRegisterPath, "application/json", mcpRegistrationBody("none"), nil)
		status := 201
		if i == 12 {
			status = 429
			if r.Header.Get("Retry-After") == "" {
				t.Fatal("missing Retry-After")
			}
		}
		want(t, "registration", r.StatusCode, status)
		r.Body.Close()
	}
}

func TestMCPDisabled(t *testing.T) {
	f := newFixture(t)
	c := f.browser()
	for _, route := range []struct{ method, path string }{
		{"GET", "/mcp"}, {"GET", mcpResourceMetadata}, {"GET", mcpIssuerMetadata},
		{"GET", mcpAuthorizePath}, {"GET", mcpApprovalPath},
		{"POST", mcpRegisterPath}, {"POST", mcpTokenPath},
	} {
		if status := f.status(c, route.method, route.path, nil); status != 404 && status != 405 {
			t.Errorf("%s %s: got %d", route.method, route.path, status)
		}
	}
}

func TestMCPAuthorizationErrors(t *testing.T) {
	f, _ := testMCPFixture(t)
	c := f.browser()
	reg := f.registerMCPForTest("none")
	for _, test := range []struct{ field, value, code string }{
		{"client_id", "unknown", "invalid_client"},
		{"redirect_uri", "https://another.example/callback", "invalid_request"},
		{"response_type", "token", "unsupported_response_type"},
		{"response_type", "", "invalid_request"},
		{"code_challenge_method", "plain", "invalid_request"},
		{"code_challenge", "short", "invalid_request"},
		{"resource", "", "invalid_target"},
		{"resource", "https://other.example/mcp", "invalid_target"},
		{"scope", "other", "invalid_scope"},
		{"state", "invalid\nstate", "invalid_request"},
	} {
		t.Run(test.field+" "+test.value, func(t *testing.T) {
			q := mcpAuthorizationQuery(reg["client_id"].(string), f.s.mcp.resource)
			q.Set(test.field, test.value)
			r := f.do(c, "GET", mcpAuthorizePath+"?"+q.Encode(), nil)
			if body := mcpJSON[map[string]string](t, r, 400); body["error"] != test.code || r.Header.Get("Location") != "" {
				t.Fatal(body, r.Header.Get("Location"))
			}
		})
	}
}

type mcpTestApprover struct {
	*fixture
	owner  *http.Client
	signer aiDevice
}

func (f *fixture) mcpApprover(user string) mcpTestApprover {
	signer := newAIDevice(f.t)
	delete(signer.key, "role")
	signer.key["name"] = "Test browser"
	owner := f.signedIn(user)
	want(f.t, "attach browser key", f.status(owner, "POST", "/api/login/device", signer.key), 200)
	return mcpTestApprover{f, owner, signer}
}

func (a mcpTestApprover) open(id string) string {
	a.t.Helper()
	r := a.do(a.browser(), "GET", mcpAuthorizePath+"?"+mcpAuthorizationQuery(id, a.s.mcp.resource).Encode(), nil)
	want(a.t, "authorization request", r.StatusCode, 303)
	root := mustURL(r.Header.Get("Location"))
	request := root.Query().Get("mcp")
	if root.Path != "/" || request == "" {
		a.t.Fatalf("approval did not open at the web root: %s", root)
	}
	doc := mcpJSON[map[string]any](a.t, a.do(a.owner, "GET", mcpApprovalPath+"?request="+request, nil), 200)
	if doc["redirectHost"] != "127.0.0.1:3000" || doc["canApprove"] != true {
		a.t.Fatal(doc)
	}
	return request
}

func (a mcpTestApprover) approval(c *http.Client, request, action string, proof *proto.Proof) *http.Response {
	a.t.Helper()
	b, _ := json.Marshal(map[string]any{"request": request, "action": action, "proof": proof})
	return a.mcpRequest(c, "POST", mcpApprovalPath, "application/json", string(b), map[string]string{"Origin": testMCPIssuer})
}

func (a mcpTestApprover) prepare(request string) mcpPrepared {
	a.t.Helper()
	return mcpJSON[mcpPrepared](a.t, a.approval(a.owner, request, "prepare", nil), 200)
}

func (a mcpTestApprover) proof(d mcpPrepared, ts int64) *proto.Proof {
	a.t.Helper()
	return &proto.Proof{TS: ts, By: a.signer.key["cred"].(string), Sig: a.signer.sign(a.t, d.Device.AIEndorsement(ts))}
}

func (a mcpTestApprover) code(request string, proof *proto.Proof) string {
	a.t.Helper()
	result := mcpJSON[map[string]string](a.t, a.approval(a.owner, request, "approve", proof), 200)
	callback := mustURL(result["redirect"]).Query()
	if callback.Get("iss") != testMCPIssuer || callback.Get("state") != "caller state" || callback.Get("code") == "" {
		a.t.Fatal(callback)
	}
	return callback.Get("code")
}

func (a mcpTestApprover) issue(id string) (string, mcpPrepared) {
	a.t.Helper()
	request := a.open(id)
	d := a.prepare(request)
	return a.code(request, a.proof(d, max(time.Now().Unix(), d.Endorsed+1))), d
}

func TestMCPApprovalAndRevocation(t *testing.T) {
	f, gateway := testMCPFixture(t)
	a := f.mcpApprover("me")
	owner, public := a.owner, f.browser()
	exchange := func(q url.Values) *http.Response {
		return f.mcpRequest(public, "POST", mcpTokenPath, "application/x-www-form-urlencoded", q.Encode(), nil)
	}
	redeem := func(id, code, verifier string) *http.Response {
		return exchange(url.Values{"grant_type": {"authorization_code"}, "client_id": {id}, "resource": {f.s.mcp.resource},
			"code": {code}, "redirect_uri": {testMCPRedirect}, "code_verifier": {verifier}})
	}
	refresh := func(id, token string) *http.Response {
		return exchange(url.Values{"grant_type": {"refresh_token"}, "client_id": {id}, "resource": {f.s.mcp.resource}, "refresh_token": {token}})
	}
	use := func(token string, status int) {
		want(t, "MCP call", f.mcpRequest(public, "POST", "/mcp", "application/json", "{}", map[string]string{"Authorization": "Bearer " + token}).StatusCode, status)
	}
	id := f.registerMCPForTest("none")["client_id"].(string)
	other := f.registerMCPForTest("none")["client_id"].(string)
	request := a.open(id)
	want(t, "untrusted prepare", a.approval(public, request, "prepare", nil).StatusCode, 403)
	want(t, "untrusted cancel", a.approval(public, request, "cancel", nil).StatusCode, 403)
	cancel := mcpJSON[map[string]string](t, a.approval(owner, request, "cancel", nil), 200)
	if callback := mustURL(cancel["redirect"]).Query(); callback.Get("error") != "access_denied" || callback.Get("iss") != testMCPIssuer {
		t.Fatal(callback)
	}
	firstRequest := a.open(id)
	firstDevice := a.prepare(firstRequest)
	replayed := a.proof(firstDevice, time.Now().Unix())
	code := a.code(firstRequest, replayed)
	key := firstDevice.Device
	secondRequest := a.open(id)
	same := a.prepare(secondRequest)
	if same.Device != key {
		t.Fatal("two unredeemed approvals use different device keys")
	}
	want(t, "replayed endorsement", a.approval(owner, secondRequest, "approve", replayed).StatusCode, 403)
	anotherCode, _ := a.issue(id)
	want(t, "PKCE", redeem(id, code, strings.Repeat("a", 43)).StatusCode, 400)
	first := mcpJSON[mcpTokenResponse](t, redeem(id, code, testMCPVerifier), 200)
	use(first.Access, 200)
	for _, test := range []struct {
		resource string
		now      time.Time
	}{
		{"https://other.example/mcp", time.Now()},
		{f.s.mcp.resource, time.Now().Add(mcpAccessTTL)},
	} {
		profile := f.s.store.mcpAccess(first.Access, test.resource, test.now)
		if profile != "" {
			t.Fatal("wrong audience or expired access token was accepted")
		}
	}
	another := mcpJSON[mcpTokenResponse](t, redeem(id, anotherCode, testMCPVerifier), 200)
	use(another.Access, 200)
	use(first.Access, 200)
	oldRequest := a.open(id)
	a.prepare(oldRequest)
	want(t, "older endorsement on the published grant", a.approval(owner, oldRequest, "approve", replayed).StatusCode, 403)
	rotated := mcpJSON[mcpTokenResponse](t, refresh(id, first.Refresh), 200)
	use(rotated.Access, 200)
	want(t, "refresh token reuse", refresh(id, first.Refresh).StatusCode, 400)
	use(rotated.Access, 401)
	want(t, "family revoked", refresh(id, rotated.Refresh).StatusCode, 400)
	use(another.Access, 200)
	code, _ = a.issue(id)
	first = mcpJSON[mcpTokenResponse](t, redeem(id, code, testMCPVerifier), 200)
	want(t, "code reuse", redeem(id, code, testMCPVerifier).StatusCode, 400)
	use(first.Access, 401)
	code, _ = a.issue(id)
	first = mcpJSON[mcpTokenResponse](t, redeem(id, code, testMCPVerifier), 200)
	code, second := a.issue(other)
	if key.Cred == second.Device.Cred {
		t.Fatal("applications share an identity")
	}
	otherTokens := mcpJSON[mcpTokenResponse](t, redeem(other, code, testMCPVerifier), 200)
	rows := mcpJSON[[]struct {
		ID     string
		Remote bool `json:"remote_mcp"`
		Device proto.BrowserKey
	}](t, f.do(owner, "GET", "/api/devices", nil), 200)
	var deviceID string
	for _, row := range rows {
		if row.Device.Cred == key.Cred && row.Remote {
			deviceID = row.ID
		}
	}
	if deviceID == "" {
		t.Fatal("remote AI client missing from My devices")
	}
	want(t, "remove", f.status(owner, "DELETE", "/api/browsers/"+deviceID, nil), 204)
	use(first.Access, 401)
	use(another.Access, 401)
	use(otherTokens.Access, 200)
	var saved Store
	if raw, err := os.ReadFile(f.data); err != nil || json.Unmarshal(raw, &saved) != nil {
		t.Fatal("cannot read saved state", err)
	}
	if saved.Browsers[deviceID] != nil || saved.MCPGrants[mcpTokenProfile(first.Access)] != nil {
		t.Fatal("removed identity remains in saved state")
	}
	if len(gateway.stopped) != 1 || gateway.stopped[0] == gateway.calls[len(gateway.calls)-1] {
		t.Fatal("removal stopped the wrong profile")
	}
}

// Optional because the SDK is an external test runner, rather than a server dependency.
func TestMCPOfficialSDK(t *testing.T) {
	dir := os.Getenv("DESKWAY_MCP_SDK")
	if dir == "" {
		t.Skip("set DESKWAY_MCP_SDK to an installed @modelcontextprotocol/sdk directory")
	}
	f, _ := testMCPFixture(t)
	owner := f.signedIn("me")
	server := httptest.NewUnstartedServer(nil)
	f.s.mcp.issuer = "https://" + server.Listener.Addr().String()
	f.s.mcp.resource = f.s.mcp.issuer + mcpResourcePath
	server.Config.Handler = f.s.Handler()
	server.StartTLS()
	defer server.Close()
	cert := filepath.Join(t.TempDir(), "tls.pem")
	if err := os.WriteFile(cert, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw}), 0600); err != nil {
		t.Fatal(err)
	}
	var cookies []string
	for _, c := range owner.Jar.Cookies(mustURL(f.srv.URL)) {
		cookies = append(cookies, c.String())
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "node", "testdata/mcp_sdk.mjs", dir, server.URL)
	cmd.Env = append(cmd.Environ(), "NODE_EXTRA_CA_CERTS="+cert, "DESKWAY_MCP_TEST_COOKIE="+strings.Join(cookies, "; "))
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("%v\n%s", err, out)
	}
	t.Logf("%s", out)
}
func TestMCPApprovalCleanup(t *testing.T) {
	f, gateway := testMCPFixture(t)
	a := f.mcpApprover("me")
	store := f.s.store
	id := f.registerMCPForTest("none")["client_id"].(string)
	root := f.s.mcpProfileRoot()
	if err := os.Mkdir(root, 0700); err != nil {
		t.Fatal(err)
	}
	m := f.s.mcp
	prepare := m.prepare
	m.prepare = func(ctx context.Context, profile, user, name string) (mcpPrepared, error) {
		if err := os.Mkdir(f.s.mcpProfilePath(profile), 0700); err != nil {
			return mcpPrepared{}, err
		}
		return prepare(ctx, profile, user, name)
	}
	for _, test := range []string{"cancel", "invalid signature", "CLI sign-in fails", "request expires", "code expires"} {
		t.Run(test, func(t *testing.T) {
			request := a.open(id)
			d := a.prepare(request)
			m.mu.Lock()
			prepared := m.requests[request].Prepared
			m.mu.Unlock()
			m.finish = func(context.Context, string) error {
				if store.mcpPreparedLogin(prepared.ID) == nil || store.SignedInOwner(prepared.ID) != "" {
					t.Error("the endorsed key must be available for CLI sign-in only")
				}
				if test == "CLI sign-in fails" {
					return errors.New("CLI could not sign in")
				}
				return nil
			}
			proof := a.proof(d, time.Now().Unix())
			switch test {
			case "cancel":
				want(t, test, a.approval(a.owner, request, "cancel", nil).StatusCode, 200)
			case "invalid signature":
				proof.Sig = "bad"
				want(t, test, a.approval(a.owner, request, "approve", proof).StatusCode, 403)
			case "CLI sign-in fails":
				body := mcpJSON[map[string]string](t, a.approval(a.owner, request, "approve", proof), 503)
				if body["error"] != "restart_required" {
					t.Fatal(body)
				}
			case "request expires":
				m.mu.Lock()
				m.requests[request].Expires = time.Now()
				m.mu.Unlock()
				f.s.dropMCPRequest(request)
			case "code expires":
				code := a.code(request, proof)
				if _, err := os.Stat(f.s.mcpProfilePath(prepared.Profile)); err != nil || code == "" {
					t.Fatal("the unredeemed code did not keep its profile", err)
				}
				f.s.expireMCPPending(prepared.Profile, time.Now().Add(mcpCodeTTL))
			}
			body := mcpJSON[map[string]string](t, a.approval(a.owner, request, "prepare", nil), 404)
			if body["error"] != "restart_required" {
				t.Fatal(body)
			}
			dirs, err := os.ReadDir(root)
			log, _ := store.DeviceLog(f.id("me"))
			if err != nil || len(dirs) != 0 || store.mcpHasProfile(prepared.Profile) || len(gateway.calls) != 0 ||
				len(store.ApprovedBrowsers(f.id("me"))) != 1 || len(log) != 1 {
				t.Fatalf("approval left profiles %v, log %v, or a device/grant/gateway: %v", dirs, log, err)
			}
		})
	}
}

func TestMCPLongDeviceName(t *testing.T) {
	f, _ := testMCPFixture(t)
	a := f.mcpApprover("me")
	name := strings.Repeat("a", 58) + "界🙂" + "tail"
	reg := mcpJSON[map[string]any](t, f.mcpRequest(f.browser(), "POST", mcpRegisterPath, "application/json",
		strings.Replace(mcpRegistrationBody("none"), "Example application", name, 1), nil), 201)
	code, d := a.issue(reg["client_id"].(string))
	if d.Device.Name != strings.Repeat("a", 58)+"界🙂" || code == "" {
		t.Fatalf("signed device name %q", d.Device.Name)
	}
}

func TestMCPRequestLimits(t *testing.T) {
	f, _ := testMCPFixture(t)
	client := f.registerMCPForTest("none")["client_id"].(string)
	query := mcpAuthorizationQuery(client, f.s.mcp.resource)
	open := func(addr string, status int) string {
		t.Helper()
		r := httptest.NewRequest("GET", mcpAuthorizePath+"?"+query.Encode(), nil)
		r.RemoteAddr = addr
		w := httptest.NewRecorder()
		f.s.authorizeMCP(w, r)
		want(t, "authorization capacity", w.Code, status)
		location := w.Header().Get("Location")
		if status == 503 && location != "" {
			t.Fatalf("capacity error redirected: %s", location)
		}
		return mustURL(location).Query().Get("mcp")
	}
	for i := range mcpMaxRequests {
		if i == mcpRequestsPerIP {
			open("first", 503)
		}
		addr := fmt.Sprint(i)
		if i < mcpRequestsPerIP {
			addr = "first"
		}
		key := open(addr, 303)
		t.Cleanup(func() { f.s.dropMCPRequest(key) })
	}
	open("another", 503)
}

func TestMCPGrantAndFamilyLimits(t *testing.T) {
	f, _ := testMCPFixture(t)
	a := f.mcpApprover("me")
	id := f.registerMCPForTest("none")["client_id"].(string)
	store := f.s.store
	first, _ := a.issue(id)
	x := mcpExchange{Client: id, Resource: f.s.mcp.resource, Redirect: testMCPRedirect, Verifier: testMCPVerifier, Token: first}
	token, _, err := store.mcpExchange(x, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	x.Token, _ = a.issue(id)
	for range mcpFamiliesPerGrant - 1 {
		a.issue(id)
	}
	if _, _, err := store.mcpExchange(x, time.Now()); err != errMCPInvalidGrant {
		t.Fatalf("oldest unredeemed code was not retired: %v", err)
	}
	profile := store.mcpAccess(token.Access, x.Resource, time.Now())
	if profile == "" {
		t.Fatal("an active family was evicted")
	}

	// Fill the rest of the account's grants with client metadata identities.
	store.mu.Lock()
	for i := range mcpGrantsPerAccount - 1 {
		key := fmt.Sprint(i)
		store.Browsers[key] = &BrowserRecord{Owner: f.id("me"), Approved: true, Role: proto.RoleAI, Key: &proto.BrowserKey{}}
		store.MCPGrants[key] = &mcpGrant{User: f.id("me"), Browser: key, Client: "https://app.example/" + key, Resource: x.Resource}
	}
	store.mu.Unlock()
	a.issue(id)
	another := f.registerMCPForTest("none")["client_id"].(string)
	body := mcpJSON[map[string]string](t, a.approval(a.owner, a.open(another), "prepare", nil), 409)
	if body["error"] != "grant_limit" {
		t.Fatal(body)
	}
	if _, err := store.mcpExisting("another account", another, x.Resource); err != nil {
		t.Fatal("another account inherited the grant limit", err)
	}
	for range mcpFamiliesPerGrant - 1 {
		x.Token, _ = a.issue(id)
		if _, _, err := store.mcpExchange(x, time.Now()); err != nil {
			t.Fatal(err)
		}
	}
	request := a.open(id)
	d := a.prepare(request)
	body = mcpJSON[map[string]string](t, a.approval(a.owner, request, "approve", a.proof(d, d.Endorsed+1)), 409)
	if body["error"] != "family_limit" || store.mcpAccess(token.Access, x.Resource, time.Now()) == "" {
		t.Fatal("new approval displaced an active sign-in", body)
	}
}

func TestMCPResourceRateLimitUsesGrant(t *testing.T) {
	f, _ := testMCPFixture(t)
	user := f.id("me")
	resource := f.s.mcp.resource
	var tokens []string
	f.s.store.MCPGrants = make(map[string]*mcpGrant)
	for range 2 {
		profile := randomHex(12)
		token := profile + "." + randomHex(32)
		tokens = append(tokens, token)
		f.s.store.MCPGrants[profile] = &mcpGrant{User: user, Resource: resource,
			Families: []mcpFamily{{Access: hashToken(token), AccessUntil: time.Now().Add(time.Minute).Unix()}}}
	}
	send := func(token, addr string, status int) {
		t.Helper()
		w := httptest.NewRecorder()
		r := httptest.NewRequest("POST", mcpResourcePath, strings.NewReader("{}"))
		r.RemoteAddr = addr
		r.Header.Set("Authorization", "Bearer "+token)
		f.s.serveMCP(w, r)
		want(t, "MCP resource limit", w.Code, status)
	}
	for range 240 {
		send(tokens[0], "proxy", 200)
		send("invalid", "proxy", 401)
	}
	send(tokens[0], "proxy", 429)
	send(tokens[0], "elsewhere", 429)
	send(tokens[1], "proxy", 200)
	send("invalid", "proxy", 429)
	send("invalid", "elsewhere", 401)
}

func TestMCPInitialPasswordGate(t *testing.T) {
	f, _ := testMCPFixture(t)
	a := f.mcpApprover("member")
	cli := f.browser()
	key := newAIDevice(t)
	want(t, "open AI sign-ins", f.status(a.owner, "POST", "/api/ai/window", nil), 200)
	want(t, "AI client asks to sign in", f.status(cli, "POST", "/api/login/ai", map[string]string{"user": "member"}), 202)
	want(t, "attach AI key", f.status(cli, "POST", "/api/login/device", key.key), 200)
	pending := f.s.store.Approvals(f.id("member"), pendingBrowserTTL)
	if len(pending) != 1 {
		t.Fatal(pending)
	}
	want(t, "approve local AI client", f.status(a.owner, "POST", "/api/approvals", map[string]any{
		"kind": "browser", "id": pending[0].ID, "allow": true, "endorse": testProof}), 204)
	want(t, "collect local AI session", f.status(cli, "GET", "/api/login/status", nil), 204)

	client := f.registerMCPForTest("none")["client_id"].(string)
	code, _ := a.issue(client)
	_, _, err := f.s.store.mcpExchange(mcpExchange{Client: client, Resource: f.s.mcp.resource,
		Token: code, Redirect: testMCPRedirect, Verifier: testMCPVerifier}, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	// The remote gateway uses the same signed-in AI signaling route as the local CLI.
	remote := f.browser()
	w := httptest.NewRecorder()
	f.s.issueSession(w, httptest.NewRequest("GET", f.srv.URL, nil), f.s.store.MCPGrants[mcpTokenProfile(code)].Browser)
	remote.Jar.SetCookies(mustURL(f.srv.URL), w.Result().Cookies())
	clients := []*http.Client{cli, remote}
	sockets := make([]*peer, 0, len(clients))
	for _, c := range clients {
		socket := f.browserSocket(f.srv.URL, c)
		socket.next(proto.TypeHello)
		sockets = append(sockets, socket)
	}
	admin := f.signedIn("me")
	want(t, "administrator resets the member password", f.status(admin, "POST", "/api/members/"+f.id("member")+"/password", nil), 200)
	for i, c := range clients {
		want(t, "new signaling connection requires password change", f.status(c, "GET", "/ws/client", nil), 428)
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		err := sockets[i].ws.Ping(ctx)
		cancel()
		if err != nil {
			t.Fatal("existing signaling connection stopped at reset", err)
		}
	}
}
