package server

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/pion/turn/v5"

	"deskway/internal/proto"
)

func TestRelayNodeJoinsAndAcceptsATURNAllocation(t *testing.T) {
	f, _ := relayFixture(t)
	admin := f.signedIn("me")
	browser := f.browserSocket(f.srv.URL, admin)
	browser.next(proto.TypeICE)

	tcp, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := tcp.Addr().(*net.TCPAddr).Port
	tcp.Close()
	cfg := TURNConfig{Port: port, MinPort: 49160, MaxPort: 49359}
	token := relayToken(f, admin)
	data := t.TempDir()
	if err := runRelayNode(context.Background(), f.srv.Client().Transport, f.srv.URL, token, data, cfg); err == nil || !strings.Contains(err.Error(), "pass -public-ip") {
		t.Fatalf("join with an observed loopback address: %v", err)
	}
	cfg.PublicIP = "127.0.0.1"
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- runRelayNode(ctx, f.srv.Client().Transport, f.srv.URL, token, data, cfg) }()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-done:
			if err != nil {
				t.Error(err)
			}
		case <-time.After(5 * time.Second):
			t.Error("relay node did not stop")
		}
	})

	servers := browser.next(proto.TypeICE).ICE
	if len(servers) != 1 {
		t.Fatalf("browser got %d ICE entries, want 1", len(servers))
	}
	ice := servers[0]
	addr := strings.TrimPrefix(ice.URLs[0], "stun:")
	udp, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer udp.Close()
	client, err := turn.NewClient(&turn.ClientConfig{
		Conn: udp, TURNServerAddr: addr, Username: ice.Username, Password: ice.Credential, RTO: 20 * time.Millisecond,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	if err := client.Listen(); err != nil {
		t.Fatal(err)
	}
	allocation, err := client.Allocate()
	if err != nil {
		t.Fatal(err)
	}
	defer allocation.Close()
	if got := allocation.LocalAddr().(*net.UDPAddr); !got.IP.IsLoopback() || got.Port < int(cfg.MinPort) || got.Port > int(cfg.MaxPort) {
		t.Fatalf("unexpected relayed address %s", got)
	}

	path := filepath.Join(data, "relay.json")
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("relay state permissions = %o", info.Mode().Perm())
	}
	saved, err := loadRelayNode(ctx, nil, f.srv.URL, "", path, cfg)
	if err != nil || saved.Secret == "" || saved.AccountMbps != 17 {
		t.Fatalf("could not load the relay identity and limit without a token: %v", err)
	}
}
