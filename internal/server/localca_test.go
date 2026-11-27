package server

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"runtime"
	"slices"
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
	if !ca.cert.Equal(again.cert) || !ca.key.Equal(again.key) {
		t.Fatal("opening the CA again changed its identity")
	}
	info, err := os.Stat(filepath.Join(dir, "local-ca", "ca.key"))
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("CA key permissions: %o", info.Mode().Perm())
	}
	pub, ok := ca.cert.PublicKey.(*ecdsa.PublicKey)
	if !ok || pub.Curve != elliptic.P256() || !ca.cert.IsCA || ca.cert.Subject.CommonName != "Deskway 客廳" ||
		!ca.cert.NotAfter.Equal(ca.cert.NotBefore.AddDate(10, 0, 0)) {
		t.Fatalf("unexpected CA: subject %s, public key %T, validity %v–%v", ca.cert.Subject, ca.cert.PublicKey, ca.cert.NotBefore, ca.cert.NotAfter)
	}
	if got, want := ca.Fingerprint(), fmt.Sprintf("%x", sha256.Sum256(ca.cert.Raw)); got != want {
		t.Fatalf("fingerprint = %q, want %q", got, want)
	}
}

func TestLocalCANameConstraints(t *testing.T) {
	ca := testLocalCA(t)
	var ranges []string
	for _, network := range ca.cert.PermittedIPRanges {
		ranges = append(ranges, network.String())
	}
	if !ca.cert.PermittedDNSDomainsCritical || !slices.Equal(ca.cert.PermittedDNSDomains, []string{"local", "localhost"}) ||
		!slices.Equal(ranges, []string{"10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "169.254.0.0/16", "::1/128", "fc00::/7", "fe80::/10"}) {
		t.Fatalf("constraints: critical=%v DNS=%v IP=%v", ca.cert.PermittedDNSDomainsCritical, ca.cert.PermittedDNSDomains, ranges)
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
	roots.AddCert(ca.cert)
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
				if _, err := crt.Verify(x509.VerifyOptions{Roots: roots, DNSName: name}); err != nil {
					t.Fatal(err)
				}
			}
		})
	}
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
