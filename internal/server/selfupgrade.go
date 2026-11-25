package server

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
)

var (
	ErrUpgradeSourceBuild = errors.New("source build has no release stamp")
	ErrUpgradeDirectory   = errors.New("cannot write to the executable's directory")
)

// Upgrade replaces the running executable with the verified binary from a release.
func Upgrade(ctx context.Context, tag string) (string, error) {
	if clientReleaseStamp == "" {
		return "", ErrUpgradeSourceBuild
	}
	stamp, err := parseReleaseStamp(clientReleaseStamp)
	if err != nil {
		return "", fmt.Errorf("invalid release stamp: %w", err)
	}
	if runtime.GOOS != "linux" || (runtime.GOARCH != "amd64" && runtime.GOARCH != "arm64") {
		return "", fmt.Errorf("no server release for %s/%s", runtime.GOOS, runtime.GOARCH)
	}
	path, err := os.Executable()
	if err != nil {
		return "", err
	}
	path, err = filepath.EvalSymlinks(path)
	if err != nil {
		return "", err
	}
	client := &http.Client{Timeout: releaseFetchTimeout}
	return upgradeExecutable(ctx, client, path, stamp.repo, tag, "deskway-server-"+runtime.GOOS+"-"+runtime.GOARCH)
}

func upgradeExecutable(ctx context.Context, client *http.Client, path, repo, tag, packageName string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	next, err := os.CreateTemp(filepath.Dir(path), ".deskway-server-*")
	if err != nil {
		return "", fmt.Errorf("%w: %w", ErrUpgradeDirectory, err)
	}
	defer os.Remove(next.Name())
	defer next.Close()

	if tag == "" {
		var metadata bytes.Buffer
		asset := "https://api.github.com/repos/" + repo + "/releases/latest"
		if _, err := downloadRelease(ctx, client, asset, &metadata, maxReleaseMetadataSize, ""); err != nil {
			return "", err
		}
		var latest struct {
			Tag string `json:"tag_name"`
		}
		if err := json.Unmarshal(metadata.Bytes(), &latest); err != nil {
			return "", err
		}
		tag = latest.Tag
		if tag == "" {
			return "", errors.New("latest release has no tag")
		}
	}
	base := releaseAssetURL(repo, tag)
	var sums bytes.Buffer
	if _, err := downloadRelease(ctx, client, base+"/SHA256SUMS", &sums, maxReleaseMetadataSize, ""); err != nil {
		return "", err
	}
	hashes, err := parseReleaseChecksums(sums.String())
	if err != nil {
		return "", err
	}
	name := packageName + ".tar.gz"
	if hashes[name] == "" {
		return "", fmt.Errorf("SHA256SUMS has no entry for %s", name)
	}
	archive, err := os.CreateTemp("", "deskway-server-*.tar.gz")
	if err != nil {
		return "", err
	}
	defer os.Remove(archive.Name())
	defer archive.Close()
	if _, err := downloadRelease(ctx, client, base+"/"+name, archive, maxReleaseAssetSize, hashes[name]); err != nil {
		return "", err
	}
	if _, err := archive.Seek(0, io.SeekStart); err != nil {
		return "", err
	}
	if err := extractServerExecutable(next, archive, packageName+"/deskway-server"); err != nil {
		return "", err
	}
	if err := next.Chmod(info.Mode().Perm()); err != nil {
		return "", err
	}
	if err := next.Close(); err != nil {
		return "", err
	}
	if err := os.Rename(next.Name(), path); err != nil {
		if errors.Is(err, os.ErrPermission) {
			return "", fmt.Errorf("%w: %w", ErrUpgradeDirectory, err)
		}
		return "", err
	}
	return tag, nil
}

func extractServerExecutable(dst io.Writer, src io.Reader, name string) error {
	data, err := gzip.NewReader(src)
	if err != nil {
		return err
	}
	defer data.Close()
	archive := tar.NewReader(data)
	for {
		entry, err := archive.Next()
		if err == io.EOF {
			return fmt.Errorf("archive has no %s", name)
		}
		if err != nil {
			return err
		}
		if entry.Name == name {
			if entry.Typeflag != tar.TypeReg || entry.Size <= 0 || entry.Size > maxReleaseAssetSize {
				return fmt.Errorf("invalid executable in archive: %s", name)
			}
			_, err := io.Copy(dst, archive)
			return err
		}
	}
}
