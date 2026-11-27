package server

import (
	"bytes"
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

const localCertBackdate = time.Hour

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
	root         *x509.Certificate
	intermediate *x509.Certificate
	key          *ecdsa.PrivateKey
	dnsName      string
	now          func() time.Time

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
	if _, err := os.Lstat(filepath.Join(dir, "ca.key")); err == nil {
		return nil, errors.New("local CA contains a root private key (ca.key); local mode requires a new CA identity")
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	root, err := readCACertificate(filepath.Join(dir, "ca.crt"))
	if err != nil {
		return nil, fmt.Errorf("load local root CA: %w", err)
	}
	intermediate, err := readCACertificate(filepath.Join(dir, "intermediate.crt"))
	if err != nil {
		return nil, fmt.Errorf("load local intermediate CA: %w", err)
	}
	keyPEM, err := readLocalKey(filepath.Join(dir, "intermediate.key"))
	if err != nil {
		return nil, fmt.Errorf("load local intermediate key: %w", err)
	}
	keyDER, err := decodeLocalPEM(keyPEM, "EC PRIVATE KEY")
	if err != nil {
		return nil, fmt.Errorf("load local intermediate key: %w", err)
	}
	key, err := x509.ParseECPrivateKey(keyDER)
	if err != nil {
		return nil, fmt.Errorf("parse local intermediate key: %w", err)
	}
	rootKey, ok := root.PublicKey.(*ecdsa.PublicKey)
	if !ok || rootKey.Curve != elliptic.P256() || !root.IsCA || !root.BasicConstraintsValid ||
		root.KeyUsage&x509.KeyUsageCertSign == 0 || root.MaxPathLen != 1 ||
		!bytes.Equal(root.RawIssuer, root.RawSubject) || root.CheckSignatureFrom(root) != nil ||
		!root.NotAfter.Equal(root.NotBefore.AddDate(10, 0, 0)) {
		return nil, errors.New("local root must be a self-signed P-256 CA with pathLen 1 and a ten-year lifetime")
	}
	if key.Curve != elliptic.P256() || !key.PublicKey.Equal(intermediate.PublicKey) ||
		key.PublicKey.Equal(rootKey) || !intermediate.IsCA || !intermediate.BasicConstraintsValid ||
		intermediate.KeyUsage&x509.KeyUsageCertSign == 0 || !intermediate.MaxPathLenZero ||
		!intermediate.PermittedDNSDomainsCritical || !slices.Equal(intermediate.PermittedDNSDomains, localDNS) ||
		len(intermediate.ExcludedDNSDomains) != 0 || len(intermediate.ExcludedIPRanges) != 0 ||
		len(intermediate.PermittedEmailAddresses) != 0 || len(intermediate.ExcludedEmailAddresses) != 0 ||
		len(intermediate.PermittedURIDomains) != 0 || len(intermediate.ExcludedURIDomains) != 0 ||
		!slices.EqualFunc(intermediate.PermittedIPRanges, localIPs, func(network *net.IPNet, prefix netip.Prefix) bool {
			return network.String() == prefix.String()
		}) {
		return nil, errors.New("local intermediate must be a distinct P-256 CA with a matching key, pathLen 0 and Deskway's critical name constraints")
	}
	if len(intermediate.ExtKeyUsage) != 1 || intermediate.ExtKeyUsage[0] != x509.ExtKeyUsageServerAuth ||
		len(intermediate.UnknownExtKeyUsage) != 0 {
		return nil, errors.New("local intermediate must have only the serverAuth extended key usage")
	}
	roots := x509.NewCertPool()
	roots.AddCert(root)
	if _, err := intermediate.Verify(x509.VerifyOptions{Roots: roots}); err != nil {
		return nil, fmt.Errorf("verify local intermediate against root: %w", err)
	}
	return &LocalCA{
		root:         root,
		intermediate: intermediate,
		key:          key,
		dnsName:      hostname + ".local",
		now:          time.Now,
		leaves:       make(map[localSAN]*tls.Certificate),
	}, nil
}

func decodeLocalPEM(data []byte, kind string) ([]byte, error) {
	data = bytes.TrimSpace(data)
	block, rest := pem.Decode(data)
	if !bytes.HasPrefix(data, []byte("-----BEGIN "+kind+"-----")) ||
		block == nil || block.Type != kind || len(bytes.TrimSpace(rest)) != 0 {
		return nil, fmt.Errorf("expected one PEM block of type %s", kind)
	}
	return block.Bytes, nil
}

func readCACertificate(path string) (*x509.Certificate, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	der, err := decodeLocalPEM(data, "CERTIFICATE")
	if err != nil {
		return nil, err
	}
	return x509.ParseCertificate(der)
}

func createLocalCA(dir, name string) error {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return err
	}
	rootDER, intermediateDER, err := createLocalChain(name, &key.PublicKey)
	if err != nil {
		return err
	}
	keyDER, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		return err
	}

	// Publish the full CA together, so an interrupted first start cannot leave half a chain.
	parent := filepath.Dir(dir)
	if err := os.MkdirAll(parent, 0o700); err != nil {
		return err
	}
	staged, err := os.MkdirTemp(parent, ".local-ca-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staged)
	if err := writeLocalKey(filepath.Join(staged, "intermediate.key"), pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: keyDER})); err != nil {
		return err
	}
	for _, cert := range []struct {
		file string
		der  []byte
	}{{"ca.crt", rootDER}, {"intermediate.crt", intermediateDER}} {
		if err := os.WriteFile(filepath.Join(staged, cert.file), pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: cert.der}), 0o644); err != nil {
			return err
		}
	}
	if err := os.Rename(staged, dir); err != nil {
		// If another start won the race, load what it published.
		if _, statErr := os.Stat(dir); statErr != nil {
			return err
		}
	}
	return nil
}

// The root key exists only here, to create the trust anchor and sign one intermediate.
func createLocalChain(name string, intermediateKey *ecdsa.PublicKey) ([]byte, []byte, error) {
	rootKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	// The whole chain must be valid for clients with a slow clock, even on first startup.
	notBefore := time.Now().UTC().Add(-localCertBackdate)
	root := &x509.Certificate{
		Subject:               pkix.Name{CommonName: name},
		NotBefore:             notBefore,
		NotAfter:              notBefore.AddDate(10, 0, 0),
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageCRLSign,
		BasicConstraintsValid: true,
		IsCA:                  true,
		MaxPathLen:            1,
	}
	rootDER, err := x509.CreateCertificate(rand.Reader, root, root, &rootKey.PublicKey, rootKey)
	if err != nil {
		return nil, nil, err
	}
	root, err = x509.ParseCertificate(rootDER)
	if err != nil {
		return nil, nil, err
	}
	intermediate := &x509.Certificate{
		Subject:                     pkix.Name{CommonName: name + " Intermediate CA"},
		NotBefore:                   root.NotBefore,
		NotAfter:                    root.NotAfter,
		KeyUsage:                    x509.KeyUsageCertSign | x509.KeyUsageCRLSign,
		ExtKeyUsage:                 []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid:       true,
		IsCA:                        true,
		MaxPathLenZero:              true,
		PermittedDNSDomainsCritical: true,
		PermittedDNSDomains:         localDNS,
	}
	for _, prefix := range localIPs {
		ip := prefix.Addr().AsSlice()
		intermediate.PermittedIPRanges = append(intermediate.PermittedIPRanges, &net.IPNet{IP: ip, Mask: net.CIDRMask(prefix.Bits(), len(ip)*8)})
	}
	intermediateDER, err := x509.CreateCertificate(rand.Reader, intermediate, root, intermediateKey, rootKey)
	return rootDER, intermediateDER, err
}

// Fingerprint is the lowercase SHA-256 hex digest of the DER root certificate.
func (ca *LocalCA) Fingerprint() string {
	digest := sha256.Sum256(ca.root.Raw)
	return hex.EncodeToString(digest[:])
}

// TLSConfig serves leaves for the server's local names and addresses, followed by the intermediate.
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
	notBefore := now.Add(-localCertBackdate)
	crt := &x509.Certificate{
		NotBefore:   notBefore,
		NotAfter:    notBefore.AddDate(0, 0, 397),
		KeyUsage:    x509.KeyUsageDigitalSignature,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	if san.dns != "" {
		crt.DNSNames = []string{san.dns}
	}
	if san.ip.IsValid() {
		crt.IPAddresses = []net.IP{san.ip.AsSlice()}
	}
	der, err := x509.CreateCertificate(rand.Reader, crt, ca.intermediate, &key.PublicKey, ca.key)
	if err != nil {
		return nil, err
	}
	parsed, err := x509.ParseCertificate(der)
	if err != nil {
		return nil, err
	}
	return &tls.Certificate{Certificate: [][]byte{der, ca.intermediate.Raw}, PrivateKey: key, Leaf: parsed}, nil
}

func resolveLocalAddr(addr string) (*net.TCPAddr, error) {
	tcp, err := net.ResolveTCPAddr("tcp", addr)
	if err != nil {
		return nil, err
	}
	ip := tcpIP(tcp)
	if tcp.IP != nil && !ip.IsUnspecified() && (ip.IsLoopback() || !localIP(ip)) {
		return nil, fmt.Errorf("%s must use an unspecified address (e.g. :%d) or a private non-loopback address in local mode", addr, tcp.Port)
	}
	return tcp, nil
}

func resolveLocalListeners(listen, guideListen string) (*net.TCPAddr, *net.TCPAddr, error) {
	https, err := resolveLocalAddr(listen)
	if err != nil {
		return nil, nil, fmt.Errorf("-listen: %w", err)
	}
	guide, err := resolveLocalAddr(guideListen)
	if err != nil {
		return nil, nil, fmt.Errorf("-guide-listen: %w", err)
	}
	httpsIP, guideIP := tcpIP(https), tcpIP(guide)
	httpsWildcard := https.IP == nil || httpsIP.IsUnspecified()
	guideWildcard := guide.IP == nil || guideIP.IsUnspecified()
	if httpsWildcard != guideWildcard || !httpsWildcard && httpsIP != guideIP {
		return nil, nil, errors.New("in local mode, -listen and -guide-listen must both use unspecified addresses or the same private non-loopback IP")
	}
	if https.Port != 0 && https.Port == guide.Port {
		return nil, nil, errors.New("in local mode, -listen and -guide-listen must use different ports")
	}
	return https, guide, nil
}

// ListenLocal opens the HTTPS and guide listeners and restricts both ends to local addresses.
func ListenLocal(listen, guideListen string) (net.Listener, net.Listener, error) {
	httpsAddr, guideAddr, err := resolveLocalListeners(listen, guideListen)
	if err != nil {
		return nil, nil, err
	}
	https, err := net.ListenTCP("tcp", httpsAddr)
	if err != nil {
		return nil, nil, fmt.Errorf("-listen: %w", err)
	}
	guide, err := net.ListenTCP("tcp", guideAddr)
	if err != nil {
		https.Close()
		return nil, nil, fmt.Errorf("-guide-listen: %w", err)
	}
	return localListener{https}, localListener{guide}, nil
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
