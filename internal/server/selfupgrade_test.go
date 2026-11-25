package server

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

const testServerPackage = "deskway-server-linux-amd64"

func serverReleaseArchive(t *testing.T, executable string) []byte {
	t.Helper()
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tarball := tar.NewWriter(gz)
	if err := tarball.WriteHeader(&tar.Header{
		Name: testServerPackage + "/deskway-server", Mode: 0o755, Size: int64(len(executable)),
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := io.WriteString(tarball, executable); err != nil {
		t.Fatal(err)
	}
	if err := tarball.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

type localReleaseTransport struct {
	server *httptest.Server
	urls   []string
}

func (t *localReleaseTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.urls = append(t.urls, req.URL.String())
	local := req.Clone(req.Context())
	local.URL.Scheme = "http"
	local.URL.Host = strings.TrimPrefix(t.server.URL, "http://")
	return t.server.Client().Transport.RoundTrip(local)
}

func TestUpgradeExecutable(t *testing.T) {
	const oldBinary = "old executable"
	const newBinary = "new executable"
	const tag = "v2"
	const repo = "owner/project"
	const asset = testServerPackage + ".tar.gz"
	archive := serverReleaseArchive(t, newBinary)
	for _, tc := range []struct {
		name, target string
		mismatch     bool
	}{
		{name: "latest"},
		{name: "explicit tag", target: tag},
		{name: "checksum mismatch", target: tag, mismatch: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			digest := sha256.Sum256(archive)
			if tc.mismatch {
				digest = sha256.Sum256([]byte("different archive"))
			}
			const latestPath = "/repos/" + repo + "/releases/latest"
			const releasePath = "/" + repo + "/releases/download/" + tag + "/"
			release := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch r.URL.Path {
				case latestPath:
					fmt.Fprintf(w, `{"tag_name":%q}`, tag)
				case releasePath + "SHA256SUMS":
					fmt.Fprintf(w, "%x  %s\n", digest, asset)
				case releasePath + asset:
					w.Write(archive)
				default:
					http.NotFound(w, r)
				}
			}))
			defer release.Close()
			transport := &localReleaseTransport{server: release}
			client := &http.Client{Transport: transport}
			dir := t.TempDir()
			path := filepath.Join(dir, "deskway-server")
			if err := os.WriteFile(path, []byte(oldBinary), 0o755); err != nil {
				t.Fatal(err)
			}

			version, err := upgradeExecutable(t.Context(), client, path, repo, tc.target, testServerPackage)
			wantBinary := newBinary
			if tc.mismatch {
				wantBinary = oldBinary
				if err == nil || !strings.Contains(err.Error(), "SHA-256 mismatch") {
					t.Fatalf("want checksum mismatch, got %v", err)
				}
			} else if err != nil || version != tag {
				t.Fatalf("version %q, error %v", version, err)
			}
			const base = "https://github.com" + releasePath
			wantURLs := []string{base + "SHA256SUMS", base + asset}
			if tc.target == "" {
				wantURLs = append([]string{"https://api.github.com" + latestPath}, wantURLs...)
			}
			if !slices.Equal(transport.urls, wantURLs) {
				t.Fatalf("requests %v, want %v", transport.urls, wantURLs)
			}
			got, err := os.ReadFile(path)
			if err != nil || string(got) != wantBinary {
				t.Fatalf("executable %q, want %q, error %v", got, wantBinary, err)
			}
			if files, err := os.ReadDir(dir); err != nil || len(files) != 1 {
				t.Fatalf("executable directory has leftover files: %v, %v", files, err)
			}
		})
	}
}

func TestUpgradeRefusesSourceBuild(t *testing.T) {
	if _, err := Upgrade(t.Context(), "v2"); !errors.Is(err, ErrUpgradeSourceBuild) {
		t.Fatalf("source build upgrade: %v", err)
	}
}
