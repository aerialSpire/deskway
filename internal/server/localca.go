package server

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"
)

var localDNS = []string{"local", "localhost"}

var localIPs = []netip.Prefix{
	netip.MustParsePrefix("10.0.0.0/8"),
	netip.MustParsePrefix("172.16.0.0/12"),
	netip.MustParsePrefix("192.168.0.0/16"),
	netip.MustParsePrefix("127.0.0.0/8"),
	netip.MustParsePrefix("169.254.0.0/16"),
	netip.MustParsePrefix("::1/128"),
	netip.MustParsePrefix("fc00::/7"),
	netip.MustParsePrefix("fe80::/10"),
}

// LocalCA owns the local TLS identity and its in-memory leaf cache.
type LocalCA struct {
	cert    *x509.Certificate
	key     *ecdsa.PrivateKey
	name    string
	dnsName string
	now     func() time.Time

	mu     sync.Mutex
	leaves map[localSAN]*tls.Certificate
}

type localSAN struct {
	dns string
	ip  netip.Addr
}

// OpenLocalCA loads or creates the CA beside the state file. An unreadable or incomplete CA
// is an error, since clients may already trust its identity.
func OpenLocalCA(dataDir, name, hostname string) (*LocalCA, error) {
	if name == "" || !utf8.ValidString(name) || utf8.RuneCountInString(name) > 40 ||
		strings.ContainsFunc(name, unicode.IsControl) {
		return nil, errors.New("-local-name must contain 1–40 UTF-8 characters without control characters")
	}
	if !dnsLabel(hostname) {
		return nil, fmt.Errorf("invalid local hostname %q", hostname)
	}
	dir := filepath.Join(dataDir, "local-ca")
	if _, err := os.Lstat(dir); errors.Is(err, os.ErrNotExist) {
		if err := createLocalCA(dir, "Deskway "+name); err != nil {
			return nil, fmt.Errorf("create local CA: %w", err)
		}
	} else if err != nil {
		return nil, err
	}
	pair, err := tls.LoadX509KeyPair(filepath.Join(dir, "ca.crt"), filepath.Join(dir, "ca.key"))
	if err != nil {
		return nil, fmt.Errorf("load local CA: %w", err)
	}
	crt, err := x509.ParseCertificate(pair.Certificate[0])
	if err != nil {
		return nil, fmt.Errorf("parse local CA: %w", err)
	}
	key, ok := pair.PrivateKey.(*ecdsa.PrivateKey)
	if !ok || key.Curve != elliptic.P256() || !crt.IsCA || crt.CheckSignatureFrom(crt) != nil ||
		!crt.PermittedDNSDomainsCritical || !slices.Equal(crt.PermittedDNSDomains, localDNS) ||
		!slices.EqualFunc(crt.PermittedIPRanges, localIPs, func(network *net.IPNet, prefix netip.Prefix) bool {
			return network.String() == prefix.String()
		}) {
		return nil, errors.New("local CA must be a self-signed P-256 CA with Deskway's critical name constraints")
	}
	return &LocalCA{
		cert:    crt,
		key:     key,
		name:    "Deskway " + name,
		dnsName: hostname + ".local",
		now:     time.Now,
		leaves:  make(map[localSAN]*tls.Certificate),
	}, nil
}

func createLocalCA(dir, name string) error {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return err
	}
	now := time.Now()
	crt := &x509.Certificate{
		Subject:                     pkix.Name{CommonName: name},
		NotBefore:                   now,
		NotAfter:                    now.AddDate(10, 0, 0),
		KeyUsage:                    x509.KeyUsageCertSign | x509.KeyUsageCRLSign,
		BasicConstraintsValid:       true,
		IsCA:                        true,
		MaxPathLenZero:              true,
		PermittedDNSDomainsCritical: true,
		PermittedDNSDomains:         localDNS,
	}
	for _, prefix := range localIPs {
		ip := prefix.Addr().AsSlice()
		crt.PermittedIPRanges = append(crt.PermittedIPRanges, &net.IPNet{IP: ip, Mask: net.CIDRMask(prefix.Bits(), len(ip)*8)})
	}
	der, err := x509.CreateCertificate(rand.Reader, crt, crt, &key.PublicKey, key)
	if err != nil {
		return err
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		return err
	}

	// Publish the two files together, so an interrupted first start cannot leave half a CA.
	parent := filepath.Dir(dir)
	if err := os.MkdirAll(parent, 0o700); err != nil {
		return err
	}
	staged, err := os.MkdirTemp(parent, ".local-ca-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staged)
	if err := os.WriteFile(filepath.Join(staged, "ca.key"), pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER}), 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(staged, "ca.crt"), pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0o644); err != nil {
		return err
	}
	if err := os.Rename(staged, dir); err != nil {
		// If another start won the race, load what it published.
		if _, statErr := os.Stat(dir); statErr != nil {
			return err
		}
	}
	return nil
}

// Fingerprint is the lowercase SHA-256 hex digest of the DER CA certificate.
func (ca *LocalCA) Fingerprint() string {
	digest := sha256.Sum256(ca.cert.Raw)
	return hex.EncodeToString(digest[:])
}

// TLSConfig serves leaves for the server's local names and addresses, with the CA in the chain.
func (ca *LocalCA) TLSConfig() *tls.Config {
	return &tls.Config{GetCertificate: ca.getCertificate, NextProtos: []string{"h2", "http/1.1"}}
}

func localIP(ip netip.Addr) bool {
	ip = ip.Unmap().WithZone("")
	return slices.ContainsFunc(localIPs, func(prefix netip.Prefix) bool { return prefix.Contains(ip) })
}

func tcpIP(addr net.Addr) netip.Addr {
	if tcp, ok := addr.(*net.TCPAddr); ok {
		return tcp.AddrPort().Addr().Unmap()
	}
	return netip.Addr{}
}

func (ca *LocalCA) getCertificate(hello *tls.ClientHelloInfo) (*tls.Certificate, error) {
	var san localSAN
	name := strings.Map(lowerASCII, strings.TrimSuffix(hello.ServerName, "."))
	if name == ca.dnsName || name == "localhost" {
		san.dns = name
	}
	if ip := tcpIP(hello.Conn.LocalAddr()); localIP(ip) {
		san.ip = ip.WithZone("")
	}
	if san.dns == "" && !san.ip.IsValid() {
		return nil, errors.New("no permitted local TLS name or address")
	}

	ca.mu.Lock()
	defer ca.mu.Unlock()
	now := ca.now()
	if leaf := ca.leaves[san]; leaf != nil && leaf.Leaf.NotAfter.Sub(now) >= 30*24*time.Hour {
		return leaf, nil
	}
	leaf, err := ca.issueLeaf(san, now)
	if err != nil {
		return nil, err
	}
	ca.leaves[san] = leaf
	return leaf, nil
}

func (ca *LocalCA) issueLeaf(san localSAN, now time.Time) (*tls.Certificate, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, err
	}
	crt := &x509.Certificate{
		NotBefore:   now,
		NotAfter:    now.AddDate(0, 0, 397),
		KeyUsage:    x509.KeyUsageDigitalSignature,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	if san.dns != "" {
		crt.DNSNames = []string{san.dns}
	}
	if san.ip.IsValid() {
		crt.IPAddresses = []net.IP{san.ip.AsSlice()}
	}
	der, err := x509.CreateCertificate(rand.Reader, crt, ca.cert, &key.PublicKey, ca.key)
	if err != nil {
		return nil, err
	}
	parsed, err := x509.ParseCertificate(der)
	if err != nil {
		return nil, err
	}
	return &tls.Certificate{Certificate: [][]byte{der, ca.cert.Raw}, PrivateKey: key, Leaf: parsed}, nil
}

// ListenLocal restricts both ends of the listener to the addresses the local CA permits.
func ListenLocal(addr string) (net.Listener, error) {
	listener, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, err
	}
	ip := tcpIP(listener.Addr())
	if !ip.IsUnspecified() && !localIP(ip) {
		listener.Close()
		return nil, fmt.Errorf("%s is outside the local address ranges", addr)
	}
	return localListener{listener}, nil
}

type localListener struct{ net.Listener }

func (l localListener) Accept() (net.Conn, error) {
	for {
		conn, err := l.Listener.Accept()
		if err != nil {
			return nil, err
		}
		if localIP(tcpIP(conn.LocalAddr())) && localIP(tcpIP(conn.RemoteAddr())) {
			return conn, nil
		}
		conn.Close()
	}
}
