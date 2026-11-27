package server

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"deskway/internal/proto"
)

var setupTestKey = proto.BrowserKey{Cred: "Y3JlZDE", Alg: -7, Key: "a2V5MQ==", Name: "iPhone"}

func (f *fixture) newMemberLink(admin *http.Client, id string) string {
	f.t.Helper()
	res := f.do(admin, "POST", "/api/members/"+id+"/link", nil)
	want(f.t, "new link", res.StatusCode, http.StatusOK)
	var body struct{ Link string }
	f.decode(res, &body)
	return body.Link
}

func TestMemberLinks(t *testing.T) {
	for _, action := range []string{"redeem", "expire", "revoke", "supersede"} {
		t.Run(action, func(t *testing.T) {
			f := newFixture(t)
			admin := f.signedIn("me")
			id, link := f.create(admin, "friend")
			code := setupCode(link)
			if decoded, err := base64.RawURLEncoding.DecodeString(code); err != nil || len(decoded) != 32 {
				t.Fatalf("invalid code: %v", err)
			}
			data, err := os.ReadFile(f.data + ".links")
			if err != nil || strings.Contains(string(data), code) || !strings.Contains(string(data), hashToken(code)) {
				t.Fatalf("the links file must keep only the hash: %v", err)
			}

			checkStatus := func(kind string, expires bool, needsSetup bool) {
				t.Helper()
				var members []Member
				f.decode(f.do(admin, "GET", "/api/members", nil), &members)
				if len(members) != 1 || members[0].LinkKind != kind || (members[0].LinkExpiry != 0) != expires || members[0].NeedsSetup != needsSetup {
					t.Fatalf("member link state: %+v", members)
				}
				if kind != "" && time.Until(time.Unix(members[0].LinkExpiry, 0)) < memberLinkTTL-time.Minute {
					t.Fatal("member link expires too soon")
				}
			}
			checkStatus(linkSetup, true, true)
			browser := f.browser()
			body := setupRequest{Code: code, Password: testPassword, Device: &setupTestKey}
			info := "/api/setup?c=" + hashToken(code)
			switch action {
			case "redeem":
				want(t, "account has no password", f.status(browser, "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusUnauthorized)
				var details setupDetails
				res := f.do(browser, "GET", info, nil)
				want(t, "inspect", res.StatusCode, http.StatusOK)
				f.decode(res, &details)
				if details != (setupDetails{Kind: linkSetup, Name: "friend"}) {
					t.Fatalf("details: %+v", details)
				}
				want(t, "redeem", f.status(browser, "POST", "/api/setup", body), http.StatusNoContent)
				want(t, "signed in", f.status(browser, "GET", "/api/me", nil), http.StatusOK)
				want(t, "a separate browser waits", f.status(f.browser(), "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusAccepted)
				checkStatus("", false, false)
			case "expire":
				links, err := openLinks(f.data)
				if err != nil {
					t.Fatal(err)
				}
				record := links.records[id]
				record.Expires = time.Now().Add(-time.Second).Unix()
				links.records[id] = record
				err = links.save()
				links.Close()
				if err != nil {
					t.Fatal(err)
				}
			case "revoke":
				want(t, "revoke", f.status(admin, "DELETE", "/api/members/"+id+"/link", nil), http.StatusNoContent)
				checkStatus("", false, true)
			case "supersede":
				replacement := f.newMemberLink(admin, id)
				if replacement == link {
					t.Fatal("the new link must differ")
				}
				checkStatus(linkSetup, true, true)
				want(t, "replacement works", f.status(browser, "POST", "/api/setup", setupRequest{Code: setupCode(replacement), Password: testPassword, Device: &setupTestKey}), http.StatusNoContent)
			}
			want(t, "invalid link", f.status(f.browser(), "POST", "/api/setup", body), http.StatusGone)
			want(t, "invalid link details", f.status(f.browser(), "GET", info, nil), http.StatusGone)
			if action == "expire" {
				checkStatus("", true, true)
				data, err := os.ReadFile(f.data + ".links")
				if err != nil || strings.Contains(string(data), hashToken(code)) {
					t.Fatalf("expired credential was not pruned: %v", err)
				}
			}
		})
	}
}

func TestResetKeepsDevicesAndApprovesOnlyTheRedeemingBrowser(t *testing.T) {
	f := newFixture(t)
	admin := f.signedIn("me")
	existing := f.signedIn("friend")
	id := f.id("friend")
	f.s.store.putDevice("pc", &DeviceRecord{Owner: id, Name: "Friend's PC", TokenHash: hashToken("pc-token")})

	redeem, other := f.browser(), f.browser()
	for _, c := range []*http.Client{redeem, other} {
		want(t, "waiting", f.status(c, "POST", "/api/login", map[string]string{"user": "friend", "password": testPassword}), http.StatusAccepted)
	}
	key := setupTestKey
	code := setupCode(f.newMemberLink(admin, id))
	info := f.do(redeem, "GET", "/api/setup?c="+hashToken(code), nil)
	var details setupDetails
	f.decode(info, &details)
	if details.Kind != linkReset || details.Name != "friend" {
		t.Fatalf("reset details: %+v", details)
	}
	const newPassword = "new password"
	if !f.s.store.PasswordOK(id, testPassword) {
		t.Fatal("issuing the reset changed the password")
	}
	want(t, "reset redeems", f.status(redeem, "POST", "/api/setup", setupRequest{Code: code, Password: newPassword, Device: &key}), http.StatusNoContent)
	want(t, "existing session kept", f.status(existing, "GET", "/api/me", nil), http.StatusOK)
	want(t, "redeeming browser signed in", f.status(redeem, "GET", "/api/me", nil), http.StatusOK)
	want(t, "other browser still pending", f.status(other, "GET", "/api/login/status", nil), http.StatusAccepted)
	want(t, "other browser has no session", f.status(other, "GET", "/api/me", nil), http.StatusUnauthorized)
	want(t, "approved browsers", len(f.s.store.ApprovedBrowsers(id)), 2)
	if !f.s.store.DeviceTokenOK("pc", "pc-token") || !f.s.store.PasswordOK(id, newPassword) || f.s.store.PasswordOK(id, testPassword) {
		t.Fatal("reset should change the password and keep the computer")
	}
	var saved Store
	if err := readState(f.data, &saved); err != nil {
		t.Fatal(err)
	}
	for _, c := range redeem.Jar.Cookies(mustURL(f.srv.URL)) {
		if c.Name == browserCookie {
			b := saved.Browsers[hashToken(c.Value)]
			if b == nil || !b.Approved || b.Key == nil || *b.Key != key {
				t.Fatalf("expected an approved browser with the submitted key: %+v", b)
			}
		}
	}
	log := saved.Users[id].DeviceLog
	if len(log) != 1 || log[0].Op != proto.OpAdd || log[0].Cred != key.Cred || log[0].By != "" {
		t.Fatalf("expected one unsigned add: %+v", log)
	}
}

func TestAdminLinksWithOpenStore(t *testing.T) {
	f := emptyFixture(t, nil, httptest.NewServer)
	if err := os.WriteFile(f.data, []byte("unreadable as account state"), 0o600); err != nil {
		t.Fatal(err)
	}
	code := func() string {
		t.Helper()
		path, err := IssueAdminLink(f.data)
		if err != nil {
			t.Fatal(err)
		}
		return setupCode(path)
	}
	c := f.browser()
	first := code()
	second := code()
	res := f.do(c, "GET", "/api/setup?c="+hashToken(second), nil)
	var details setupDetails
	f.decode(res, &details)
	if details != (setupDetails{Kind: linkSetup}) {
		t.Fatalf("admin setup details: %+v", details)
	}
	links, err := openLinks(f.data)
	if err != nil {
		t.Fatal(err)
	}
	expiry := time.Until(time.Unix(links.records[adminLinkID].Expires, 0))
	links.Close()
	if expiry > adminLinkTTL || expiry < adminLinkTTL-time.Minute {
		t.Fatalf("admin link lifetime: %v", expiry)
	}
	for _, trial := range []struct {
		name    string
		request setupRequest
		status  int
	}{
		{"superseded", setupRequest{Code: first, Password: testPassword, Name: "me"}, http.StatusGone},
		{"missing device", setupRequest{Code: second, Password: testPassword, Name: "me"}, http.StatusUnprocessableEntity},
		{"invalid device", setupRequest{Code: second, Password: testPassword, Name: "me", Device: &proto.BrowserKey{}}, http.StatusUnprocessableEntity},
		{"missing name", setupRequest{Code: second, Password: testPassword, Device: &setupTestKey}, http.StatusBadRequest},
		{"short password", setupRequest{Code: second, Password: "short", Name: "me", Device: &setupTestKey}, http.StatusBadRequest},
		{"long password", setupRequest{Code: second, Password: strings.Repeat("界", 25), Name: "me", Device: &setupTestKey}, http.StatusBadRequest},
		{"create admin", setupRequest{Code: second, Password: strings.Repeat("界", 24), Name: "me", Display: "Admin", Device: &setupTestKey}, http.StatusNoContent},
		{"used", setupRequest{Code: second, Password: testPassword, Name: "me2"}, http.StatusGone},
	} {
		want(t, trial.name, f.status(c, "POST", "/api/setup", trial.request), trial.status)
	}
	id := f.s.store.ID("me")
	if !f.s.store.IsAdmin(id) || f.s.store.Display(id) != "Admin" || f.s.store.ID("me2") != "" {
		t.Fatal("expected exactly the admin created by setup")
	}
	want(t, "admin signed in", f.status(c, "GET", "/api/members", nil), http.StatusOK)
	reset := code()
	f.decode(f.do(c, "GET", "/api/setup?c="+hashToken(reset), nil), &details)
	if details != (setupDetails{Kind: linkReset, Name: "me"}) {
		t.Fatalf("admin reset details: %+v", details)
	}
	want(t, "admin reset", f.status(f.browser(), "POST", "/api/setup", setupRequest{Code: reset, Password: "new password", Device: &setupTestKey}), http.StatusNoContent)
	if f.s.store.ID("me") != id || !f.s.store.PasswordOK(id, "new password") {
		t.Fatal("reset must keep the admin account")
	}
}

func TestSetupRequiresSameOriginJSON(t *testing.T) {
	f := newFixture(t)
	link, err := IssueAdminLink(f.data)
	if err != nil {
		t.Fatal(err)
	}
	body, err := json.Marshal(setupRequest{Code: setupCode(link), Password: testPassword, Device: &setupTestKey})
	if err != nil {
		t.Fatal(err)
	}
	for _, trial := range []struct {
		site, content string
		status        int
	}{
		{"cross-site", "application/json", http.StatusForbidden},
		{"same-site", "application/json", http.StatusForbidden},
		{"same-origin", "text/plain", http.StatusUnsupportedMediaType},
		{"", "application/json; charset=utf-8", http.StatusNoContent},
	} {
		req := httptest.NewRequest("POST", "/api/setup", bytes.NewReader(body))
		if trial.site != "" {
			req.Header.Set("Sec-Fetch-Site", trial.site)
		}
		req.Header.Set("Content-Type", trial.content)
		res := httptest.NewRecorder()
		f.s.Handler().ServeHTTP(res, req)
		want(t, trial.site+" "+trial.content, res.Code, trial.status)
	}
}

func TestLinkRedemptionIsRateLimited(t *testing.T) {
	f := newFixture(t)
	c := f.browser()
	for range 4 {
		want(t, "wrong password", f.status(c, "POST", "/api/login", map[string]string{"user": "me", "password": "nope"}), http.StatusUnauthorized)
	}
	link, err := IssueAdminLink(f.data)
	if err != nil {
		t.Fatal(err)
	}
	info := "/api/setup?c=" + hashToken(setupCode(link))
	want(t, "lookup before lockout", f.status(c, "GET", info, nil), http.StatusOK)
	want(t, "bad code", f.status(c, "POST", "/api/setup", setupRequest{Code: "nope", Password: testPassword}), http.StatusGone)
	want(t, "lookup during lockout", f.status(c, "GET", info, nil), http.StatusOK)
	res := f.do(c, "POST", "/api/setup", setupRequest{Code: setupCode(link), Password: testPassword})
	want(t, "valid code at locked address", res.StatusCode, http.StatusTooManyRequests)
	if res.Header.Get("Retry-After") == "" || len(f.s.store.ApprovedBrowsers(f.id("me"))) != 0 {
		t.Fatal("a blocked request must have no effect and say when to retry")
	}
}
