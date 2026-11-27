package server

import (
	"context"
	"errors"
	"log"
	"net"
	"net/netip"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"golang.org/x/net/dns/dnsmessage"
	"golang.org/x/net/ipv4"
	"golang.org/x/net/ipv6"
)

const (
	maxDNSLabelLen = 63
	mdnsService    = "_deskway._tcp.local."
	mdnsPort       = 5353
	mdnsTTL        = 120
	mdnsClassBit   = 1 << 15 // QU in questions, cache flush in resource records
)

// LocalHostname is the DNS label used for the computer's .local name and service instance.
func LocalHostname() (string, error) {
	host, err := os.Hostname()
	if err != nil {
		return "", err
	}
	return hostnameLabel(host), nil
}

func hostnameLabel(host string) string {
	var label strings.Builder
	inInvalidRun := false
	for _, r := range host {
		r = lowerASCII(r)
		if dnsLabelChar(r) {
			label.WriteByte(byte(r))
			inInvalidRun = false
		} else if !inInvalidRun {
			label.WriteByte('-')
			inInvalidRun = true
		}
	}
	name := strings.Trim(label.String(), "-")
	if len(name) > maxDNSLabelLen {
		name = strings.TrimRight(name[:maxDNSLabelLen], "-")
	}
	if name == "" {
		return "deskway"
	}
	return name
}

func lowerASCII(r rune) rune {
	if r >= 'A' && r <= 'Z' {
		return r + ('a' - 'A')
	}
	return r
}

func dnsLabelChar(r rune) bool {
	return r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-'
}

func dnsLabel(label string) bool {
	return len(label) > 0 && len(label) <= maxDNSLabelLen && label[0] != '-' && label[len(label)-1] != '-' &&
		!strings.ContainsFunc(label, func(r rune) bool { return !dnsLabelChar(r) })
}

type mdnsServiceRecords struct {
	https uint16
	txt   []string
	host  dnsmessage.Name
	name  dnsmessage.Name
	ptr   dnsmessage.Name
}

func newMDNSRecords(host, name, fingerprint string, https, guide uint16) mdnsServiceRecords {
	return mdnsServiceRecords{
		https: https,
		txt:   []string{"v=1", "fp=" + fingerprint, "name=" + name, "guide=" + strconv.Itoa(int(guide))},
		host:  dnsmessage.MustNewName(host + ".local."),
		name:  dnsmessage.MustNewName(host + "." + mdnsService),
		ptr:   dnsmessage.MustNewName(mdnsService),
	}
}

func (s mdnsServiceRecords) records(ips []netip.Addr, ttl uint32, flush bool) []dnsmessage.Resource {
	makeRecord := func(name dnsmessage.Name, kind dnsmessage.Type, body dnsmessage.ResourceBody) dnsmessage.Resource {
		class := dnsmessage.ClassINET
		if flush && kind != dnsmessage.TypePTR {
			class |= mdnsClassBit
		}
		return dnsmessage.Resource{Header: dnsmessage.ResourceHeader{Name: name, Type: kind, Class: class, TTL: ttl}, Body: body}
	}
	records := []dnsmessage.Resource{
		makeRecord(s.ptr, dnsmessage.TypePTR, &dnsmessage.PTRResource{PTR: s.name}),
		makeRecord(s.name, dnsmessage.TypeSRV, &dnsmessage.SRVResource{Port: s.https, Target: s.host}),
		makeRecord(s.name, dnsmessage.TypeTXT, &dnsmessage.TXTResource{TXT: s.txt}),
	}
	for _, ip := range ips {
		if ip.Is4() {
			records = append(records, makeRecord(s.host, dnsmessage.TypeA, &dnsmessage.AResource{A: ip.As4()}))
		} else {
			records = append(records, makeRecord(s.host, dnsmessage.TypeAAAA, &dnsmessage.AAAAResource{AAAA: ip.As16()}))
		}
	}
	return records
}

func (s mdnsServiceRecords) announcement(ips []netip.Addr, ttl uint32) ([]byte, error) {
	msg := dnsmessage.Message{Header: dnsmessage.Header{Response: true, Authoritative: true}, Answers: s.records(ips, ttl, ttl != 0)}
	return msg.Pack()
}

func (s mdnsServiceRecords) answer(packet []byte, addresses func() []netip.Addr, legacy bool) ([]byte, bool, error) {
	var query dnsmessage.Message
	if err := query.Unpack(packet); err != nil || query.Response || query.OpCode != 0 {
		return nil, false, nil
	}
	if !slices.ContainsFunc(query.Questions, func(q dnsmessage.Question) bool {
		name := q.Name.String()
		return strings.EqualFold(name, s.ptr.String()) || strings.EqualFold(name, s.name.String()) || strings.EqualFold(name, s.host.String())
	}) {
		return nil, false, nil
	}
	ips := addresses()
	if len(ips) == 0 {
		return nil, false, nil
	}
	reply := dnsmessage.Message{Header: dnsmessage.Header{Response: true, Authoritative: true}}
	ttl := uint32(mdnsTTL)
	if legacy {
		reply.ID, reply.Questions = query.ID, query.Questions
		ttl = 10
	}
	unicast := true
	withRelated := false
	var related []dnsmessage.Resource
	for _, record := range s.records(ips, ttl, !legacy) {
		kind := record.Header.Type
		// RFC 6762 §7.1. The service PTR is a shared record.
		if kind == dnsmessage.TypePTR && !legacy && slices.ContainsFunc(query.Answers, func(known dnsmessage.Resource) bool {
			ptr, ok := known.Body.(*dnsmessage.PTRResource)
			return ok && known.Header.Class == dnsmessage.ClassINET && known.Header.TTL >= mdnsTTL/2 &&
				strings.EqualFold(known.Header.Name.String(), s.ptr.String()) && strings.EqualFold(ptr.PTR.String(), s.name.String())
		}) {
			continue
		}
		matched := false
		for _, q := range query.Questions {
			class := q.Class &^ mdnsClassBit
			if (class == dnsmessage.ClassINET || class == dnsmessage.ClassANY) &&
				(q.Type == kind || q.Type == dnsmessage.TypeALL) &&
				strings.EqualFold(q.Name.String(), record.Header.Name.String()) {
				matched = true
				unicast = unicast && q.Class&mdnsClassBit != 0
			}
		}
		if matched {
			reply.Answers = append(reply.Answers, record)
			withRelated = withRelated || kind == dnsmessage.TypePTR || kind == dnsmessage.TypeSRV
		} else if kind != dnsmessage.TypePTR {
			related = append(related, record)
		}
	}
	if len(reply.Answers) == 0 {
		return nil, false, nil
	}
	if withRelated {
		reply.Additionals = related
	}
	data, err := reply.Pack()
	return data, legacy || unicast, err
}

type mdnsSocket struct {
	iface *net.Interface
	group *net.UDPAddr
	conn  *net.UDPConn
	read  func([]byte) (int, int, net.Addr, error)
}

type multicastConn interface {
	SetMulticastInterface(*net.Interface) error
	JoinGroup(*net.Interface, net.Addr) error
}

func joinMDNS(conn multicastConn, iface *net.Interface, group *net.UDPAddr) error {
	if err := conn.SetMulticastInterface(iface); err != nil {
		return err
	}
	return conn.JoinGroup(iface, group)
}

func newMDNSSocket(iface *net.Interface, group *net.UDPAddr) (*mdnsSocket, error) {
	network := "udp4"
	if group.IP.To4() == nil {
		network = "udp6"
		group.Zone = iface.Name
	}
	// Bind the wildcard address before joining the group, and share the mDNS port with
	// the OS and our other interfaces.
	config := net.ListenConfig{Control: func(_, _ string, c syscall.RawConn) error {
		var sockErr error
		if err := c.Control(func(fd uintptr) { sockErr = reuseMDNSPort(fd) }); err != nil {
			return err
		}
		return sockErr
	}}
	packet, err := config.ListenPacket(context.Background(), network, ":"+strconv.Itoa(mdnsPort))
	if err != nil {
		return nil, err
	}
	conn := packet.(*net.UDPConn)
	sk := &mdnsSocket{iface: iface, group: group, conn: conn, read: func(buf []byte) (int, int, net.Addr, error) {
		n, addr, err := conn.ReadFromUDP(buf)
		return n, 0, addr, err
	}}
	// Where supported, use the incoming interface to keep multicast sockets on their own link.
	if network == "udp4" {
		p := ipv4.NewPacketConn(conn)
		if err = joinMDNS(p, iface, group); err == nil {
			err = p.SetMulticastTTL(255)
		}
		if p.SetControlMessage(ipv4.FlagInterface, true) == nil {
			sk.read = func(buf []byte) (int, int, net.Addr, error) {
				n, cm, addr, err := p.ReadFrom(buf)
				if cm != nil {
					return n, cm.IfIndex, addr, err
				}
				return n, 0, addr, err
			}
		}
	} else {
		p := ipv6.NewPacketConn(conn)
		if err = joinMDNS(p, iface, group); err == nil {
			err = p.SetMulticastHopLimit(255)
		}
		if p.SetControlMessage(ipv6.FlagInterface, true) == nil {
			sk.read = func(buf []byte) (int, int, net.Addr, error) {
				n, cm, addr, err := p.ReadFrom(buf)
				if cm != nil {
					return n, cm.IfIndex, addr, err
				}
				return n, 0, addr, err
			}
		}
	}
	if err != nil {
		conn.Close()
		return nil, err
	}
	return sk, nil
}

func interfaceNetworks(iface *net.Interface) []netip.Prefix {
	addrs, _ := iface.Addrs()
	var networks []netip.Prefix
	for _, addr := range addrs {
		if prefix, err := netip.ParsePrefix(addr.String()); err == nil {
			networks = append(networks, prefix)
		}
	}
	return networks
}

func onLink(src netip.Addr, networks []netip.Prefix) bool {
	src = src.Unmap().WithZone("")
	return !src.IsUnspecified() && !src.IsMulticast() &&
		slices.ContainsFunc(networks, func(network netip.Prefix) bool { return network.Contains(src) })
}

func (sk *mdnsSocket) send(packet []byte, addr *net.UDPAddr, err error) {
	if err == nil {
		_ = sk.conn.SetWriteDeadline(time.Now().Add(time.Second))
		_, err = sk.conn.WriteToUDP(packet, addr)
	}
	if err != nil {
		log.Printf("mDNS on %s to %s: %v", sk.iface.Name, addr, err)
	}
}

// MDNS advertises a local-mode server on the machine's multicast interfaces.
type MDNS struct {
	records mdnsServiceRecords
	httpsIP netip.Addr
	guideIP netip.Addr
	sockets []*mdnsSocket
	readers sync.WaitGroup

	replyMu sync.RWMutex
	closing bool
}

func (m *MDNS) advertisedIPs(iface *net.Interface, networks []netip.Prefix) []netip.Addr {
	matches := func(bind, ip netip.Addr) bool {
		zone := bind.Zone()
		return bind.IsUnspecified() || bind.WithZone("") == ip &&
			(zone == "" || zone == iface.Name || zone == strconv.Itoa(iface.Index))
	}
	var ips []netip.Addr
	for _, network := range networks {
		ip := network.Addr()
		if localIP(ip) && matches(m.httpsIP, ip) && matches(m.guideIP, ip) {
			ips = append(ips, ip)
		}
	}
	return ips
}

// StartMDNS announces the service and starts answering queries. A failed interface is reported
// independently, so an unavailable multicast family does not stop the HTTPS service.
func StartMDNS(host, name, fingerprint string, https, guide *net.TCPAddr) (*MDNS, error) {
	interfaces, err := net.Interfaces()
	if err != nil {
		return nil, err
	}
	m := &MDNS{
		records: newMDNSRecords(host, name, fingerprint, uint16(https.Port), uint16(guide.Port)),
		httpsIP: tcpIP(https),
		guideIP: tcpIP(guide),
	}
	for i := range interfaces {
		iface := &interfaces[i]
		if iface.Flags&(net.FlagUp|net.FlagMulticast|net.FlagLoopback) != net.FlagUp|net.FlagMulticast {
			continue
		}
		networks := interfaceNetworks(iface)
		for _, group := range []*net.UDPAddr{
			{IP: net.IPv4(224, 0, 0, 251), Port: mdnsPort},
			{IP: netip.MustParseAddr("ff02::fb").AsSlice(), Port: mdnsPort},
		} {
			v4 := group.IP.To4() != nil
			if !slices.ContainsFunc(networks, func(network netip.Prefix) bool { return network.Addr().Is4() == v4 }) {
				continue
			}
			sk, err := newMDNSSocket(iface, group)
			if err != nil {
				log.Printf("mDNS on %s to %s: %v", iface.Name, group, err)
				continue
			}
			m.sockets = append(m.sockets, sk)
			if ips := m.advertisedIPs(iface, networks); len(ips) > 0 {
				packet, err := m.records.announcement(ips, mdnsTTL)
				sk.send(packet, group, err)
			}
			m.readers.Go(func() { m.serve(sk) })
		}
	}
	if len(m.sockets) == 0 {
		return nil, errors.New("no multicast sockets available for mDNS")
	}
	return m, nil
}

func (m *MDNS) serve(sk *mdnsSocket) {
	var buf [9000]byte
	for {
		n, iface, from, err := sk.read(buf[:])
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				return
			}
			continue
		}
		if iface != 0 && iface != sk.iface.Index {
			continue
		}
		src := from.(*net.UDPAddr)
		packet, unicast, err := m.records.answer(buf[:n], func() []netip.Addr {
			networks := interfaceNetworks(sk.iface)
			if !onLink(src.AddrPort().Addr(), networks) {
				return nil
			}
			return m.advertisedIPs(sk.iface, networks)
		}, src.Port != mdnsPort)
		if packet != nil || err != nil {
			dest := sk.group
			if unicast {
				dest = src
			}
			m.replyMu.RLock()
			if !m.closing {
				sk.send(packet, dest, err)
			}
			m.replyMu.RUnlock()
		}
	}
}

// Close stops answering, sends the TTL-zero goodbye and closes the sockets.
func (m *MDNS) Close() {
	m.replyMu.Lock()
	m.closing = true
	m.replyMu.Unlock()
	for _, sk := range m.sockets {
		ips := m.advertisedIPs(sk.iface, interfaceNetworks(sk.iface))
		packet, err := m.records.announcement(ips, 0)
		sk.send(packet, sk.group, err)
		sk.conn.Close()
	}
	m.readers.Wait()
}
