package server

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

// clientReleaseStamp is set by the release workflow. Source builds have no release stamp.
var clientReleaseStamp string

const (
	releaseFetchTimeout    = 30 * time.Second
	maxReleaseAssetSize    = 128 << 20
	maxReleaseMetadataSize = 1 << 20
)

type releaseStamp struct {
	repo, tag string
	hashes    map[string]string
}

func parseReleaseStamp(stamp string) (releaseStamp, error) {
	raw, err := base64.StdEncoding.DecodeString(stamp)
	if err != nil {
		return releaseStamp{}, err
	}
	repo, rest, _ := strings.Cut(string(raw), "\n")
	tag, sums, _ := strings.Cut(rest, "\n")
	if repo == "" || tag == "" || sums == "" {
		return releaseStamp{}, errors.New("missing repository, tag or hashes")
	}
	hashes, err := parseReleaseChecksums(sums)
	return releaseStamp{repo, tag, hashes}, err
}

func parseReleaseChecksums(sums string) (map[string]string, error) {
	hashes := make(map[string]string)
	for line := range strings.SplitSeq(strings.TrimSuffix(sums, "\n"), "\n") {
		sum, name, _ := strings.Cut(line, " ")
		digest, err := hex.DecodeString(sum)
		// GNU sha256sum separates the digest and name with a space and a mode marker.
		if len(name) < 2 || (name[0] != ' ' && name[0] != '*') || err != nil || len(digest) != sha256.Size || hex.EncodeToString(digest) != sum {
			return nil, fmt.Errorf("invalid SHA-256 entry %q", line)
		}
		hashes[name[1:]] = sum
	}
	return hashes, nil
}

func releaseAssetURL(repo, tag string) string {
	return "https://github.com/" + repo + "/releases/download/" + url.PathEscape(tag)
}

type releaseFailure string

const (
	releaseDownloadFailed releaseFailure = "download"
	releaseChecksumFailed releaseFailure = "checksum"
	releaseStorageFailed  releaseFailure = "storage"
)

// downloadRelease writes a bounded response to dst and checks its SHA-256 when one is supplied.
func downloadRelease(ctx context.Context, client *http.Client, asset string, dst io.Writer, limit int64, expected string) (releaseFailure, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, asset, nil)
	if err != nil {
		return releaseDownloadFailed, err
	}
	res, err := client.Do(req)
	if err != nil {
		return releaseDownloadFailed, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		return releaseDownloadFailed, fmt.Errorf("download %s: HTTP %s", asset, res.Status)
	}
	h := sha256.New()
	if expected != "" {
		dst = io.MultiWriter(dst, h)
	}
	n, err := io.Copy(dst, io.LimitReader(res.Body, limit+1))
	if err != nil {
		if _, disk := errors.AsType[*os.PathError](err); disk {
			return releaseStorageFailed, err
		}
		return releaseDownloadFailed, fmt.Errorf("download %s: %w", asset, err)
	}
	if n > limit {
		return releaseDownloadFailed, fmt.Errorf("download %s exceeds %d bytes", asset, limit)
	}
	if expected != "" {
		if actual := hex.EncodeToString(h.Sum(nil)); actual != expected {
			return releaseChecksumFailed, fmt.Errorf("SHA-256 mismatch: got %s, want %s", actual, expected)
		}
	}
	return "", nil
}
