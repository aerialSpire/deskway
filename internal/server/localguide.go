package server

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/xml"
	"fmt"
	"html/template"
	"io/fs"
	"net/http"
	"strings"
)

// GuideHandler serves a standalone HTTP page and the two ways to download the CA.
func (ca *LocalCA) GuideHandler(web fs.FS, httpsPort uint16) (http.Handler, error) {
	page, err := template.ParseFS(web, "local/guide.html")
	if err != nil {
		return nil, err
	}
	dataURL := func(path, mime string) (template.URL, error) {
		body, err := fs.ReadFile(web, path)
		// These URLs contain only embedded assets. The guide needs no extra HTTP routes.
		return template.URL("data:" + mime + ";base64," + base64.StdEncoding.EncodeToString(body)), err
	}
	design, err := dataURL("design.css", "text/css")
	if err != nil {
		return nil, err
	}
	language, err := dataURL("js/language.js", "text/javascript")
	if err != nil {
		return nil, err
	}
	script, err := dataURL("local/guide.js", "text/javascript")
	if err != nil {
		return nil, err
	}
	catalogs := make(map[string]map[string]string)
	for _, lang := range []string{"en", "zh-Hans", "zh-Hant"} {
		body, err := fs.ReadFile(web, "i18n/"+lang+".json")
		if err != nil {
			return nil, err
		}
		var catalog map[string]string
		if err := json.Unmarshal(body, &catalog); err != nil {
			return nil, err
		}
		text := make(map[string]string)
		for key, value := range catalog {
			if strings.HasPrefix(key, "guide.") || key == "common.continue" {
				text[key] = value
			}
		}
		catalogs[lang] = text
	}
	model := struct {
		Design, Language, Script template.URL
		Catalogs                 map[string]map[string]string
		Title, Fingerprint       string
		Port                     uint16
	}{design, language, script, catalogs, catalogs["en"]["guide.title"], ca.Fingerprint(), httpsPort}
	// The template has no request data, so render it once. Only its bootstrap script may run.
	var html bytes.Buffer
	if err := page.Execute(&html, model); err != nil {
		return nil, err
	}
	_, tail, _ := strings.Cut(html.String(), `<script type="module">`)
	bootstrap, _, _ := strings.Cut(tail, "</script>")
	hash := sha256.Sum256([]byte(strings.ReplaceAll(bootstrap, "\r\n", "\n")))
	csp := "default-src 'none'; style-src 'unsafe-inline' data:; script-src data: 'sha256-" +
		base64.StdEncoding.EncodeToString(hash[:]) + "'; frame-ancestors 'none'; base-uri 'none'"

	profile := ca.mobileconfig()
	return securityHeaders(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Cache-Control", "no-store")
		if r.Method == http.MethodGet {
			switch r.URL.Path {
			case "/":
				w.Header().Set("Content-Security-Policy", csp)
				w.Header().Set("Content-Type", "text/html; charset=utf-8")
				_, _ = w.Write(html.Bytes())
				return
			case "/local/ca.crt":
				ca.serveCertificate(w, r)
				return
			case "/local/ca.mobileconfig":
				serveCAFile(w, "deskway-ca.mobileconfig", "application/x-apple-aspen-config", profile)
				return
			}
		}
		http.NotFound(w, r)
	}), false), nil
}

func (ca *LocalCA) serveCertificate(w http.ResponseWriter, _ *http.Request) {
	serveCAFile(w, "deskway-ca.crt", "application/x-x509-ca-cert", ca.cert.Raw)
}

func serveCAFile(w http.ResponseWriter, name, mime string, data []byte) {
	w.Header().Set("Content-Type", mime)
	w.Header().Set("Content-Disposition", "attachment; filename="+name)
	_, _ = w.Write(data)
}

func (ca *LocalCA) mobileconfig() []byte {
	var name bytes.Buffer
	_ = xml.EscapeText(&name, []byte(ca.name))
	fp := ca.Fingerprint()
	digest, _ := hex.DecodeString(fp)
	// Use two stable UUIDs so re-downloading the same CA identifies the same profile and payload.
	uuid := func(b []byte) string {
		b[6] = b[6]&0x0f | 0x80
		b[8] = b[8]&0x3f | 0x80
		return fmt.Sprintf("%x-%x-%x-%x-%x", b[:4], b[4:6], b[6:8], b[8:10], b[10:16])
	}
	return fmt.Appendf(nil, `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadVersion</key><integer>1</integer>
  <key>PayloadIdentifier</key><string>org.deskway.local-ca.%[1]s</string>
  <key>PayloadUUID</key><string>%[2]s</string>
  <key>PayloadDisplayName</key><string>%[4]s</string>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadType</key><string>com.apple.security.root</string>
      <key>PayloadVersion</key><integer>1</integer>
      <key>PayloadIdentifier</key><string>org.deskway.local-ca.%[1]s.certificate</string>
      <key>PayloadUUID</key><string>%[3]s</string>
      <key>PayloadDisplayName</key><string>%[4]s</string>
      <key>PayloadCertificateFileName</key><string>deskway-ca.crt</string>
      <key>PayloadContent</key><data>%[5]s</data>
    </dict>
  </array>
</dict>
</plist>
`, fp, uuid(digest[:16]), uuid(digest[16:]), name.String(), base64.StdEncoding.EncodeToString(ca.cert.Raw))
}
