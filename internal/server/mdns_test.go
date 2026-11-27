package server

import (
	"encoding/json"
	"net"
	"net/netip"
	"os"
	"reflect"
	"strings"
	"syscall"
	"testing"

	"golang.org/x/net/dns/dnsmessage"
)

func TestHostnameLabel(t *testing.T) {
	for _, tt := range []struct{ host, want string }{
		{"SEAN_ASUS", "sean-asus"},
		{"--_Office__客廳...PC-09_--", "office-pc-09"},
		{"A--B", "a--b"},
		{"", "deskway"},
		{"_K客!-", "deskway"},
		{strings.Repeat("-", 64) + strings.Repeat("A", 64), strings.Repeat("a", 63)},
		{strings.Repeat("A", 62) + "_B", strings.Repeat("a", 62)},
	} {
		if got := hostnameLabel(tt.host); got != tt.want || !dnsLabel(got) {
			t.Errorf("hostnameLabel(%q) = %q, want %q", tt.host, got, tt.want)
		}
	}
	if dnsLabel("sean_asus") {
		t.Error("dnsLabel accepted an underscore")
	}
}

func TestMDNSAnswers(t *testing.T) {
	var contract struct {
		Service string
		Records struct {
			SRV struct {
				Host string
				Port uint16
			}
			TXT map[string]string
		}
	}
	data, err := os.ReadFile("../../proto/local-discovery.json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, &contract); err != nil {
		t.Fatal(err)
	}
	srv, txt := contract.Records.SRV, contract.Records.TXT
	records := newMDNSRecords(hostnameLabel("SEAN_ASUS"), txt["name"], txt["fp"], srv.Port, 18498)
	if got := records.ptr.String(); got != contract.Service {
		t.Fatalf("service type = %q, want %q", got, contract.Service)
	}
	v4 := netip.MustParseAddr("192.168.1.2")
	v6 := netip.MustParseAddr("fd00::2")
	ips := func() []netip.Addr { return []netip.Addr{v4, v6} }
	host := srv.Host
	instance := strings.TrimSuffix(host, ".local.") + "." + contract.Service
	for _, tt := range []struct {
		name       string
		kind       dnsmessage.Type
		class      dnsmessage.Class
		legacy     bool
		answer     dnsmessage.ResourceBody
		additional int
	}{
		{"_Deskway._TCP.local.", dnsmessage.TypePTR, dnsmessage.ClassINET, false, &dnsmessage.PTRResource{PTR: dnsmessage.MustNewName(instance)}, 4},
		{instance, dnsmessage.TypeSRV, dnsmessage.ClassINET, false, &dnsmessage.SRVResource{Port: srv.Port, Target: dnsmessage.MustNewName(host)}, 3},
		{instance, dnsmessage.TypeTXT, dnsmessage.ClassINET, false, &dnsmessage.TXTResource{TXT: []string{"v=" + txt["v"], "fp=" + txt["fp"], "name=" + txt["name"], "guide=" + txt["guide"]}}, 0},
		{host, dnsmessage.TypeA, dnsmessage.ClassINET, false, &dnsmessage.AResource{A: v4.As4()}, 0},
		{host, dnsmessage.TypeAAAA, dnsmessage.ClassINET | mdnsClassBit, false, &dnsmessage.AAAAResource{AAAA: v6.As16()}, 0},
		{host, dnsmessage.TypeA, dnsmessage.ClassINET, true, &dnsmessage.AResource{A: v4.As4()}, 0},
		{host, dnsmessage.TypeTXT, dnsmessage.ClassINET, false, nil, 0},
		{"sean_asus.local.", dnsmessage.TypeA, dnsmessage.ClassINET, false, nil, 0},
		{"other.local.", dnsmessage.TypeA, dnsmessage.ClassINET, false, nil, 0},
		{host, dnsmessage.TypeA, dnsmessage.ClassCHAOS, false, nil, 0},
	} {
		t.Run(tt.name+tt.kind.String()+tt.class.String(), func(t *testing.T) {
			query := dnsmessage.Message{Header: dnsmessage.Header{ID: 123},
				Questions: []dnsmessage.Question{{Name: dnsmessage.MustNewName(tt.name), Type: tt.kind, Class: tt.class}}}
			packet, err := query.Pack()
			if err != nil {
				t.Fatal(err)
			}
			data, unicast, err := records.answer(packet, ips, tt.legacy)
			if err != nil {
				t.Fatal(err)
			}
			if tt.answer == nil {
				if len(data) != 0 {
					t.Fatal("answered an unrelated question")
				}
				return
			}
			if unicast != (tt.legacy || tt.class&mdnsClassBit != 0) {
				t.Fatalf("unexpected unicast=%t", unicast)
			}
			var msg dnsmessage.Message
			if err := msg.Unpack(data); err != nil {
				t.Fatal(err)
			}
			if !msg.Response || !msg.Authoritative || len(msg.Answers) != 1 || len(msg.Additionals) != tt.additional {
				t.Fatalf("invalid response: %+v", msg)
			}
			answer := msg.Answers[0]
			if !reflect.DeepEqual(answer.Body, tt.answer) || answer.Header.Type != tt.kind || !strings.EqualFold(answer.Header.Name.String(), tt.name) {
				t.Fatalf("answer = %v, want %v", answer, tt.answer)
			}
			if tt.legacy {
				if msg.ID != query.ID || !reflect.DeepEqual(msg.Questions, query.Questions) || answer.Header.TTL != 10 ||
					answer.Header.Class != dnsmessage.ClassINET {
					t.Fatalf("invalid legacy unicast response: %+v", msg)
				}
			} else if msg.ID != 0 || len(msg.Questions) != 0 || answer.Header.TTL != mdnsTTL ||
				(answer.Header.Class&mdnsClassBit != 0) != (tt.kind != dnsmessage.TypePTR) {
				t.Fatalf("invalid multicast response: %+v", msg)
			}
		})
	}
}

func TestMDNSKnownPTR(t *testing.T) {
	records := newMDNSRecords("hub", "Hub", "", 18499, 18498)
	ips := func() []netip.Addr { return []netip.Addr{netip.MustParseAddr("192.168.1.2")} }
	for _, tt := range []struct {
		name, owner, target string
		class               dnsmessage.Class
		ttl                 uint32
		legacy, wantReply   bool
	}{
		{"fresh", "_DESKWAY._tcp.local.", "Hub._deskway._tcp.local.", dnsmessage.ClassINET, mdnsTTL / 2, false, false},
		{"stale", mdnsService, "hub._deskway._tcp.local.", dnsmessage.ClassINET, mdnsTTL/2 - 1, false, true},
		{"different instance", mdnsService, "other._deskway._tcp.local.", dnsmessage.ClassINET, mdnsTTL, false, true},
		{"different name", "_other._tcp.local.", "hub._deskway._tcp.local.", dnsmessage.ClassINET, mdnsTTL, false, true},
		{"different class", mdnsService, "hub._deskway._tcp.local.", dnsmessage.ClassCHAOS, mdnsTTL, false, true},
		{"legacy", mdnsService, "hub._deskway._tcp.local.", dnsmessage.ClassINET, mdnsTTL, true, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			query := dnsmessage.Message{
				Questions: []dnsmessage.Question{{Name: records.ptr, Type: dnsmessage.TypePTR, Class: dnsmessage.ClassINET}},
				Answers: []dnsmessage.Resource{{
					Header: dnsmessage.ResourceHeader{Name: dnsmessage.MustNewName(tt.owner), Type: dnsmessage.TypePTR, Class: tt.class, TTL: tt.ttl},
					Body:   &dnsmessage.PTRResource{PTR: dnsmessage.MustNewName(tt.target)},
				}},
			}
			packet, err := query.Pack()
			if err != nil {
				t.Fatal(err)
			}
			data, _, err := records.answer(packet, ips, tt.legacy)
			if err != nil || (len(data) > 0) != tt.wantReply {
				t.Fatalf("answer length = %d, err = %v, want reply = %t", len(data), err, tt.wantReply)
			}
		})
	}
}

func TestMDNSOnLink(t *testing.T) {
	networks := []netip.Prefix{netip.MustParsePrefix("192.168.2.42/24"), netip.MustParsePrefix("fe80::42/64")}
	for _, tt := range []struct {
		source string
		want   bool
	}{
		{"192.168.2.7", true},
		{"::ffff:192.168.2.7", true},
		{"192.168.3.7", false},
		{"10.0.0.1", false},
		{"fe80::1%en0", true},
		{"fe80:0:0:1::1%en0", false},
		{"fd00::1", false},
	} {
		if got := onLink(netip.MustParseAddr(tt.source), networks); got != tt.want {
			t.Errorf("onLink(%s) = %t, want %t", tt.source, got, tt.want)
		}
	}
}

func TestMDNSReadsAfterOversizedDatagram(t *testing.T) {
	m := &MDNS{}
	var reads int
	m.serve(&mdnsSocket{read: func([]byte) (int, int, net.Addr, error) {
		reads++
		if reads == 1 {
			return 0, 0, nil, os.NewSyscallError("recvfrom", syscall.EMSGSIZE)
		}
		return 0, 0, nil, &net.OpError{Op: "read", Err: net.ErrClosed}
	}})
	if reads != 2 {
		t.Fatalf("read %d times, want 2", reads)
	}
}

func TestMDNSAdvertisedIPs(t *testing.T) {
	m := &MDNS{httpsIP: netip.IPv6Unspecified(), guideIP: netip.IPv6Unspecified()}
	iface := &net.Interface{Index: 1, Name: "lan"}
	before := netip.MustParsePrefix("192.168.1.2/24")
	after := netip.MustParsePrefix("192.168.1.3/24")
	for _, network := range []netip.Prefix{before, after} {
		if ips := m.advertisedIPs(iface, []netip.Prefix{network}); !reflect.DeepEqual(ips, []netip.Addr{network.Addr()}) {
			t.Fatalf("advertised addresses = %v, want %v", ips, network.Addr())
		}
	}
	m.httpsIP = before.Addr()
	if ips := m.advertisedIPs(iface, []netip.Prefix{before, after}); !reflect.DeepEqual(ips, []netip.Addr{before.Addr()}) {
		t.Fatalf("advertised addresses = %v, want only the bound address", ips)
	}
}

func TestMDNSAnnouncementAndGoodbye(t *testing.T) {
	records := newMDNSRecords("hub", "Hub", strings.Repeat("a", 64), 18499, 18498)
	ips := []netip.Addr{netip.MustParseAddr("192.168.1.2"), netip.MustParseAddr("fd00::2")}
	for _, ttl := range []uint32{mdnsTTL, 0} {
		packet, err := records.announcement(ips, ttl)
		if err != nil {
			t.Fatal(err)
		}
		var msg dnsmessage.Message
		if err := msg.Unpack(packet); err != nil {
			t.Fatal(err)
		}
		if !msg.Response || !msg.Authoritative || len(msg.Answers) != 5 || len(msg.Questions) != 0 {
			t.Fatalf("invalid announcement: %+v", msg)
		}
		for _, rr := range msg.Answers {
			if rr.Header.TTL != ttl {
				t.Fatalf("%s TTL = %d, want %d", rr.Header.Type, rr.Header.TTL, ttl)
			}
		}
	}
}
