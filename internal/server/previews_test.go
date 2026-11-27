package server

import (
	"bytes"
	"encoding/json"
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
			s.forwardThumb(host, "preview", "screen")
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
	hello := proto.Msg{Type: proto.TypeHello, Version: proto.Version, Caps: []string{proto.CapVitals}, App: "0.1.3", Machine: machine}
	s.fromHost(host, hello, func() {})
	if req := one(t, host); req.Type != proto.TypeThumbs || !req.Vitals {
		t.Fatalf("preview request: %+v", req)
	}
	for _, client := range []*conn{viewer, idle, other} {
		got(client)
	}

	report := decodeVitals(t, `12.3`, `{"used":4000000000,"total":16000000000}`, `[{"name":"D:","used":0,"total":10,"system":true}]`)
	s.fromHost(host, report, nil)

	want := &proto.Vitals{CPU: *report.CPU, Memory: *report.Memory, Disks: *report.Disks, RTT: report.RTT}
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

	s.forwardThumb(host, "preview", "screen")
	got(viewer)
	s.forgetHostLocked(host.deviceID)
	if _, ok := s.asked[host.deviceID]; ok || len(s.previews[host.deviceID]) != 0 {
		t.Fatal("disconnect kept preview state")
	}
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

	reconnected := fakeConn("", "")
	reconnected.deviceID = host.deviceID
	s.hosts[host.deviceID] = reconnected
	s.forwardThumb(host, "late preview", "screen")
	nothing(t, viewer)
	s.fromHost(reconnected, hello, func() {})
	if req := one(t, reconnected); req.Type != proto.TypeThumbs || !req.Vitals {
		t.Fatalf("reconnected host request: %+v", req)
	}
}

func decodeVitals(t *testing.T, cpu, memory, disks string) proto.Msg {
	t.Helper()
	var m proto.Msg
	raw := `{"type":"vitals","cpu":` + cpu + `,"memory":` + memory + `,"disks":` + disks + `,"rtt":0}`
	if err := json.Unmarshal([]byte(raw), &m); err != nil {
		t.Fatal(err)
	}
	return m
}

func TestVitalsAcceptOneValidReportPerRequest(t *testing.T) {
	f := signalingFixture(t)
	s := f.s
	host, viewer := fakeConn("", ""), fakeConn(f.id("me"), "")
	host.deviceID, host.version, host.caps = "pc1", proto.Version, []string{proto.CapVitals}
	viewer.watching = true
	s.hosts[host.deviceID], s.clients[viewer] = host, true

	const memory = `{"used":0,"total":16}`
	const disks = `[{"name":"D:","used":0,"total":32}]`
	report := decodeVitals(t, "0", memory, disks)
	s.fromHost(host, report, nil)
	nothing(t, viewer)

	now := time.Now()
	s.requestPreviewLocked(host, now)
	one(t, host)
	for _, fields := range [][3]string{
		{"null", memory, disks},
		{"-0.1", memory, disks},
		{"100.1", memory, disks},
		{"0", `{"total":16}`, disks},
		{"0", `{"used":null,"total":16}`, disks},
		{"0", `{"used":0}`, disks},
		{"0", `{"used":0,"total":0}`, disks},
		{"0", `{"used":17,"total":16}`, disks},
		{"0", memory, `null`},
		{"0", memory, `[{"name":"D:","total":32}]`},
		{"0", memory, `[{"name":"D:","used":0}]`},
		{"0", memory, `[{"name":"D:","used":33,"total":32}]`},
	} {
		t.Run(fields[0]+"/"+fields[1]+"/"+fields[2], func(t *testing.T) {
			s.fromHost(host, decodeVitals(t, fields[0], fields[1], fields[2]), nil)
			nothing(t, viewer)
		})
	}
	s.fromHost(host, report, nil)
	one(t, viewer)
	s.fromHost(host, report, nil)
	nothing(t, viewer)

	s.requestPreviewLocked(host, now.Add(previewMaxAge))
	one(t, host)
	s.fromHost(host, decodeVitals(t, "100", memory, `[]`), nil)
	one(t, viewer)
}
