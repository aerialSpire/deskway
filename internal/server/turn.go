package server

import (
	"fmt"
	"log"
	"net"
	"strconv"
	"time"

	"github.com/pion/logging"
	"github.com/pion/turn/v5"

	"deskway/internal/proto"
)

const (
	turnCredTTL      = 12 * time.Hour
	iceRenew         = turnCredTTL / 2 // a browser's credentials are always 6 h from expiring
	turnSocketBuffer = 4 << 20
)

// TURNConfig configures the embedded relay.
type TURNConfig struct {
	PublicIP string
	Port     int
	Secret   string
	TLSAddr  string // certificate hostname and HTTPS port, when shared with TURN
	// Relay ports stay in this range, so a firewall only has to open this much.
	MinPort, MaxPort uint16
	// AllowPrivatePeers is for tests on one machine. A public server must leave it off:
	// it would let anyone holding TURN credentials reach the server's own network.
	AllowPrivatePeers bool
	// AccountMbps is each account's share of the relay (relaylimit.go).
	AccountMbps int
}

// StartTURN serves UDP, TCP, and the TLS listener when one is provided.
func StartTURN(cfg TURNConfig, secure net.Listener) (*turn.Server, error) {
	ip := net.ParseIP(cfg.PublicIP)
	if ip == nil {
		return nil, fmt.Errorf("invalid public IP %q", cfg.PublicIP)
	}
	addr := fmt.Sprintf("0.0.0.0:%d", cfg.Port)
	udp, err := net.ListenPacket("udp4", addr)
	if err != nil {
		return nil, fmt.Errorf("TURN UDP listen: %w", err)
	}
	// Video arrives in bursts; the default buffer drops packets before the relay can read them.
	if c, ok := udp.(*net.UDPConn); ok {
		_ = c.SetReadBuffer(turnSocketBuffer)
		_ = c.SetWriteBuffer(turnSocketBuffer)
	}
	tcp, err := net.Listen("tcp4", addr)
	if err != nil {
		udp.Close()
		return nil, fmt.Errorf("TURN TCP listen: %w", err)
	}
	limit := newRelayLimiter(cfg.AccountMbps, time.Now)
	gen := func() turn.RelayAddressGenerator {
		return limit.generator(&turn.RelayAddressGeneratorPortRange{RelayAddress: ip, Address: "0.0.0.0", MinPort: cfg.MinPort, MaxPort: cfg.MaxPort})
	}
	allowed := publicPeersOnly
	if cfg.AllowPrivatePeers {
		log.Printf("server: TURN may reach private addresses; never use this on a public server")
		allowed = func(net.Addr, net.IP) bool { return true }
	}
	logger := logging.NewDefaultLoggerFactory().NewLogger("turn")
	listenerConfig := func(l net.Listener) turn.ListenerConfig {
		return turn.ListenerConfig{Listener: l, RelayAddressGenerator: gen(), PermissionHandler: allowed}
	}
	listeners := []turn.ListenerConfig{listenerConfig(tcp)}
	if secure != nil {
		listeners = append(listeners, listenerConfig(secure))
	}
	relay, err := turn.NewServer(turn.ServerConfig{
		Realm:       "deskway",
		AuthHandler: turn.LongTermTURNRESTAuthHandler(cfg.Secret, logger),
		PacketConnConfigs: []turn.PacketConnConfig{{
			PacketConn:            udp,
			RelayAddressGenerator: gen(),
			PermissionHandler:     allowed,
		}},
		ListenerConfigs: listeners,
	})
	if err != nil {
		tcp.Close()
		udp.Close()
	}
	return relay, err
}

// publicPeersOnly stops anyone holding TURN credentials from using the relay to
// reach the server's own loopback or private network.
func publicPeersOnly(_ net.Addr, ip net.IP) bool {
	return !(ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified())
}

// ICEProvider returns, for an account (by its id), ICE servers with short-lived TURN credentials
// that name it, so the relay counts its traffic against it (relaylimit.go). A browser gets its
// account's; a host, those of the account of the session it is offered.
func (s *Server) ICEProvider(account string) []proto.ICEServer {
	nodes, _, _ := s.offeredRelays()
	return s.iceServers(account, nodes)
}

func (s *Server) iceServers(account string, nodes []TURNConfig) []proto.ICEServer {
	out := []proto.ICEServer{}
	if len(s.stun) > 0 {
		out = append(out, proto.ICEServer{URLs: s.stun})
	}
	if s.turn.PublicIP != "" {
		out = appendTURN(out, s.turn, account)
	}
	for _, node := range nodes {
		out = appendTURN(out, node, account)
	}
	return out
}

func appendTURN(out []proto.ICEServer, cfg TURNConfig, account string) []proto.ICEServer {
	user, pass, err := turn.GenerateLongTermTURNRESTCredentials(cfg.Secret, account, turnCredTTL)
	if err != nil {
		log.Printf("server: TURN credentials: %v", err)
		return out
	}
	base := net.JoinHostPort(cfg.PublicIP, strconv.Itoa(cfg.Port))
	urls := []string{"stun:" + base, "turn:" + base + "?transport=udp", "turn:" + base + "?transport=tcp"}
	if cfg.TLSAddr != "" {
		urls = append(urls, "turns:"+cfg.TLSAddr+"?transport=tcp")
	}
	return append(out, proto.ICEServer{URLs: urls, Username: user, Credential: pass})
}
