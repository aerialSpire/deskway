// Command deskway-server runs the Deskway rendezvous server.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"golang.org/x/crypto/acme/autocert"

	"deskway/internal/server"
	"deskway/web"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "admin-link":
			os.Exit(adminLinkCommand(os.Args[2:], os.Stdout))
		case "-supports-upgrade":
			// The installer probes with a flag so older servers exit without starting.
			return
		case "upgrade":
			os.Exit(upgradeCommand(os.Args[2:]))
		case "upgrade-check":
			os.Exit(upgradeCheckCommand(os.Args[2:]))
		case "remote-mcp-check":
			os.Exit(remoteMCPCheckCommand(os.Args[2:]))
		case "relay":
			os.Exit(relayCommand(os.Args[2:]))
		}
	}
	flag.Usage = func() {
		fmt.Fprintln(flag.CommandLine.Output(), "Usage: deskway-server [flags]")
		fmt.Fprintln(flag.CommandLine.Output(), "       deskway-server admin-link [-if-needed | -login] [-data FILE] -url ORIGIN")
		fmt.Fprintln(flag.CommandLine.Output(), "       deskway-server upgrade-check [-data FILE]")
		fmt.Fprintln(flag.CommandLine.Output(), "       deskway-server remote-mcp-check [-data FILE] [-clients DIR]")
		fmt.Fprintln(flag.CommandLine.Output(), "       "+upgradeUsage)
		fmt.Fprintln(flag.CommandLine.Output(), "       "+relayUsage)
		flag.PrintDefaults()
	}
	dataPath, clients, fetch := clientOptions(flag.CommandLine)
	var (
		listen    = flag.String("listen", "127.0.0.1:8080", "HTTP or HTTPS listen address")
		local     = flag.Bool("local", false, "use LAN HTTPS with a local CA, without STUN or TURN")
		guideAddr = flag.String("guide-listen", "", "local mode's plain HTTP certificate guide listen address")
		localName = flag.String("local-name", "", "local mode's advertised computer name (up to 40 characters; defaults to the hostname)")
		remoteMCP = flag.Bool("remote-mcp", false, "allow applications to connect to this server over MCP with OAuth")
		mcpOrigin = flag.String("mcp-public-url", "", "public HTTPS origin for remote MCP when the server cannot infer it")
		domain    = flag.String("domain", "", "domain name; gets a Let's Encrypt certificate automatically (listen on :443)")
		tlsCert   = flag.String("tls-cert", "", "TLS certificate file")
		tlsKey    = flag.String("tls-key", "", "TLS key file")
		turnHost  = flag.String("public-host", "", "public TURN hostname for a TLS certificate without a single concrete DNS name")
		publicIP  = flag.String("public-ip", "", "public IP; enables the TURN relay")
		turnPort  = flag.Int("turn-port", turnDefaultPort, "TURN port (UDP and TCP)")
		turnMin   = flag.Int("turn-min-port", turnDefaultMin, "lowest TURN relay port")
		turnMax   = flag.Int("turn-max-port", turnDefaultMax, "highest TURN relay port")
		turnPriv  = flag.Bool("turn-allow-private-for-tests", false, "let TURN reach private addresses; for tests on one machine only")
		stun      = flag.String("stun", "", "comma-separated STUN URLs when TURN is off")
		relayMbps = flag.Int("relay-account-mbps", 30, "each account's share of the TURN relay, both directions together, in Mbit/s")
	)
	flag.Parse()
	if flag.NArg() != 0 {
		flag.Usage()
		os.Exit(2)
	}
	var https, guide net.Listener
	if *local {
		if *domain != "" || *tlsCert != "" || *tlsKey != "" || *publicIP != "" || *stun != "" || *remoteMCP {
			log.Fatal("-local cannot be combined with -domain, -tls-cert, -tls-key, -public-ip, -stun, or -remote-mcp")
		}
		if *guideAddr == "" {
			log.Fatal("-local requires -guide-listen")
		}
		var err error
		https, guide, err = server.ListenLocal(*listen, *guideAddr)
		if err != nil {
			log.Fatal(err)
		}
	} else if *guideAddr != "" || *localName != "" {
		log.Fatal("-guide-listen and -local-name require -local")
	}

	// The server holds the state file for as long as it runs.
	store, err := server.OpenStore(*dataPath)
	if errors.Is(err, server.ErrLocked) {
		log.Fatalf("%s is in use: another deskway-server is running", *dataPath)
	}
	if err != nil {
		log.Fatal(err)
	}
	defer store.Close()
	if !*local && !store.HasAdmin() {
		log.Printf("no admin yet, run admin-link as the service user, e.g. sudo -u deskway deskway-server admin-link -data %q -url https://YOUR_DOMAIN", *dataPath)
	}

	var (
		tlsConfig   *tls.Config
		guideServer *http.Server
		ca          *server.LocalCA
		hostname    string
	)
	switch {
	case *local:
		hostname, err = server.LocalHostname()
		if err != nil {
			log.Fatal(err)
		}
		if *localName == "" {
			*localName = hostname[:min(len(hostname), 40)]
		}
		ca, err = server.OpenLocalCA(filepath.Dir(*dataPath), *localName, hostname)
		if err != nil {
			log.Fatal(err)
		}
		httpsPort := uint16(https.Addr().(*net.TCPAddr).Port)
		handler, err := ca.GuideHandler(web.Files(), httpsPort)
		if err != nil {
			log.Fatal(err)
		}
		guideServer = &http.Server{Handler: handler, ReadHeaderTimeout: 10 * time.Second}
		tlsConfig = ca.TLSConfig()
	case *domain != "":
		// The certificate comes through the HTTPS port itself, so port 80 can stay closed.
		m := &autocert.Manager{
			Prompt:     autocert.AcceptTOS,
			HostPolicy: autocert.HostWhitelist(*domain),
			Cache:      autocert.DirCache(filepath.Join(filepath.Dir(*dataPath), "certs")),
		}
		tlsConfig = m.TLSConfig()
	case *tlsCert != "":
		cert, err := tls.LoadX509KeyPair(*tlsCert, *tlsKey)
		if err != nil {
			log.Fatal(err)
		}
		tlsConfig = &tls.Config{Certificates: []tls.Certificate{cert}, NextProtos: []string{"h2", "http/1.1"}}
	}

	turnCfg := server.TURNConfig{PublicIP: *publicIP, Port: *turnPort, Secret: store.TURNSecret, MinPort: uint16(*turnMin), MaxPort: uint16(*turnMax), AllowPrivatePeers: *turnPriv, AccountMbps: *relayMbps}
	if *publicIP != "" {
		var secureRelay net.Listener
		if tlsConfig != nil {
			host := *domain
			if host == "" {
				host, err = turnTLSHost(tlsConfig.Certificates[0], *turnHost)
				if err != nil {
					log.Fatal(err)
				}
			} else if net.ParseIP(host) != nil {
				log.Fatal("TURN over TLS requires -domain to be a DNS hostname")
			}
			listener, err := net.Listen("tcp", *listen)
			if err != nil {
				log.Fatal(err)
			}
			_, port, err := net.SplitHostPort(listener.Addr().String())
			if err != nil {
				log.Fatal(err)
			}
			turnCfg.TLSAddr = net.JoinHostPort(host, port)
			https, secureRelay = server.SplitTLS(listener, tlsConfig.Clone(), 10*time.Second)
		}
		t, err := server.StartTURN(turnCfg, secureRelay)
		if err != nil {
			log.Fatal(err)
		}
		defer t.Close()
		log.Printf("TURN relay on port %d", *turnPort)
		if turnCfg.TLSAddr != "" {
			log.Printf("TURN over TLS at %s", turnCfg.TLSAddr)
		}
	}
	var stunURLs []string
	if *stun != "" {
		stunURLs = strings.Split(*stun, ",")
	}

	s := server.New(store, turnCfg, stunURLs, web.Files())
	if ca != nil {
		s.EnableLocalMode(ca)
	}
	s.ServeClients(clientDirectory(*dataPath, *clients), *fetch)
	if *remoteMCP {
		origin := *mcpOrigin
		if origin == "" && tlsConfig != nil {
			host := *domain
			if host == "" {
				host, _ = turnTLSHost(tlsConfig.Certificates[0], *turnHost)
			}
			_, port, _ := net.SplitHostPort(*listen)
			if host != "" && port != "" && port != "0" {
				if port != "443" && port != "https" {
					host = net.JoinHostPort(host, port)
				}
				origin = "https://" + host
			}
		}
		if err := s.EnableRemoteMCP(origin); err != nil {
			log.Fatal(err)
		}
	}
	srv := &http.Server{
		Addr:              *listen,
		TLSConfig:         tlsConfig,
		Handler:           s.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
	}
	// A stop (systemd, Ctrl+C) withdraws the LAN announcement, closes both listeners and
	// every WebSocket, and saves state before exiting.
	signals, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	serveErr := make(chan error, 2)
	if guideServer != nil {
		log.Printf("guide listening on %s", guide.Addr())
		go func() { serveErr <- guideServer.Serve(guide) }()
	}
	actualAddr := *listen
	if ca != nil {
		actualAddr = https.Addr().String()
	}
	log.Printf("listening on %s", actualAddr)
	go func() {
		switch {
		case ca != nil:
			serveErr <- srv.ServeTLS(https, "", "")
		case https != nil:
			serveErr <- srv.Serve(https)
		case tlsConfig != nil:
			serveErr <- srv.ListenAndServeTLS("", "")
		default:
			serveErr <- srv.ListenAndServe()
		}
	}()
	var mdns *server.MDNS
	if ca != nil {
		fingerprint := ca.Fingerprint()
		log.Printf("local CA SHA-256 fingerprint %s", fingerprint)
		if !store.HasAdmin() {
			host, port, _ := net.SplitHostPort(actualAddr)
			if net.ParseIP(host).IsUnspecified() {
				host = "localhost"
			}
			origin := "https://" + net.JoinHostPort(host, port)
			log.Printf("no admin yet, run deskway-server admin-link -data %q -url %s", *dataPath, origin)
		}
		var mdnsErr error
		mdns, mdnsErr = server.StartMDNS(hostname, *localName, fingerprint, https.Addr().(*net.TCPAddr), guide.Addr().(*net.TCPAddr))
		if mdnsErr != nil {
			log.Printf("mDNS for %s.local: %v", hostname, mdnsErr)
		} else {
			log.Printf("mDNS advertising %s.local", hostname)
		}
	}
	var serveError error
	select {
	case <-signals.Done():
	case serveError = <-serveErr:
	}
	if mdns != nil {
		mdns.Close()
	}
	s.StopMCP()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	var shutdown sync.WaitGroup
	if guideServer != nil {
		shutdown.Go(func() { _ = guideServer.Shutdown(ctx) })
	}
	_ = srv.Shutdown(ctx)
	shutdown.Wait()
	s.Shutdown()
	if serveError != nil && !errors.Is(serveError, http.ErrServerClosed) {
		log.Fatal(serveError)
	}
	log.Printf("stopped")
}

func turnTLSHost(pair tls.Certificate, host string) (string, error) {
	cert := pair.Leaf
	if cert == nil {
		var err error
		cert, err = x509.ParseCertificate(pair.Certificate[0])
		if err != nil {
			return "", err
		}
	}
	if host == "" && len(cert.DNSNames) == 1 && !strings.Contains(cert.DNSNames[0], "*") {
		host = cert.DNSNames[0]
	}
	if host == "" {
		return "", errors.New("set -public-host to the public hostname covered by the TLS certificate")
	}
	if net.ParseIP(host) != nil || strings.Contains(host, "*") || cert.VerifyHostname(host) != nil {
		return "", fmt.Errorf("TURN hostname %q must be a DNS name covered by the TLS certificate", host)
	}
	return host, nil
}

func upgradeCheckCommand(args []string) int {
	flags := flag.NewFlagSet("upgrade-check", flag.ExitOnError)
	dataPath := flags.String("data", "deskway-data/state.json", "state file")
	flags.Parse(args)
	if flags.NArg() != 0 {
		flags.Usage()
		return 2
	}
	ready, err := server.UpgradeCheck(*dataPath, os.Stdout)
	if err != nil {
		log.Print(err)
		return 2
	}
	if !ready {
		return 1
	}
	return 0
}

func clientOptions(flags *flag.FlagSet) (*string, *string, *bool) {
	dataPath := flags.String("data", "deskway-data/state.json", "state file")
	clients := flags.String("clients", "", "folder of the AI client programs to hand out (default: clients beside the state file)")
	fetch := flags.Bool("fetch-clients", true, "fetch missing AI client programs from this server's GitHub release at startup")
	return dataPath, clients, fetch
}

func clientDirectory(dataPath, clients string) string {
	if clients == "" {
		return filepath.Join(filepath.Dir(dataPath), "clients")
	}
	return clients
}

func remoteMCPCheckCommand(args []string) int {
	flags := flag.NewFlagSet("remote-mcp-check", flag.ExitOnError)
	dataPath, clients, _ := clientOptions(flags)
	flags.Parse(args)
	if flags.NArg() != 0 {
		flags.Usage()
		return 2
	}
	if err := server.CheckRemoteMCP(*dataPath, clientDirectory(*dataPath, *clients)); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	fmt.Println("Remote MCP preflight passed.")
	return 0
}

// adminLinkCommand takes only the account links lock, so it can run while the server
// holds the state lock. Use the server's origin, including any non-default port.
func adminLinkCommand(args []string, out io.Writer) int {
	flags := flag.NewFlagSet("admin-link", flag.ExitOnError)
	dataPath := flags.String("data", "deskway-data/state.json", "state file")
	origin := flags.String("url", "", "Deskway HTTPS origin (HTTP is allowed on loopback)")
	ifNeeded := flags.Bool("if-needed", false, "issue a link only when the admin needs a password")
	login := flags.Bool("login", false, "issue a two-minute sign-in link for the existing admin")
	flags.Parse(args)
	u, err := url.Parse(*origin)
	if flags.NArg() != 0 || *ifNeeded && *login || err != nil || u.Hostname() == "" || u.User != nil ||
		(u.Path != "" && u.Path != "/") || u.ForceQuery || u.RawQuery != "" || strings.Contains(*origin, "#") {
		flags.Usage()
		return 2
	}
	ip := net.ParseIP(u.Hostname())
	if u.Scheme != "https" && !(u.Scheme == "http" && (u.Hostname() == "localhost" || ip.IsLoopback())) {
		flags.Usage()
		return 2
	}
	issue := server.IssueAdminLink
	if *ifNeeded {
		issue = server.IssueAdminLinkIfNeeded
	} else if *login {
		issue = server.IssueAdminLoginLink
	}
	link, err := issue(*dataPath)
	if err != nil {
		log.Print(err)
		return 1
	}
	if link != "" {
		fmt.Fprintln(out, u.Scheme+"://"+u.Host+link)
	}
	return 0
}
