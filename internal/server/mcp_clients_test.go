package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestMCPMetadataClient(t *testing.T) {
	for _, test := range []struct {
		name          string
		status        int
		content, body string
		valid         bool
	}{
		{"valid", 200, "application/json", "", true},
		{"non-HTTPS", 200, "application/json", "", false},
		{"redirect", 302, "application/json", "", false},
		{"too large", 200, "application/json", strings.Repeat("x", mcpMetadataLimit+1), false},
		{"too slow", 200, "application/json", "", false},
		{"malformed", 200, "application/json", `{"client_id":`, false},
		{"wrong client ID", 200, "application/json", `{"client_id":"https://client.example/other"}`, false},
		{"wrong content type", 200, "text/html", "", false},
		{"shared secret", 200, "application/json", `{"client_id":%q,"client_name":"Example","redirect_uris":["https://client.example/callback"],"client_secret":null}`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			var requests atomic.Int32
			server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				w.Header().Set("Content-Type", test.content)
				if test.name == "too slow" {
					<-r.Context().Done()
					return
				}
				if test.status == 302 && r.URL.Path == "/client.json" {
					http.Redirect(w, r, "/redirected", 302)
					return
				}
				id := "https://" + r.Host + "/client.json"
				if test.body != "" {
					if strings.Contains(test.body, "%q") {
						fmt.Fprintf(w, test.body, id)
					} else {
						fmt.Fprint(w, test.body)
					}
					return
				}
				_ = json.NewEncoder(w).Encode(map[string]any{"client_id": id, "client_name": "Example",
					"redirect_uris": []string{"https://client.example/callback"}, "token_endpoint_auth_method": "none"})
			}))
			defer server.Close()
			m := newMCPMetadata("desk.example", "")
			m.http.Transport = server.Client().Transport
			if test.name == "too slow" {
				m.http.Timeout = 60 * time.Millisecond
			}
			id := server.URL + "/client.json"
			if test.name == "non-HTTPS" {
				id = strings.Replace(id, "https:", "http:", 1)
			}
			c, ok := m.get(context.Background(), id)
			if ok != test.valid || ok && (c.Name != "Example" || c.Method != "none") {
				t.Fatalf("got %+v, valid=%v", c, ok)
			}
			if ok {
				_, ok := m.get(context.Background(), id)
				if !ok || requests.Load() != 1 {
					t.Fatal("valid document was not cached")
				}
			}
			if test.name == "redirect" && requests.Load() != 1 || test.name == "non-HTTPS" && requests.Load() != 0 {
				t.Fatalf("unexpected fetch count %d", requests.Load())
			}
		})
	}
}

func TestMCPMetadataRejectsNonPublicAndOwnAddresses(t *testing.T) {
	for _, ip := range []string{"127.0.0.1", "10.0.0.1", "169.254.169.254", "100.64.0.1", "::1",
		"::ffff:192.168.1.1", "fe80::1", "fc00::1", "64:ff9b::c0a8:101", "203.0.113.1"} {
		t.Run(ip, func(t *testing.T) {
			if mcpPublicIP(netip.MustParseAddr(ip)) {
				t.Fatal("nonpublic address allowed")
			}
		})
	}
	for _, test := range []struct{ address, own, publicIP string }{
		{"127.0.0.1:443", "127.0.0.2", ""}, {"11.0.0.1:443", "11.0.0.1", ""},
		{"11.0.0.1:443", "127.0.0.1", "11.0.0.1"},
	} {
		if c, err := dialMCPMetadata(context.Background(), test.address, test.own, test.publicIP); err == nil {
			c.Close()
			t.Fatalf("dialed %s", test.address)
		}
	}
}

func TestMCPRedirectURIs(t *testing.T) {
	c := mcpClient{Redirects: []string{"https://app.example/cb?q=1", "http://127.0.0.1:80/cb", "http://[::1]/cb?x=2"}}
	for _, test := range []struct {
		raw     string
		allowed bool
	}{
		{"https://app.example/cb?q=1", true}, {"https://app.example:443/cb?q=1", false},
		{"https://app.example/%63b?q=1", false}, {"https://app.example/cb?q=2", false},
		{"http://127.0.0.1:34283/cb", true}, {"http://127.0.0.2:34283/cb", false},
		{"http://localhost:34283/cb", false}, {"http://[::1]:34283/cb?x=2", true},
		{"http://::1:34283/cb?x=2", false},
		{"http://[::1]:34283/cb?x=%32", false}, {"http://[::1]:34283/cb?x=2#", false},
	} {
		_, allowed := c.allowsRedirect(test.raw)
		if allowed != test.allowed {
			t.Errorf("%s: %v", test.raw, allowed)
		}
	}
}

func TestMCPRegistrationExpiryAndCapacity(t *testing.T) {
	f, _ := testMCPFixture(t)
	now := time.Now()
	store := f.s.store
	c := mcpClient{Name: "Example application", Redirects: []string{testMCPRedirect}, Method: "none"}
	expired, _, err := store.mcpRegister(c, "one", now.Add(-mcpRegistrationTTL))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := store.mcpClient(expired, now); ok {
		t.Fatal("an unused registration reached its expiry and remained usable")
	}
	touched, _, err := store.mcpRegister(c, "one", now.Add(-mcpRegistrationTTL+time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.mcpUseRegistration(touched, now.Add(-time.Minute)); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name string
		at   time.Time
		ok   bool
	}{
		{"recently used", now.Add(mcpRegistrationTTL - 2*time.Minute), true},
		{"expired since last use", now.Add(mcpRegistrationTTL - time.Minute), false},
	} {
		_, ok := store.mcpClient(touched, test.at)
		if ok != test.ok {
			t.Errorf("%s: registration valid = %t, want %t", test.name, ok, test.ok)
		}
	}
	store.MCPGrants = map[string]*mcpGrant{"profile": {Client: touched}}
	if _, ok := store.mcpClient(touched, now.Add(2*mcpRegistrationTTL)); !ok {
		t.Fatal("a live grant lost its registration")
	}
	for i := range mcpMaxRegistrations - 2 {
		addr := "other"
		if i < mcpRegistrationsIP {
			addr = "one"
		}
		store.MCPClients[fmt.Sprint(i)] = mcpRegistration{mcpClient: c, Address: addr, LastUsed: now.Unix()}
	}
	if _, _, err := store.mcpRegister(c, "one", now); err != errMCPFull {
		t.Fatalf("registration past address capacity: %v", err)
	}
	if _, _, err := store.mcpRegister(c, "another", now); err != nil {
		t.Fatal("expired registration still occupies a slot:", err)
	}
	if _, _, err := store.mcpRegister(c, "another", now); err != errMCPFull {
		t.Fatalf("registration past capacity: %v", err)
	}
}

func TestMCPRegistrationAndClientAuthentication(t *testing.T) {
	f, _ := testMCPFixture(t)
	for _, method := range mcpAuthMethods {
		t.Run(method, func(t *testing.T) {
			reg := f.registerMCPForTest(method)
			id := reg["client_id"].(string)
			secret, _ := reg["client_secret"].(string)
			stored, ok := f.s.store.mcpClient(id, time.Now())
			if !ok || reg["token_endpoint_auth_method"] != method || stored.Method != method ||
				(method == "none") != (secret == "") || secret != "" && stored.SecretHash != hashToken(secret) {
				t.Fatal("registration did not store the advertised authentication method and secret hash")
			}
			for _, valid := range []bool{true, false} {
				if !valid && method == "none" {
					continue
				}
				q := url.Values{"grant_type": {"refresh_token"}, "client_id": {id}, "resource": {f.s.mcp.resource}, "refresh_token": {"inapplicable"}}
				headers := map[string]string{}
				provided := secret
				if !valid {
					provided = "wrong secret"
				}
				switch method {
				case "client_secret_post":
					q.Set("client_secret", provided)
				case "client_secret_basic":
					req := httptest.NewRequest("POST", "/", nil)
					req.SetBasicAuth(id, url.QueryEscape(provided))
					headers["Authorization"] = req.Header.Get("Authorization")
					q.Del("client_id")
				}
				status, code := 400, "invalid_grant"
				if !valid {
					code = "invalid_client"
					if method == "client_secret_basic" {
						status = 401
					}
				}
				r := f.mcpRequest(f.browser(), "POST", mcpTokenPath, "application/x-www-form-urlencoded", q.Encode(), headers)
				if body := mcpJSON[map[string]string](t, r, status); body["error"] != code {
					t.Fatal(body)
				}
			}
		})
	}
}
