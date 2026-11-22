package server

import (
	"fmt"
	"log"
	"net"
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
	// Relay ports stay in this range, so a firewall only has to open this much.
	MinPort, MaxPort uint16
	// AllowPrivatePeers is for tests on one machine. A public server must leave it off:
	// it would let anyone holding TURN credentials reach the server's own network.
	AllowPrivatePeers bool
	// AccountMbps is each account's share of the relay (relaylimit.go).
	AccountMbps int
}

// StartTURN starts the relay on UDP and TCP. TCP matters on networks that block UDP.
func StartTURN(cfg TURNConfig) (*turn.Server, error) {
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
	return turn.NewServer(turn.ServerConfig{
		Realm:       "deskway",
		AuthHandler: turn.LongTermTURNRESTAuthHandler(cfg.Secret, logger),
		PacketConnConfigs: []turn.PacketConnConfig{{
			PacketConn:            udp,
			RelayAddressGenerator: gen(),
			PermissionHandler:     allowed,
		}},
		ListenerConfigs: []turn.ListenerConfig{{
			Listener:              tcp,
			RelayAddressGenerator: gen(),
			PermissionHandler:     allowed,
		}},
	})
}

// publicPeersOnly stops anyone holding TURN credentials from using the relay to
// reach the server's own loopback or private network.
func publicPeersOnly(_ net.Addr, ip net.IP) bool {
	return !(ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified())
}

// ICEProvider returns, for an account (by its id), ICE servers with short-lived TURN credentials
// that name it, so the relay counts its traffic against it (relaylimit.go). A browser gets its
// account's; a host, those of the account of the session it is offered.
// Without a relay it offers only the given STUN URLs.
func ICEProvider(cfg *TURNConfig, stun []string) func(account string) []proto.ICEServer {
	return func(account string) []proto.ICEServer {
		out := []proto.ICEServer{}
		if len(stun) > 0 {
			out = append(out, proto.ICEServer{URLs: stun})
		}
		if cfg == nil {
			return out
		}
		user, pass, err := turn.GenerateLongTermTURNRESTCredentials(cfg.Secret, account, turnCredTTL)
		if err != nil {
			log.Printf("server: TURN credentials: %v", err)
			return out
		}
		base := fmt.Sprintf("%s:%d", cfg.PublicIP, cfg.Port)
		return append(out, proto.ICEServer{
			URLs:       []string{"stun:" + base, "turn:" + base + "?transport=udp", "turn:" + base + "?transport=tcp"},
			Username:   user,
			Credential: pass,
		})
	}
}
