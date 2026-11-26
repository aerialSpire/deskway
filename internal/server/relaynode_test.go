package server

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/pion/turn/v5"

	"deskway/internal/proto"
)

func TestSessionRelaysThroughTheChosenNode(t *testing.T) {
	f, _ := relayFixture(t)
	f.s.turn.PublicIP, f.s.turn.Port = "203.0.113.10", 3478
	f.s.turn.Secret, f.s.turn.TLSAddr = f.s.store.TURNSecret, "desk.example:443"
	f.s.stun = []string{"stun:stun.example:3478"}
	mainAddr := relayAddress(f.s.turn.PublicIP, f.s.turn.Port)
	f.s.store.putDevice("pc1", &DeviceRecord{Owner: f.id("me"), TokenHash: hashToken("tok-pc1")})
	admin := f.signedIn("me")
	host := f.hostSocket(f.srv.URL, "pc1")
	host.send(proto.Hello{Type: proto.TypeHello, Version: proto.Version, Caps: []string{proto.CapRelayRTT}})
	initial := host.next(proto.TypeRelayList)
	if got := initial.Relays; got == nil || len(*got) != 1 || (*got)[mainRelayID] != mainAddr {
		t.Fatalf("initial relay list = %v", got)
	}

	tcp, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := tcp.Addr().(*net.TCPAddr).Port
	tcp.Close()
	cfg := TURNConfig{Port: port, MinPort: 49160, MaxPort: 49359, AllowPrivatePeers: true}
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

	list := host.next(proto.TypeRelayList)
	addr := relayAddress(cfg.PublicIP, cfg.Port)
	if list.Relays == nil || len(*list.Relays) != 2 || (*list.Relays)[mainRelayID] != mainAddr {
		t.Fatalf("updated relay list = %v", list.Relays)
	}
	path := filepath.Join(data, "relay.json")
	saved, err := loadRelayNode(ctx, nil, f.srv.URL, "", path, cfg)
	if err != nil || saved.Secret == "" || saved.AccountMbps != 17 {
		t.Fatalf("could not load the relay identity and limit without a token: %v", err)
	}
	if got := (*list.Relays)[saved.ID]; got != addr {
		t.Fatalf("node STUN address = %q, want %q", got, addr)
	}
	host.send(proto.Msg{Type: proto.TypeRelayRTTs, Probe: list.Probe, RelayRTTs: map[string]int{mainRelayID: 50, saved.ID: 5}})
	host.send(proto.Msg{Type: proto.TypeRelayRTTs, Probe: initial.Probe, RelayRTTs: map[string]int{mainRelayID: 1}})

	browser := f.browserSocket(f.srv.URL, admin)
	mainURLs := []string{"stun:" + mainAddr, "turn:" + mainAddr + "?transport=udp", "turn:" + mainAddr + "?transport=tcp", "turns:desk.example:443?transport=tcp"}
	if ice := browser.next(proto.TypeICE).ICE; len(ice) != 2 || !slices.Equal(ice[0].URLs, f.s.stun) || !slices.Equal(ice[1].URLs, mainURLs) {
		t.Fatal("browser ICE should contain only the main server's STUN and TURN entries")
	}
	servers := f.connect(t, browser, host, "relayed").ICE
	nodeURLs := []string{"stun:" + addr, "turn:" + addr + "?transport=udp", "turn:" + addr + "?transport=tcp"}
	if len(servers) != 2 || !slices.Equal(servers[0].URLs, f.s.stun) || !slices.Equal(servers[1].URLs, nodeURLs) {
		t.Fatal("host offer should contain the main STUN entry and only the chosen TURN relay")
	}
	ice := servers[1]
	if _, account, _ := strings.Cut(ice.Username, ":"); account != f.id("me") {
		t.Fatalf("TURN credentials name account %q, want %q", account, f.id("me"))
	}
	udp, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer udp.Close()
	client, err := turn.NewClient(&turn.ClientConfig{
		Conn: udp, STUNServerAddr: addr, TURNServerAddr: addr, Username: ice.Username, Password: ice.Credential, RTO: 20 * time.Millisecond,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	if err := client.Listen(); err != nil {
		t.Fatal(err)
	}
	if got, err := client.SendBindingRequest(); err != nil || got.String() != udp.LocalAddr().String() {
		t.Fatalf("node STUN binding address = %v, error = %v", got, err)
	}
	allocation, err := client.Allocate()
	if err != nil {
		t.Fatal(err)
	}
	defer allocation.Close()
	if got := allocation.LocalAddr().(*net.UDPAddr); !got.IP.IsLoopback() || got.Port < int(cfg.MinPort) || got.Port > int(cfg.MaxPort) {
		t.Fatalf("unexpected relayed address %s", got)
	}

	// Use the browser's direct UDP path. The host's first send installs the TURN permission,
	// as a connectivity check to the browser's UDP candidate would.
	direct, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer direct.Close()
	deadline := time.Now().Add(5 * time.Second)
	direct.SetDeadline(deadline)
	allocation.SetReadDeadline(deadline)
	for _, leg := range []struct {
		from, to net.PacketConn
		data     string
	}{
		{allocation, direct, "host to browser"},
		{direct, allocation, "browser to host"},
	} {
		if _, err := leg.from.WriteTo([]byte(leg.data), leg.to.LocalAddr()); err != nil {
			t.Fatalf("%s send: %v", leg.data, err)
		}
		var buf [32]byte
		n, from, err := leg.to.ReadFrom(buf[:])
		if err != nil || from.String() != leg.from.LocalAddr().String() || string(buf[:n]) != leg.data {
			t.Fatalf("%s received %q from %v, error = %v", leg.data, buf[:n], from, err)
		}
	}

	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("relay state permissions = %o", info.Mode().Perm())
	}
}
