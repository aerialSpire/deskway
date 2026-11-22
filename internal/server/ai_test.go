package server

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"os"
	"path/filepath"
	"testing"
	"time"

	"deskway/internal/proto"
)

// A proof of "me"'s browser for whatever needs one.
var testProof = map[string]any{"ts": 1, "by": "Y3JlZDE", "sig": "cw"}

// aiDevice is a test AI client's key pair: what it attaches, and its signature as deskway signs.
type aiDevice struct {
	key  map[string]any
	priv *ecdsa.PrivateKey
}

func newAIDevice(t *testing.T) aiDevice {
	t.Helper()
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	der, _ := x509.MarshalPKIXPublicKey(&priv.PublicKey)
	return aiDevice{map[string]any{"cred": "YWkx", "alg": -7, "key": base64.StdEncoding.EncodeToString(der), "name": "Claude Code", "role": "ai"}, priv}
}

// sign signs a message as deskway does: ECDSA P-256 over the SHA-256 of its SHA-256, r||s.
func (d aiDevice) sign(t *testing.T, message string) string {
	t.Helper()
	inner := sha256.Sum256([]byte(message))
	digest := sha256.Sum256(inner[:])
	r, s, err := ecdsa.Sign(rand.Reader, d.priv, digest[:])
	if err != nil {
		t.Fatal(err)
	}
	raw := make([]byte, 64)
	r.FillBytes(raw[:32])
	s.FillBytes(raw[32:])
	return base64.RawURLEncoding.EncodeToString(raw)
}

// aiSignedIn is an AI client of "me" with the key of d, approved by its browser `by` with a
// proof, after `by` opened the account to AI sign-ins.
func (f *fixture) aiSignedIn(by *http.Client, d aiDevice) *http.Client {
	f.t.Helper()
	c := f.browser()
	want(f.t, "the account takes AI sign-ins", f.status(by, "POST", "/api/ai/window", nil), http.StatusOK)
	want(f.t, "an AI client waits for approval", f.status(c, "POST", "/api/login/ai", map[string]any{"user": "me"}), http.StatusAccepted)
	want(f.t, "it attaches its key", f.status(c, "POST", "/api/login/device", d.key), http.StatusOK)
	reqs := f.pending()
	if len(reqs) != 1 {
		f.t.Fatalf("waiting: %+v", reqs)
	}
	want(f.t, "approved", f.status(by, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": reqs[0].ID, "allow": true, "endorse": testProof}), http.StatusNoContent)
	want(f.t, "collects its session", f.status(c, "GET", "/api/login/status", nil), http.StatusNoContent)
	return c
}

func TestAnAIClientIsApprovedByABrowserWithItsRoleInTheLog(t *testing.T) {
	f := newFixture(t)
	me := f.signedIn("me")
	d := newAIDevice(t)
	c := f.browser()
	want(t, "the account takes AI sign-ins", f.status(me, "POST", "/api/ai/window", nil), http.StatusOK)
	want(t, "an AI client asks", f.status(c, "POST", "/api/login/ai", map[string]any{"user": "me"}), http.StatusAccepted)
	asBrowser := map[string]any{"cred": "YWkx", "alg": -7, "key": d.key["key"], "name": "x"}
	want(t, "an AI client's record takes no browser key", f.status(c, "POST", "/api/login/device", asBrowser), http.StatusNotFound)
	var got struct{ Code string }
	_ = json.NewDecoder(f.do(c, "POST", "/api/login/device", d.key).Body).Decode(&got)
	if want := (proto.BrowserKey{Key: d.key["key"].(string), Role: proto.RoleAI}).Code(); got.Code != want || f.pending()[0].Code != want {
		t.Fatalf("code %s, approval %s: want the AI client's code-v2 %s", got.Code, f.pending()[0].Code, want)
	}
	want(t, "approved", f.status(me, "POST", "/api/approvals", map[string]any{"kind": "browser", "id": f.pending()[0].ID, "allow": true, "endorse": testProof}), http.StatusNoContent)
	l, _ := f.s.store.DeviceLog(f.id("me"))
	if e := l[len(l)-1]; e.Op != proto.OpAdd || e.Cred != "YWkx" || e.Role != proto.RoleAI || e.By != "Y3JlZDE" {
		t.Fatalf("log: %+v", e)
	}
}

func TestAnAIClientsLoginReachesOnlyWhatItNeeds(t *testing.T) {
	f := newFixture(t)
	me := f.signedIn("me")
	ai := f.aiSignedIn(me, newAIDevice(t))
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), Name: "PC"})
	for _, r := range []struct {
		method, path string
		body         any
		status       int
	}{
		{"GET", "/api/me", nil, http.StatusOK},
		{"GET", "/api/devices", nil, http.StatusOK},
		{"GET", "/api/ai/clients", nil, http.StatusOK},
		{"POST", "/api/approvals", map[string]any{"kind": "browser", "id": "x", "allow": true}, http.StatusForbidden},
		{"POST", "/api/policy", map[string]any{"keys": false, "proof": testProof}, http.StatusForbidden},
		{"POST", "/api/computers/pc1/ai", map[string]any{"ai": true, "proof": testProof}, http.StatusForbidden},
		{"DELETE", "/api/browsers/x", nil, http.StatusForbidden},
		{"PATCH", "/api/browsers/x", map[string]any{"name": "x"}, http.StatusForbidden},
		{"DELETE", "/api/computers/pc1", nil, http.StatusForbidden},
		{"POST", "/api/password", map[string]any{}, http.StatusForbidden},
		{"PATCH", "/api/me", map[string]any{"display": "x"}, http.StatusForbidden},
		{"POST", "/api/ai/window", nil, http.StatusForbidden},
	} {
		want(t, r.method+" "+r.path, f.status(ai, r.method, r.path, r.body), r.status)
	}
}

// Without the password: a new AI client only while a browser of the account has opened it to
// AI sign-ins, never for an unknown name; an approved one with its key's signature, any time,
// each signature once.
func TestAnAIClientSignsInWithoutThePassword(t *testing.T) {
	f := newFixture(t)
	me := f.signedIn("me")
	until := func() int64 {
		var w struct{ Until int64 }
		_ = json.NewDecoder(f.do(me, "GET", "/api/ai/window", nil).Body).Decode(&w)
		return w.Until
	}
	if until() != 0 {
		t.Fatal("the account takes AI sign-ins before anyone opened it")
	}
	want(t, "a new AI client while the account does not take them", f.status(f.browser(), "POST", "/api/login/ai", map[string]any{"user": "me"}), http.StatusForbidden)
	want(t, "a browser opens the account to AI sign-ins", f.status(me, "POST", "/api/ai/window", nil), http.StatusOK)
	if u := until(); u < time.Now().Add(aiWindowTTL-time.Minute).Unix() || u > time.Now().Add(aiWindowTTL).Unix() {
		t.Fatalf("open until %d", u)
	}
	want(t, "an unknown account, answered as one that does not take them", f.status(f.browser(), "POST", "/api/login/ai", map[string]any{"user": "nobody"}), http.StatusForbidden)
	stale := f.browser()
	want(t, "a renewal by a device not approved here, while the account takes AI sign-ins, is refused", f.status(stale, "POST", "/api/login/ai", map[string]any{"user": "me", "renew": true}), http.StatusUnauthorized)
	if len(f.pending()) != 0 {
		t.Fatalf("a renewal asked for approval: %+v", f.pending())
	}
	d := newAIDevice(t)
	ai := f.aiSignedIn(me, d)

	// Its session gone: it signs in again with its key, even once the account no longer takes new ones.
	f.s.ai.mu.Lock()
	f.s.ai.open = map[string]time.Time{}
	f.s.ai.mu.Unlock()
	base, _ := url.Parse(f.srv.URL)
	again := func() *http.Client {
		jar, _ := cookiejar.New(nil)
		for _, ck := range ai.Jar.Cookies(base) {
			if ck.Name != sessionCookie {
				jar.SetCookies(base, []*http.Cookie{ck})
			}
		}
		return &http.Client{Jar: jar}
	}
	now := time.Now().Unix()
	other := newAIDevice(t)
	for _, c := range []struct {
		name   string
		ts     int64
		by     aiDevice
		user   string
		status int
	}{
		{"signed now", now, d, "me", http.StatusNoContent},
		{"the same signature again", now, d, "me", http.StatusUnauthorized},
		{"an earlier time", now - 1, d, "me", http.StatusUnauthorized},
		{"too long ago", now - aiSigSkew - 60, d, "me", http.StatusUnauthorized},
		{"another key", now + 1, other, "me", http.StatusUnauthorized},
		{"signed for another name", now + 2, d, "you", http.StatusForbidden},
		{"signed later", now + 3, d, "me", http.StatusNoContent},
	} {
		c := c
		f.s.login.reset("127.0.0.1")
		got := f.status(again(), "POST", "/api/login/ai", map[string]any{"user": c.user, "ts": c.ts, "sig": c.by.sign(t, proto.AILogin(c.user, c.ts))})
		want(t, c.name, got, c.status)
	}
}

// The AI client programs: listed with their SHA-256 for the account, handed out to anyone by
// name, nothing else from the folder.
func TestTheServerHandsOutTheAIClientPrograms(t *testing.T) {
	f := newFixture(t)
	me := f.signedIn("me")
	dir := t.TempDir()
	files := map[string]string{"deskway-macos-arm64": "mac program", "deskway-linux-x64": "not one of them", "deskway-windows-x64.exe": "windows program", "notes.txt": "not a program"}
	for name, body := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	f.s.ServeClients(dir)
	want(t, "the list needs a signed-in browser", f.status(f.browser(), "GET", "/api/ai/clients", nil), http.StatusUnauthorized)
	var list []ClientFile
	_ = json.NewDecoder(f.do(me, "GET", "/api/ai/clients", nil).Body).Decode(&list)
	sum := func(s string) string { h := sha256.Sum256([]byte(s)); return hex.EncodeToString(h[:]) }
	if len(list) != 2 || list[0].File != "deskway-windows-x64.exe" || list[0].SHA256 != sum("windows program") || list[1].File != "deskway-macos-arm64" || list[1].OS != "macos" || list[1].SHA256 != sum("mac program") {
		t.Fatalf("listed %+v", list)
	}
	for _, c := range []struct {
		file   string
		status int
	}{
		{"deskway-macos-arm64", http.StatusOK},
		{"deskway-macos-x64", http.StatusNotFound},
		{"deskway-linux-x64", http.StatusNotFound},
		{"notes.txt", http.StatusNotFound},
	} {
		want(t, "download "+c.file, f.status(f.browser(), "GET", "/download/"+c.file, nil), c.status)
	}
}

func TestAnAIClientSendsOnlyWhatASessionNeeds(t *testing.T) {
	offer := func(k string) proto.Msg {
		return proto.Msg{Type: proto.TypeOffer, Auth: json.RawMessage(`{"k":"` + k + `"}`)}
	}
	for _, c := range []struct {
		m  proto.Msg
		ok bool
	}{
		{proto.Msg{Type: proto.TypeHello}, true},
		{proto.Msg{Type: proto.TypeChallenge}, true},
		{offer("dk"), true},
		{offer("rs"), false},
		{proto.Msg{Type: proto.TypeOffer}, false},
		{proto.Msg{Type: proto.TypeRestart}, true},
		{proto.Msg{Type: proto.TypeCandidate}, true},
		{proto.Msg{Type: proto.TypeBye}, true},
		{proto.Msg{Type: proto.TypeAttach}, true},
		{proto.Msg{Type: proto.TypePair}, false},
		{proto.Msg{Type: proto.TypePairAsk}, false},
		{proto.Msg{Type: proto.TypeWatch}, false},
	} {
		if got := aiMay(c.m); got != c.ok {
			t.Errorf("%s %s: %v, want %v", c.m.Type, c.m.Auth, got, c.ok)
		}
	}
}

func TestTheAISwitchIsKeptPerComputerAndOnlyANewerOneReplacesIt(t *testing.T) {
	f := newFixture(t)
	me := f.signedIn("me")
	other := f.signedIn("other")
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), Name: "PC"})
	at := func(ts int64, ai, admin bool) map[string]any {
		return map[string]any{"ai": ai, "admin": admin, "proof": map[string]any{"ts": ts, "by": "Y3JlZDE", "sig": "cw"}}
	}
	want(t, "no proof", f.status(me, "POST", "/api/computers/pc1/ai", map[string]any{"ai": true}), http.StatusBadRequest)
	want(t, "another account's computer", f.status(other, "POST", "/api/computers/pc1/ai", at(5, true, false)), http.StatusNotFound)
	want(t, "on", f.status(me, "POST", "/api/computers/pc1/ai", at(5, true, true)), http.StatusNoContent)
	want(t, "an older one", f.status(me, "POST", "/api/computers/pc1/ai", at(4, false, false)), http.StatusConflict)
	_, owned := f.s.store.DeviceLog(f.id("me"))
	if sw := owned["pc1"]; sw == nil || sw.Host != "pc1" || !sw.AI || !sw.Admin || sw.TS != 5 || sw.By != "Y3JlZDE" {
		t.Fatalf("the computer would get %+v", sw)
	}
	list := f.s.store.DevicesFor(f.id("me"))
	if v := list[0].AI; v == nil || *v != (proto.AiView{State: proto.AIOn, Admin: true, TS: 5}) {
		t.Fatalf("the account sees %+v", v)
	}
}

func TestTheAccountSeesTheSwitchAsTheComputerHoldsItAndWhoUsesIt(t *testing.T) {
	on := &proto.AiView{State: proto.AIOn, TS: 5}
	claude := &proto.Holder{Session: "A1", Name: "Claude", Since: 9}
	for _, c := range []struct {
		name   string
		sw     *proto.AiView
		state  string
		holder *proto.Holder
		want   *proto.AiView
	}{
		{"never set, an old computer", nil, "", nil, nil},
		{"never set, the computer says off", nil, proto.AIOff, nil, &proto.AiView{State: proto.AIOff}},
		{"on, as signed", on, "", nil, on},
		{"stopped at the computer", on, proto.AIStopped, nil, &proto.AiView{State: proto.AIStopped, TS: 5}},
		{"in use", on, proto.AIOn, claude, &proto.AiView{State: proto.AIOn, TS: 5, Name: "Claude", Since: 9}},
	} {
		got := aiView(c.sw, c.state, c.holder)
		if (got == nil) != (c.want == nil) || (got != nil && *got != *c.want) {
			t.Errorf("%s: %+v, want %+v", c.name, got, c.want)
		}
	}
}
