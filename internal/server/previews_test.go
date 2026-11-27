package server

import (
	"bytes"
	"os"
	"reflect"
	"testing"
	"time"

	"deskway/internal/proto"
)

func TestPreviewRequestsIncludeVitalsOnlyForCapableHosts(t *testing.T) {
	f := signalingFixture(t)
	s := f.s
	host, viewer := fakeConn("", ""), fakeConn(f.id("me"), "")
	host.deviceID = "pc1"
	s.hosts[host.deviceID], s.clients[viewer] = host, true
	watch := proto.Msg{Type: proto.TypeWatch, On: true}

	for _, tc := range []struct {
		name    string
		version int
		caps    []string
		vitals  bool
	}{
		{"previous protocol", proto.MinVersion, nil, false},
		{"headless", proto.Version, []string{proto.CapVitals}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			host.version = 0
			s.asked[host.deviceID] = time.Time{}
			s.fromClient(viewer, watch)
			nothing(t, host) // wait for capabilities
			s.fromHost(host, proto.Msg{Type: proto.TypeHello, Version: tc.version, Caps: tc.caps}, func() {})
			if req := one(t, host); req.Type != proto.TypeThumbs || !req.On || req.Vitals != tc.vitals {
				t.Fatalf("preview request: %+v", req)
			}
			s.fromClient(viewer, watch)
			nothing(t, host)

			s.asked[host.deviceID] = time.Now().Add(-previewMaxAge)
			s.forwardThumb(host.deviceID, "preview", "screen")
			s.fromClient(viewer, watch)
			nothing(t, host) // previews are still fresh
			s.previews[host.deviceID]["screen"] = preview{data: "preview", at: time.Now().Add(-previewMaxAge)}
			s.fromClient(viewer, watch)
			if req := one(t, host); req.Type != proto.TypeThumbs || req.Vitals != tc.vitals {
				t.Fatalf("refresh request: %+v", req)
			}
			s.fromClient(viewer, proto.Msg{Type: proto.TypeWatch})
			s.asked[host.deviceID] = time.Now().Add(-previewMaxAge)
			s.requestPreviewLocked(host, time.Now())
			nothing(t, host)
		})
	}
}

func TestVitalsLiveOnTheHostConnectionAndAppIsSaved(t *testing.T) {
	f := signalingFixture(t)
	s := f.s
	host, viewer := fakeConn("", ""), fakeConn(f.id("me"), "")
	idle, other := fakeConn(viewer.user, ""), fakeConn("another-account", "")
	viewer.watching, other.watching = true, true
	host.deviceID = "pc1"
	s.hosts[host.deviceID] = host
	for _, client := range []*conn{viewer, idle, other} {
		s.clients[client] = true
	}
	machine := &proto.Machine{CPU: "Model", Cores: 4, Threads: 8, Memory: 16_000_000_000}
	s.fromHost(host, proto.Msg{Type: proto.TypeHello, Version: proto.Version, Caps: []string{proto.CapVitals}, App: "0.1.3", Machine: machine}, func() {})
	for _, client := range []*conn{viewer, idle, other} {
		got(client)
	}

	cpu, rtt := 12.3, uint64(0)
	memory := proto.Usage{Used: 4_000_000_000, Total: machine.Memory}
	disks := []proto.DiskUsage{}
	report := proto.Msg{Type: proto.TypeVitals, CPU: &cpu, Memory: &memory, Disks: &disks, RTT: &rtt}
	s.fromHost(host, report, nil)

	want := &proto.Vitals{CPU: cpu, Memory: memory, Disks: disks, RTT: &rtt}
	msg := one(t, viewer)
	if msg.Type != proto.TypeDevices || len(msg.Devices) != 1 {
		t.Fatalf("device list: %+v", msg)
	}
	online := msg.Devices[0]
	if !online.Online || online.App != "0.1.3" || !reflect.DeepEqual(online.Machine, machine) || !reflect.DeepEqual(online.Vitals, want) {
		t.Fatalf("online device: %+v", online)
	}
	nothing(t, idle)
	nothing(t, other)

	s.forgetHostLocked(host.deviceID)
	s.fromHost(host, report, nil)
	nothing(t, viewer)
	offline := s.deviceList(viewer)[0]
	if offline.Online || offline.Version != proto.Version || offline.App != "0.1.3" || offline.Vitals != nil {
		t.Fatalf("offline device: %+v", offline)
	}
	raw, err := os.ReadFile(f.data)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(raw, []byte(`"0.1.3"`)) || bytes.Contains(raw, []byte(`"vitals"`)) {
		t.Fatalf("unexpected saved state: %s", raw)
	}
}
