package server

import (
	"bytes"
	"encoding/base64"
	"encoding/xml"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"deskway/web"
)

func TestLocalGuideRoutes(t *testing.T) {
	ca := testLocalCA(t)
	h, err := ca.GuideHandler(web.Files(), 18499)
	if err != nil {
		t.Fatal(err)
	}
	for _, tt := range []struct {
		method, path string
		status       int
		content      string
	}{
		{"GET", "/", 200, "text/html; charset=utf-8"},
		{"GET", "/local/ca.crt", 200, "application/x-x509-ca-cert"},
		{"GET", "/local/ca.mobileconfig", 200, "application/x-apple-aspen-config"},
		{"GET", "/api/me", 404, ""},
		{"GET", "/setup", 404, ""},
		{"GET", "/local/guide.js", 404, ""},
		{"GET", "/design.css", 404, ""},
		{"GET", "/local/ca.crt/", 404, ""},
		{"POST", "/", 404, ""},
		{"HEAD", "/local/ca.crt", 404, ""},
	} {
		t.Run(tt.method+" "+tt.path, func(t *testing.T) {
			w := httptest.NewRecorder()
			h.ServeHTTP(w, httptest.NewRequest(tt.method, tt.path, nil))
			want(t, tt.path, w.Code, tt.status)
			if tt.content != "" && w.Header().Get("Content-Type") != tt.content {
				t.Fatalf("content type = %q, want %q", w.Header().Get("Content-Type"), tt.content)
			}
			switch {
			case tt.path == "/" && tt.status == 200:
				for _, value := range []string{ca.Fingerprint(), `data-https-port="18499"`, `data-certificate-name="Deskway 客廳 &amp; Office"`, "Trust the Deskway certificate", "shown on the computer running the Deskway server", "data:text/css"} {
					if !strings.Contains(w.Body.String(), value) {
						t.Fatalf("missing %q from guide", value)
					}
				}
			case tt.path == "/local/ca.crt" && tt.status == 200:
				if !bytes.Equal(w.Body.Bytes(), ca.root.Raw) {
					t.Fatal("expected DER CA")
				}
			}
		})
	}
}

func TestLocalHTTPSCertificateRoute(t *testing.T) {
	ca := testLocalCA(t)
	f := emptyFixture(t, func(s *Server) { s.EnableLocalMode(ca) }, httptest.NewTLSServer)
	res := f.do(f.browser(), "GET", "/local/ca.crt", nil)
	want(t, "HTTPS CA download", res.StatusCode, http.StatusOK)
	if res.Header.Get("Content-Type") != "application/x-x509-ca-cert" {
		t.Fatal("expected a CA certificate on HTTPS")
	}
	if res.Header.Get("Strict-Transport-Security") != "" {
		t.Fatal("HSTS would prevent access to the HTTP guide on the same hostname")
	}
	want(t, "relay enrollment disabled", f.status(f.browser(), "POST", "/api/relays/join", nil), http.StatusMethodNotAllowed)
}

func TestMeReportsLocalMode(t *testing.T) {
	for _, local := range []bool{false, true} {
		var fingerprint any
		f := newHTTPFixture(t, func(s *Server) {
			if local {
				ca := testLocalCA(t)
				s.EnableLocalMode(ca)
				fingerprint = ca.Fingerprint()
			}
		}, httptest.NewTLSServer)
		res := f.do(f.signedIn("me"), "GET", "/api/me", nil)
		want(t, "account payload", res.StatusCode, http.StatusOK)
		var me map[string]any
		f.decode(res, &me)
		if me["local_mode"] != local {
			t.Fatalf("local_mode = %v, want %t", me["local_mode"], local)
		}
		if me["local_ca_fingerprint"] != fingerprint {
			t.Fatalf("local_ca_fingerprint = %v, want %v", me["local_ca_fingerprint"], fingerprint)
		}
	}
}

func TestLocalMobileconfig(t *testing.T) {
	ca := testLocalCA(t)
	h, err := ca.GuideHandler(web.Files(), 18499)
	if err != nil {
		t.Fatal(err)
	}
	w := httptest.NewRecorder()
	h.ServeHTTP(w, httptest.NewRequest("GET", "/local/ca.mobileconfig", nil))
	type dictionary struct {
		Keys     []string `xml:"key"`
		Strings  []string `xml:"string"`
		Versions []int    `xml:"integer"`
		Data     []string `xml:"data"`
	}
	var plist struct {
		XMLName xml.Name `xml:"plist"`
		Dict    struct {
			dictionary
			Payloads []dictionary `xml:"array>dict"`
		} `xml:"dict"`
	}
	if err := xml.Unmarshal(w.Body.Bytes(), &plist); err != nil {
		t.Fatal(err)
	}
	profile := plist.Dict
	if len(profile.Payloads) != 1 || len(profile.Strings) != 4 ||
		profile.Strings[0] != "Configuration" || profile.Strings[3] != "Deskway 客廳 & Office" {
		t.Fatalf("invalid configuration profile: %+v", profile)
	}
	cert := profile.Payloads[0]
	if len(cert.Strings) != 5 || cert.Strings[0] != "com.apple.security.root" ||
		cert.Strings[2] == profile.Strings[2] || len(cert.Data) != 1 {
		t.Fatalf("invalid certificate payload: %+v", cert)
	}
	der, err := base64.StdEncoding.DecodeString(cert.Data[0])
	if err != nil || !bytes.Equal(der, ca.root.Raw) {
		t.Fatal("root payload did not contain the CA")
	}
}
