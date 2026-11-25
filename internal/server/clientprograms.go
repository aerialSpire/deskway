package server

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log"
	"maps"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// clientReleaseStamp is set by the release workflow. A source build serves local files.
var clientReleaseStamp string

const (
	clientFetchTimeout = 30 * time.Second
	clientFetchRetry   = time.Minute
	maxClientFileSize  = 128 << 20
)

// The program for each system, by the names the prompt and the downloads use.
var clientFiles = [...]struct{ OS, Arch, File string }{
	{"windows", "x64", "deskway-windows-x64.exe"},
	{"windows", "arm64", "deskway-windows-arm64.exe"},
	{"macos", "arm64", "deskway-macos-arm64"},
	{"macos", "x64", "deskway-macos-x64"},
}

// ClientFile is one program the server has, as GET /api/ai/clients lists it.
type ClientFile struct {
	OS     string `json:"os"`
	Arch   string `json:"arch"`
	File   string `json:"file"`
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
}

type clientPrograms struct {
	dir, releaseURL string
	hashes          map[string]string

	mu         sync.Mutex
	failures   map[string]clientFailure // Last fetch failure for each unavailable program.
	fileHashes map[string]clientFileHash
}

type clientFileHash struct {
	size int64
	mod  time.Time
	sum  string
}

type clientFailure string

const (
	clientDownloadFailed clientFailure = "download"
	clientChecksumFailed clientFailure = "checksum"
	clientStorageFailed  clientFailure = "storage"
)

// ServeClients configures program distribution and starts fetching before serving HTTP.
// Files placed directly in dir take priority over the release cache.
func (s *Server) ServeClients(dir string, fetch bool) {
	p, err := newClientPrograms(dir, clientReleaseStamp)
	if err != nil {
		log.Printf("server: AI client release stamp: %v", err)
	}
	s.serveClients(p, fetch, clientFetchRetry)
}

func (s *Server) serveClients(p *clientPrograms, fetch bool, retry time.Duration) {
	s.programs = p
	if fetch && p.dir != "" && p.releaseURL != "" {
		s.active.Go(func() { p.fetchMissing(s.ctx, retry) })
	}
}

func newClientPrograms(dir, stamp string) (*clientPrograms, error) {
	p := &clientPrograms{
		dir:        dir,
		failures:   map[string]clientFailure{},
		fileHashes: map[string]clientFileHash{},
	}
	if stamp == "" {
		return p, nil
	}
	b, err := base64.StdEncoding.DecodeString(stamp)
	if err != nil {
		return p, err
	}
	lines := strings.Split(strings.TrimSuffix(string(b), "\n"), "\n")
	if len(lines) < 3 || lines[0] == "" || lines[1] == "" {
		return p, errors.New("missing repository, tag or hashes")
	}
	hashes := make(map[string]string)
	for _, line := range lines[2:] {
		sum, name, _ := strings.Cut(line, " *")
		digest, err := hex.DecodeString(sum)
		if name == "" || err != nil || len(digest) != sha256.Size || hex.EncodeToString(digest) != sum {
			return p, fmt.Errorf("invalid SHA-256 for %s", name)
		}
		hashes[name] = sum
	}
	p.hashes = hashes
	p.releaseURL = "https://github.com/" + lines[0] + "/releases/download/" + url.PathEscape(lines[1])
	return p, nil
}

func (p *clientPrograms) releasePath(name string) string {
	return filepath.Join(p.dir, ".release", name)
}

type clientProgramFile struct {
	*os.File
	info os.FileInfo
	sum  string
}

func (p *clientPrograms) open(name string) (*clientProgramFile, error) {
	if p == nil || p.dir == "" {
		return nil, nil
	}
	f, err := p.openClientFile(filepath.Join(p.dir, name))
	if !errors.Is(err, os.ErrNotExist) {
		return f, err
	}
	expected := p.hashes[name]
	if expected == "" {
		return nil, nil
	}
	f, err = p.openClientFile(p.releasePath(name))
	if err != nil {
		return nil, nil
	}
	if f.sum != expected {
		f.Close()
		return nil, nil
	}
	return f, nil
}

func (p *clientPrograms) openClientFile(path string) (*clientProgramFile, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	st, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if !st.Mode().IsRegular() {
		return nil, os.ErrNotExist
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	info, err := f.Stat()
	if err != nil {
		f.Close()
		return nil, err
	}
	cached, ok := p.fileHashes[path]
	if !ok || cached.size != info.Size() || !cached.mod.Equal(info.ModTime()) {
		h := sha256.New()
		_, err := io.Copy(h, f)
		if err == nil {
			_, err = f.Seek(0, io.SeekStart)
		}
		if err != nil {
			f.Close()
			return nil, err
		}
		cached = clientFileHash{info.Size(), info.ModTime(), hex.EncodeToString(h.Sum(nil))}
		p.fileHashes[path] = cached
	}
	return &clientProgramFile{f, info, cached.sum}, nil
}

func (p *clientPrograms) fetchMissing(ctx context.Context, retry time.Duration) {
	client := &http.Client{Timeout: clientFetchTimeout}
	for {
		retryNeeded := false
		for _, c := range clientFiles {
			if p.hashes[c.File] == "" {
				continue
			}
			if ctx.Err() != nil {
				return
			}
			f, openErr := p.open(c.File)
			var failure clientFailure
			if f != nil {
				f.Close()
			} else if openErr == nil {
				var err error
				failure, err = p.fetch(ctx, client, c.File)
				if err != nil && ctx.Err() == nil {
					log.Printf("server: AI client program %s: %v", c.File, err)
				}
			}
			retryNeeded = retryNeeded || openErr != nil || failure != ""
			p.mu.Lock()
			if failure == "" {
				delete(p.failures, c.File)
			} else {
				p.failures[c.File] = failure
			}
			p.mu.Unlock()
		}
		if !retryNeeded {
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(retry):
		}
	}
}

func (p *clientPrograms) fetch(ctx context.Context, client *http.Client, name string) (clientFailure, error) {
	asset := p.releaseURL + "/" + name
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, asset, nil)
	if err != nil {
		return clientDownloadFailed, err
	}
	res, err := client.Do(req)
	if err != nil {
		return clientDownloadFailed, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		return clientDownloadFailed, fmt.Errorf("download %s: HTTP %s", asset, res.Status)
	}
	path := p.releasePath(name)
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return clientStorageFailed, err
	}
	tmp, err := os.CreateTemp(dir, ".download-*")
	if err != nil {
		return clientStorageFailed, err
	}
	defer os.Remove(tmp.Name())
	h := sha256.New()
	n, err := io.Copy(io.MultiWriter(tmp, h), io.LimitReader(res.Body, maxClientFileSize+1))
	closeErr := tmp.Close()
	if err != nil {
		if _, disk := errors.AsType[*os.PathError](err); disk {
			return clientStorageFailed, err
		}
		return clientDownloadFailed, fmt.Errorf("download %s: %w", asset, err)
	}
	if closeErr != nil {
		return clientStorageFailed, closeErr
	}
	if n > maxClientFileSize {
		return clientDownloadFailed, fmt.Errorf("download %s exceeds %d bytes", asset, maxClientFileSize)
	}
	if actual, expected := hex.EncodeToString(h.Sum(nil)), p.hashes[name]; actual != expected {
		return clientChecksumFailed, fmt.Errorf("SHA-256 mismatch: got %s, want %s", actual, expected)
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if err := os.Rename(tmp.Name(), path); err != nil {
		return clientStorageFailed, err
	}
	delete(p.fileHashes, path)
	return "", nil
}

func (s *Server) handleClients(w http.ResponseWriter, r *http.Request) {
	details := r.URL.Query().Get("details") == "1"
	p := s.programs
	list := []ClientFile{}
	failures := map[string]clientFailure{}
	if details && p != nil {
		p.mu.Lock()
		maps.Copy(failures, p.failures)
		p.mu.Unlock()
	}
	for _, c := range clientFiles {
		f, err := p.open(c.File)
		if f != nil {
			f.Close()
			list = append(list, ClientFile{c.OS, c.Arch, c.File, f.info.Size(), f.sum})
			delete(failures, c.File)
		} else if details && err != nil {
			failures[c.File] = clientStorageFailed
		}
	}
	w.Header().Set("Cache-Control", "no-store")
	if details {
		writeJSON(w, map[string]any{"files": list, "errors": failures})
		return
	}
	writeJSON(w, list)
}

// Downloads are public so a client can get the program before signing in.
func (s *Server) handleDownload(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("file")
	for _, c := range clientFiles {
		if c.File != name {
			continue
		}
		f, _ := s.programs.open(name)
		if f == nil {
			break
		}
		defer f.Close()
		w.Header().Set("Content-Disposition", `attachment; filename="`+name+`"`)
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("ETag", `"`+f.sum+`"`)
		http.ServeContent(w, r, name, f.info.ModTime(), f)
		return
	}
	http.NotFound(w, r)
}
