package server

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"log"
	"maps"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"time"
)

const clientFetchRetry = time.Minute

// The program for each system, by the names the prompt and the downloads use.
var clientFiles = [...]struct{ OS, Arch, File string }{
	{"windows", "x64", "deskway-windows-x64.exe"},
	{"windows", "arm64", "deskway-windows-arm64.exe"},
	{"macos", "arm64", "deskway-macos-arm64"},
	{"macos", "x64", "deskway-macos-x64"},
	{"linux", "x64", "deskway-linux-x64"},
	{"linux", "arm64", "deskway-linux-arm64"},
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
	failures   map[string]releaseFailure // Last fetch failure for each unavailable program.
	fileHashes map[string]clientFileHash
}

type clientFileHash struct {
	size int64
	mod  time.Time
	sum  string
}

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
		failures:   map[string]releaseFailure{},
		fileHashes: map[string]clientFileHash{},
	}
	if stamp == "" {
		return p, nil
	}
	release, err := parseReleaseStamp(stamp)
	if err != nil {
		return p, err
	}
	p.hashes = release.hashes
	p.releaseURL = releaseAssetURL(release.repo, release.tag)
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
	client := &http.Client{Timeout: releaseFetchTimeout}
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
			var failure releaseFailure
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

func (p *clientPrograms) fetch(ctx context.Context, client *http.Client, name string) (releaseFailure, error) {
	path := p.releasePath(name)
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return releaseStorageFailed, err
	}
	tmp, err := os.CreateTemp(dir, ".download-*")
	if err != nil {
		return releaseStorageFailed, err
	}
	defer os.Remove(tmp.Name())
	failure, err := downloadRelease(ctx, client, p.releaseURL+"/"+name, tmp, maxReleaseAssetSize, p.hashes[name])
	closeErr := tmp.Close()
	if err != nil {
		return failure, err
	}
	if closeErr != nil {
		return releaseStorageFailed, closeErr
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if err := os.Rename(tmp.Name(), path); err != nil {
		return releaseStorageFailed, err
	}
	delete(p.fileHashes, path)
	return "", nil
}

func (s *Server) handleClients(w http.ResponseWriter, r *http.Request) {
	details := r.URL.Query().Get("details") == "1"
	p := s.programs
	list := []ClientFile{}
	failures := map[string]releaseFailure{}
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
			failures[c.File] = releaseStorageFailed
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
