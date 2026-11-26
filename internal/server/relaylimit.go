package server

import (
	"errors"
	"net"
	"sync"
	"time"

	"github.com/pion/turn/v5"
)

// The relay's share per account (docs/accounts.md, Limits): every account's relayed traffic,
// both directions of all its allocations together, goes through one token bucket at a fixed
// rate. What is beyond it is dropped, as a congested link would: the media's congestion control
// lowers its rate, and one account's sessions cannot crowd out another's.
//
// The account comes from the TURN credentials (iceServers names it as their user); pion's
// long-term auth handler returns it as the allocation's user id, and the relay socket made for
// that allocation is wrapped with the account's bucket. Allocations are only UDP (WebRTC asks
// for nothing else); TCP relays (RFC 6062) are refused, so none escapes the limit.

// relayBurst is how much an account may send at once over its rate: a quarter second's worth,
// enough for a keyframe.
const relayBurst = 250 * time.Millisecond

var errNoTCPRelay = errors.New("TCP relays are not offered")

// relayLimiter holds every account's bucket.
type relayLimiter struct {
	rate float64 // bytes a second
	now  func() time.Time

	mu      sync.Mutex
	buckets map[string]*relayBucket
}

type relayBucket struct {
	tokens float64
	at     time.Time
}

func newRelayLimiter(mbps int, now func() time.Time) *relayLimiter {
	return &relayLimiter{rate: float64(mbps) * 1e6 / 8, now: now, buckets: map[string]*relayBucket{}}
}

// allow takes n bytes from the account's bucket, or says they are over its rate.
func (l *relayLimiter) allow(account string, n int) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	burst := l.rate * relayBurst.Seconds()
	now := l.now()
	b := l.buckets[account]
	if b == nil {
		b = &relayBucket{tokens: burst, at: now}
		l.buckets[account] = b
	}
	b.tokens = min(burst, b.tokens+now.Sub(b.at).Seconds()*l.rate)
	b.at = now
	if b.tokens < float64(n) {
		return false
	}
	b.tokens -= float64(n)
	return true
}

// generator makes the relay sockets of allocations through gen, each policed by its account's
// bucket.
func (l *relayLimiter) generator(gen turn.RelayAddressGenerator) turn.RelayAddressGenerator {
	return &limitedGenerator{RelayAddressGenerator: gen, l: l}
}

type limitedGenerator struct {
	turn.RelayAddressGenerator
	l *relayLimiter
}

func (g *limitedGenerator) AllocatePacketConn(conf turn.AllocateListenerConfig) (net.PacketConn, net.Addr, error) {
	conn, addr, err := g.RelayAddressGenerator.AllocatePacketConn(conf)
	if err != nil {
		return nil, nil, err
	}
	return &limitedConn{PacketConn: conn, account: conf.UserID, l: g.l}, addr, nil
}

func (g *limitedGenerator) AllocateListener(turn.AllocateListenerConfig) (net.Listener, net.Addr, error) {
	return nil, nil, errNoTCPRelay
}

func (g *limitedGenerator) AllocateConn(turn.AllocateConnConfig) (net.Conn, error) {
	return nil, errNoTCPRelay
}

// limitedConn is one allocation's relay socket: what it sends to the peer (from the client)
// and what it receives from the peer (for the client) both count against the account.
type limitedConn struct {
	net.PacketConn
	account string
	l       *relayLimiter
}

// WriteTo drops a packet over the account's rate, as if sent.
func (c *limitedConn) WriteTo(p []byte, addr net.Addr) (int, error) {
	if !c.l.allow(c.account, len(p)) {
		return len(p), nil
	}
	return c.PacketConn.WriteTo(p, addr)
}

// ReadFrom passes on only the packets within the account's rate.
func (c *limitedConn) ReadFrom(p []byte) (int, net.Addr, error) {
	for {
		n, addr, err := c.PacketConn.ReadFrom(p)
		if err != nil || c.l.allow(c.account, n) {
			return n, addr, err
		}
	}
}
