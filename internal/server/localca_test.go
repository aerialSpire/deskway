package server

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"
)

func testLocalCA(t *testing.T) *LocalCA {
	t.Helper()
	ca, err := OpenLocalCA(t.TempDir(), "客廳 & Office", "sean-asus")
	if err != nil {
		t.Fatal(err)
	}
	return ca
}

func TestLocalCAReuse(t *testing.T) {
	dir := t.TempDir()
	ca, err := OpenLocalCA(dir, "客廳", "hub")
	if err != nil {
		t.Fatal(err)
	}
	again, err := OpenLocalCA(dir, "New name", "new-host")
	if err != nil {
		t.Fatal(err)
	}
	if !ca.root.Equal(again.root) || !ca.intermediate.Equal(again.intermediate) || !ca.key.Equal(again.key) {
		t.Fatal("opening the CA again changed its identity")
	}
	files, err := os.ReadDir(filepath.Join(dir, "local-ca"))
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, file := range files {
		names = append(names, file.Name())
	}
	if !slices.Equal(names, []string{"ca.crt", "intermediate.crt", "intermediate.key"}) {
		t.Fatalf("unexpected persisted CA files: %v", names)
	}
	info, err := files[2].Info()
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("CA key permissions: %o", info.Mode().Perm())
	}
	root := ca.root
	pub, ok := root.PublicKey.(*ecdsa.PublicKey)
	if !ok || pub.Curve != elliptic.P256() || !root.IsCA || root.MaxPathLen != 1 ||
		root.Subject.CommonName != "Deskway 客廳" || root.CheckSignatureFrom(root) != nil ||
		root.PermittedDNSDomainsCritical || len(root.PermittedDNSDomains) != 0 || len(root.PermittedIPRanges) != 0 ||
		!root.NotAfter.Equal(root.NotBefore.AddDate(10, 0, 0)) {
		t.Fatalf("unexpected root certificate: %+v", root)
	}
	if got, want := ca.Fingerprint(), fmt.Sprintf("%x", sha256.Sum256(root.Raw)); got != want {
		t.Fatalf("fingerprint = %q, want %q", got, want)
	}
}

func TestLocalCANameConstraints(t *testing.T) {
	ca := testLocalCA(t)
	crt := ca.intermediate
	var ranges []string
	for _, network := range crt.PermittedIPRanges {
		ranges = append(ranges, network.String())
	}
	if !crt.IsCA || !crt.MaxPathLenZero || crt.CheckSignatureFrom(ca.root) != nil ||
		!crt.PermittedDNSDomainsCritical || !slices.Equal(crt.PermittedDNSDomains, []string{"local", "localhost"}) ||
		!slices.Equal(ranges, []string{"10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "169.254.0.0/16", "::1/128", "fc00::/7", "fe80::/10"}) {
		t.Fatalf("intermediate: pathLen=%d critical=%v DNS=%v IP=%v", crt.MaxPathLen, crt.PermittedDNSDomainsCritical, crt.PermittedDNSDomains, ranges)
	}
}

func TestLocalCARejectsInvalidFiles(t *testing.T) {
	for _, tt := range []struct {
		name, file, wantError string
	}{
		{"root key present", "ca.key", "root private key"},
		{"root from another chain", "ca.crt", "verify local intermediate"},
		{"key from another chain", "intermediate.key", "matching key"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			dir, other := t.TempDir(), t.TempDir()
			for _, dataDir := range []string{dir, other} {
				if _, err := OpenLocalCA(dataDir, "Hub", "hub"); err != nil {
					t.Fatal(err)
				}
			}
			file := tt.file
			if file == "ca.key" {
				file = "intermediate.key"
			}
			data, err := os.ReadFile(filepath.Join(other, "local-ca", file))
			if err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(dir, "local-ca", tt.file), data, 0o600); err != nil {
				t.Fatal(err)
			}
			if _, err := OpenLocalCA(dir, "Hub", "hub"); err == nil || !strings.Contains(err.Error(), tt.wantError) {
				t.Fatalf("OpenLocalCA returned %v, want %q", err, tt.wantError)
			}
		})
	}
}

func TestLocalCARejectsKeyWithGroupOrOtherAccess(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Unix permissions required")
	}
	dir := t.TempDir()
	if _, err := OpenLocalCA(dir, "Hub", "hub"); err != nil {
		t.Fatal(err)
	}
	for _, mode := range []os.FileMode{0o640, 0o604} {
		if err := os.Chmod(filepath.Join(dir, "local-ca", "intermediate.key"), mode); err != nil {
			t.Fatal(err)
		}
		if _, err := OpenLocalCA(dir, "Hub", "hub"); err == nil || !strings.Contains(err.Error(), "owner-only permissions") {
			t.Fatalf("OpenLocalCA with key mode %o returned %v", mode, err)
		}
	}
}

func TestLocalIntermediateRejectsPublicSANs(t *testing.T) {
	ca := testLocalCA(t)
	roots, intermediates := x509.NewCertPool(), x509.NewCertPool()
	roots.AddCert(ca.root)
	intermediates.AddCert(ca.intermediate)
	for _, san := range []localSAN{{dns: "example.com"}, {ip: netip.MustParseAddr("8.8.8.8")}} {
		pair, err := ca.issueLeaf(san, time.Now())
		if err != nil {
			t.Fatal(err)
		}
		_, err = pair.Leaf.Verify(x509.VerifyOptions{Roots: roots, Intermediates: intermediates})
		var invalid x509.CertificateInvalidError
		if !errors.As(err, &invalid) || invalid.Reason != x509.CANotAuthorizedForThisName {
			t.Fatalf("public SAN %+v: expected rejection by the intermediate's name constraints, got %v", san, err)
		}
	}
}

// Only LocalAddr is needed to select an IP SAN.
type localHelloConn struct {
	net.Conn
	addr *net.TCPAddr
}

func (c localHelloConn) LocalAddr() net.Addr { return c.addr }

func localHello(sni, ip string) *tls.ClientHelloInfo {
	addr := net.TCPAddrFromAddrPort(netip.AddrPortFrom(netip.MustParseAddr(ip), 443))
	return &tls.ClientHelloInfo{ServerName: sni, Conn: localHelloConn{addr: addr}}
}

func TestLocalLeafNames(t *testing.T) {
	ca := testLocalCA(t)
	roots := x509.NewCertPool()
	roots.AddCert(ca.root)
	for _, tt := range []struct {
		sni, addr string
		dns, ip   string
	}{
		{"SEAN-ASUS.local.", "192.168.1.2", "sean-asus.local", "192.168.1.2"},
		{"sean_asus.local", "192.168.1.2", "", "192.168.1.2"},
		{"sean-asuſ.local", "192.168.1.2", "", "192.168.1.2"},
		{"localhost", "127.0.0.1", "localhost", "127.0.0.1"},
		{"sean-asus.local", "8.8.8.8", "sean-asus.local", ""},
		{"other.local", "192.168.1.2", "", "192.168.1.2"},
		{"other.sean-asus.local", "192.168.1.2", "", "192.168.1.2"},
		{"other.local", "8.8.8.8", "", ""},
		{"", "10.1.2.3", "", "10.1.2.3"},
		{"", "172.31.0.1", "", "172.31.0.1"},
		{"", "169.254.3.4", "", "169.254.3.4"},
		{"", "::1", "", "::1"},
		{"", "fd00::1", "", "fd00::1"},
		{"", "fe80::a%3", "", "fe80::a"},
		{"", "::ffff:192.168.1.3", "", "192.168.1.3"},
		{"public.example", "192.168.1.2", "", "192.168.1.2"},
		{"public.example", "8.8.8.8", "", ""},
		{"*.local", "8.8.8.8", "", ""},
		{"local", "8.8.8.8", "", ""},
		{"else.localhost", "8.8.8.8", "", ""},
		{"", "172.32.0.1", "", ""},
		{"", "2001:4860::1", "", ""},
	} {
		t.Run(tt.sni+"/"+tt.addr, func(t *testing.T) {
			pair, err := ca.TLSConfig().GetCertificate(localHello(tt.sni, tt.addr))
			if tt.dns == "" && tt.ip == "" {
				if err == nil || pair != nil {
					t.Fatal("issued a leaf without a permitted SAN")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if len(pair.Certificate) != 2 || !bytes.Equal(pair.Certificate[0], pair.Leaf.Raw) ||
				!bytes.Equal(pair.Certificate[1], ca.intermediate.Raw) {
				t.Fatal("expected TLS to serve the leaf followed by the intermediate")
			}
			intermediates := x509.NewCertPool()
			intermediates.AddCert(ca.intermediate)
			crt := pair.Leaf
			var dns, ips []string
			if tt.dns != "" {
				dns = []string{tt.dns}
			}
			if tt.ip != "" {
				ips = []string{tt.ip}
			}
			var gotIPs []string
			for _, ip := range crt.IPAddresses {
				gotIPs = append(gotIPs, ip.String())
			}
			if !slices.Equal(crt.DNSNames, dns) || !slices.Equal(gotIPs, ips) {
				t.Fatalf("SANs DNS=%v IP=%v, want DNS=%v IP=%v", crt.DNSNames, gotIPs, dns, ips)
			}
			if !crt.NotAfter.Equal(crt.NotBefore.AddDate(0, 0, 397)) {
				t.Fatal("expected a 397-day leaf")
			}
			for _, name := range append(dns, ips...) {
				if _, err := crt.Verify(x509.VerifyOptions{Roots: roots, Intermediates: intermediates, DNSName: name}); err != nil {
					t.Fatal(err)
				}
			}
		})
	}
}

func TestListenLocal(t *testing.T) {
	for _, addr := range []string{"127.0.0.1:8080", "[::1]:0", "[::ffff:127.0.0.1]:0", "8.8.8.8:0", "[2001:4860::1]:0"} {
		listener, err := ListenLocal(addr)
		if err == nil {
			listener.Close()
			t.Fatalf("ListenLocal accepted %s", addr)
		}
		if !strings.Contains(err.Error(), "private non-loopback address in local mode") {
			t.Fatalf("ListenLocal(%s): %v", addr, err)
		}
	}
	listener, err := ListenLocal(":0")
	if err != nil {
		t.Fatal(err)
	}
	listener.Close()
}

func TestLocalLeafCacheUnknownNames(t *testing.T) {
	ca := testLocalCA(t)
	const ip = "192.168.1.2"
	first, err := ca.getCertificate(localHello("", ip))
	if err != nil {
		t.Fatal(err)
	}
	for i := range 10 {
		name := fmt.Sprintf("other-%d.local", i)
		got, err := ca.getCertificate(localHello(name, ip))
		if err != nil {
			t.Fatal(err)
		}
		if got != first {
			t.Fatalf("%s did not reuse the IP-only leaf", name)
		}
	}
	if len(ca.leaves) != 1 {
		t.Fatalf("leaf cache contains %d entries, want 1", len(ca.leaves))
	}
}

func TestLocalLeafRenewal(t *testing.T) {
	ca := testLocalCA(t)
	now := time.Now().Truncate(time.Second)
	ca.now = func() time.Time { return now }
	hello := localHello("sean-asus.local", "192.168.1.2")
	first, err := ca.getCertificate(hello)
	if err != nil {
		t.Fatal(err)
	}
	for _, tt := range []struct {
		name  string
		when  time.Time
		reuse bool
	}{
		{"next request", now, true},
		{"30 days left", first.Leaf.NotAfter.Add(-30 * 24 * time.Hour), true},
		{"less than 30 days left", first.Leaf.NotAfter.Add(-30*24*time.Hour + time.Second), false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			now = tt.when
			got, err := ca.getCertificate(hello)
			if err != nil {
				t.Fatal(err)
			}
			if (got == first) != tt.reuse || !tt.reuse && !got.Leaf.NotAfter.After(first.Leaf.NotAfter) {
				t.Fatalf("unexpected cached leaf, expires %v", got.Leaf.NotAfter)
			}
		})
	}
}
