package main

import (
	"bytes"
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"deskway/internal/server"
)

func TestAdminLinkIfNeeded(t *testing.T) {
	for _, tc := range []struct {
		name     string
		state    string
		wantLink bool
	}{
		{"admin has a password", `{"users":{"operator":{"name":"me","admin":true,"hash":"password-hash"}}}`, false},
		{"admin has no password", `{"users":{"operator":{"name":"me","admin":true}}}`, true},
		{"old admin with an unused initial password", `{"users":{"operator":{"name":"me","admin":true,"hash":"initial-hash","must_change":true}}}`, true},
		{"admin absent", `{"users":{"member":{"name":"friend","hash":"password-hash"}}}`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "state.json")
			if err := os.WriteFile(path, []byte(tc.state), 0o600); err != nil {
				t.Fatal(err)
			}
			store, err := server.OpenStore(path)
			if err != nil {
				t.Fatal(err)
			}
			defer store.Close()

			const origin = "https://desk.example.net"
			var out bytes.Buffer
			if code := adminLinkCommand([]string{"-if-needed", "-data", path, "-url", origin}, &out); code != 0 {
				t.Fatalf("exit code = %d", code)
			}
			if !tc.wantLink {
				if out.Len() != 0 {
					t.Fatal("command printed a link for an admin with a password")
				}
				if _, err := os.Stat(path + ".links"); !os.IsNotExist(err) {
					t.Fatalf("expected no links file, stat returned %v", err)
				}
				return
			}
			code, hasPrefix := strings.CutPrefix(out.String(), origin+"/setup#c=")
			token, err := base64.RawURLEncoding.DecodeString(strings.TrimSuffix(code, "\n"))
			if !hasPrefix || err != nil || len(token) != 32 || !strings.HasSuffix(code, "\n") {
				t.Fatal("expected one setup link")
			}
		})
	}
}
