package server

import (
	"net"
	"testing"
	"time"

	"github.com/pion/turn/v5"
)

// The relay's share per account: the bucket, and which account an allocation's traffic counts
// against.

func TestTheRelayBucketDropsBeyondTheRateAndRefills(t *testing.T) {
	now := time.Unix(1000, 0)
	l := newRelayLimiter(8, func() time.Time { return now }) // 1 MB/s: a 250 KB burst
	if !l.allow("a", 250_000) {
		t.Fatal("the burst is refused")
	}
	if l.allow("a", 1) {
		t.Fatal("beyond the burst passes")
	}
	if !l.allow("b", 250_000) {
		t.Fatal("another account pays for the first")
	}
	now = now.Add(100 * time.Millisecond)
	if !l.allow("a", 100_000) || l.allow("a", 1) {
		t.Fatal("a tenth of a second refills a tenth of the rate, no more")
	}
	now = now.Add(time.Hour)
	if l.allow("a", 250_001) {
		t.Fatal("an idle account saves up more than the burst")
	}
}

// The credentials a browser or a host is given name the account; the relay's auth handler
// gives that account back as the allocation's user, which the relay socket is limited by.
func TestRelayCredentialsCarryTheAccount(t *testing.T) {
	cfg := &TURNConfig{PublicIP: "203.0.113.1", Port: 3478, Secret: "s"}
	ice := ICEProvider(cfg, nil)("acct1")
	if len(ice) != 1 {
		t.Fatalf("%+v", ice)
	}
	auth := turn.LongTermTURNRESTAuthHandler(cfg.Secret, nil)
	user, key, ok := auth(&turn.RequestAttributes{Username: ice[0].Username, Realm: "deskway"})
	if !ok || user != "acct1" || string(key) != string(turn.GenerateAuthKey(ice[0].Username, "deskway", ice[0].Credential)) {
		t.Fatalf("auth: %q %v", user, ok)
	}
}

// loopbackGenerator hands out relay sockets on the loopback, as the port range would.
type loopbackGenerator struct{ turn.RelayAddressGenerator }

func (loopbackGenerator) AllocatePacketConn(turn.AllocateListenerConfig) (net.PacketConn, net.Addr, error) {
	c, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		return nil, nil, err
	}
	return c, c.LocalAddr(), nil
}

func TestAnAllocationsTrafficCountsAgainstItsAccount(t *testing.T) {
	now := time.Unix(1000, 0)
	l := newRelayLimiter(8, func() time.Time { return now })
	gen := l.generator(loopbackGenerator{})
	relay, _, err := gen.AllocatePacketConn(turn.AllocateListenerConfig{Network: "udp4", UserID: "acct1"})
	if err != nil {
		t.Fatal(err)
	}
	defer relay.Close()
	peer, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer peer.Close()
	// Room for the whole burst before it is read.
	_ = peer.(*net.UDPConn).SetReadBuffer(turnSocketBuffer)

	// Towards the peer, fill the 250 KB burst with datagrams that fit macOS's UDP limit.
	// The next packet is dropped, as if sent.
	const burstPackets = 50
	packet := make([]byte, 5_000)
	for i := range burstPackets + 1 {
		if n, err := relay.WriteTo(packet, peer.LocalAddr()); err != nil || n != len(packet) {
			t.Fatalf("write %d: %d %v", i, n, err)
		}
	}
	got := 0
	buf := make([]byte, len(packet))
	_ = peer.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	for {
		if _, _, err := peer.ReadFrom(buf); err != nil {
			break
		}
		got++
	}
	if got != burstPackets {
		t.Fatalf("the peer got %d packets, want %d", got, burstPackets)
	}
	if l.allow("acct1", 1) {
		t.Fatal("the account's bucket was not the one used")
	}

	// From the peer: 1 ms refills 1 KB, too little for this packet, which is dropped. The
	// small one after it passes.
	now = now.Add(time.Millisecond)
	if _, err := peer.WriteTo(packet, relay.LocalAddr()); err != nil {
		t.Fatal(err)
	}
	if _, err := peer.WriteTo([]byte("passes"), relay.LocalAddr()); err != nil {
		t.Fatal(err)
	}
	_ = relay.SetReadDeadline(time.Now().Add(time.Second))
	n, _, err := relay.ReadFrom(buf)
	if err != nil || string(buf[:n]) != "passes" {
		t.Fatalf("read %q %v", buf[:n], err)
	}

	if _, _, err := gen.AllocateListener(turn.AllocateListenerConfig{Network: "tcp4", UserID: "acct1"}); err == nil {
		t.Fatal("a TCP relay was allocated: it would escape the limit")
	}
}
