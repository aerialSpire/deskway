// Command deskway-server runs the Deskway rendezvous server.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"golang.org/x/crypto/acme/autocert"

	"deskway/internal/server"
	"deskway/web"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "admin" {
		adminCommand(os.Args[2:])
		return
	}
	var (
		listen    = flag.String("listen", "127.0.0.1:8080", "HTTP listen address")
		dataPath  = flag.String("data", "deskway-data/state.json", "state file")
		clients   = flag.String("clients", "", "folder of the AI client programs to hand out (default: clients beside the state file)")
		fetch     = flag.Bool("fetch-clients", true, "fetch missing AI client programs from this server's GitHub release")
		domain    = flag.String("domain", "", "domain name; gets a Let's Encrypt certificate automatically (listen on :443)")
		tlsCert   = flag.String("tls-cert", "", "TLS certificate file")
		tlsKey    = flag.String("tls-key", "", "TLS key file")
		publicIP  = flag.String("public-ip", "", "public IP; enables the TURN relay")
		turnPort  = flag.Int("turn-port", 3478, "TURN port (UDP and TCP)")
		turnMin   = flag.Int("turn-min-port", 49160, "lowest TURN relay port")
		turnMax   = flag.Int("turn-max-port", 49359, "highest TURN relay port")
		turnPriv  = flag.Bool("turn-allow-private-for-tests", false, "let TURN reach private addresses; for tests on one machine only")
		stun      = flag.String("stun", "", "comma-separated STUN URLs when TURN is off")
		relayMbps = flag.Int("relay-account-mbps", 30, "each account's share of the TURN relay, both directions together, in Mbit/s")
	)
	flag.Parse()

	// The server holds the state file for as long as it runs.
	store, err := server.OpenStore(*dataPath)
	if errors.Is(err, server.ErrLocked) {
		log.Fatalf("%s is in use: another deskway-server, or its admin command, is running", *dataPath)
	}
	if err != nil {
		log.Fatal(err)
	}
	defer store.Close()
	if store.UserCount() == 0 {
		// The admin's initial password is printed only by the admin command: a service's output ends up in logs.
		log.Printf("no accounts yet; stop the server and make the admin with: deskway-server admin NAME -data %s", *dataPath)
	}

	var turnCfg *server.TURNConfig
	if *publicIP != "" {
		turnCfg = &server.TURNConfig{PublicIP: *publicIP, Port: *turnPort, Secret: store.TURNSecret, MinPort: uint16(*turnMin), MaxPort: uint16(*turnMax), AllowPrivatePeers: *turnPriv, AccountMbps: *relayMbps}
		t, err := server.StartTURN(*turnCfg)
		if err != nil {
			log.Fatal(err)
		}
		defer t.Close()
		log.Printf("TURN relay on port %d", *turnPort)
	}
	var stunURLs []string
	if *stun != "" {
		stunURLs = strings.Split(*stun, ",")
	}

	s := server.New(store, server.ICEProvider(turnCfg, stunURLs), web.Files())
	if *clients == "" {
		*clients = filepath.Join(filepath.Dir(*dataPath), "clients")
	}
	s.ServeClients(*clients, *fetch)
	srv := &http.Server{
		Addr:              *listen,
		Handler:           s.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
	}
	// A stop (systemd, Ctrl+C) closes the listener, then every socket, and saves state
	// before exiting, instead of dying mid-write.
	stopped := make(chan struct{})
	go func() {
		sig := make(chan os.Signal, 1)
		signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
		<-sig
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_ = srv.Shutdown(ctx)
		close(stopped)
	}()
	log.Printf("listening on %s", *listen)
	switch {
	case *domain != "":
		// The certificate comes through port 443 itself, so port 80 can stay closed.
		m := &autocert.Manager{
			Prompt:     autocert.AcceptTOS,
			HostPolicy: autocert.HostWhitelist(*domain),
			Cache:      autocert.DirCache(filepath.Join(filepath.Dir(*dataPath), "certs")),
		}
		srv.TLSConfig = m.TLSConfig()
		err = srv.ListenAndServeTLS("", "")
	case *tlsCert != "":
		err = srv.ListenAndServeTLS(*tlsCert, *tlsKey)
	default:
		err = srv.ListenAndServe()
	}
	if err == http.ErrServerClosed {
		<-stopped
	}
	s.Shutdown()
	if err != http.ErrServerClosed {
		log.Fatal(err)
	}
	log.Printf("stopped")
}

// adminCommand makes the server's first account, the admin (docs/accounts.md):
//
//	admin NAME -data FILE
//
// It refuses once any account exists; the admin makes the others on the website. It prints the
// initial password once. The admin signs in with it and then chooses their own password and
// super password. It opens the store, which a running server holds, so it refuses to run until
// the server stops.
func adminCommand(args []string) {
	fs := flag.NewFlagSet("admin", flag.ExitOnError)
	dataPath := fs.String("data", "deskway-data/state.json", "state file")
	if len(args) < 1 || strings.HasPrefix(args[0], "-") {
		log.Fatal("usage: deskway-server admin NAME -data FILE")
	}
	name := args[0]
	if err := fs.Parse(args[1:]); err != nil {
		log.Fatal(err)
	}
	store, err := server.OpenStore(*dataPath)
	if errors.Is(err, server.ErrLocked) {
		log.Fatal("the server is running; stop it first")
	}
	if err != nil {
		log.Fatal(err)
	}
	defer store.Close()
	password, err := store.CreateAdmin(name)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("Admin: %s  Initial password: %s\nSign in with it once; the website then asks for a password and a super password of your own.\n", name, password)
}
