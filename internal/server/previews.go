package server

import (
	"slices"
	"time"

	"deskway/internal/proto"
)

// Previews and vitals for the overview, kept in memory only.

// preview is the latest thumbnail of one display, and when it arrived.
type preview struct {
	data string
	at   time.Time
}

const previewMaxAge = 30 * time.Second

// anyWatcherLocked reports whether a user who may see the device has the overview open.
func (s *Server) anyWatcherLocked(device string) bool {
	for c := range s.clients {
		if c.watching && s.store.CanAccess(c.user, device) {
			return true
		}
	}
	return false
}

// syncThumbs gives a browser that just opened the overview its cached previews and asks for
// a refresh. The same request goes to a host that sends its hello while someone is watching.
func (s *Server) syncThumbs(viewer *conn) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for device, screens := range s.previews {
		if !s.store.CanAccess(viewer.user, device) {
			continue
		}
		for screen, p := range screens {
			viewer.send(proto.Msg{Type: proto.TypeThumb, Device: device, Data: p.data, Screen: screen})
		}
	}
	now := time.Now()
	for _, host := range s.hosts {
		s.requestPreviewLocked(host, now)
	}
}

// The request is shared by previews and vitals, including hosts with no previews.
func (s *Server) requestPreviewLocked(host *conn, now time.Time) {
	id := host.deviceID
	if host.version == 0 || now.Sub(s.asked[id]) < previewMaxAge || !s.anyWatcherLocked(id) {
		return
	}
	fresh := len(s.previews[id]) > 0
	for _, p := range s.previews[id] {
		fresh = fresh && now.Sub(p.at) < previewMaxAge
	}
	if fresh {
		return
	}
	s.asked[id] = now
	host.send(proto.Msg{Type: proto.TypeThumbs, On: true, Vitals: slices.Contains(host.caps, proto.CapVitals)})
}

func (s *Server) onVitals(host *conn, m proto.Msg) {
	if m.CPU == nil || *m.CPU < 0 || *m.CPU > 100 || m.Memory == nil || m.Memory.Used > m.Memory.Total || m.Disks == nil || *m.Disks == nil {
		return
	}
	for _, disk := range *m.Disks {
		if disk.Name == "" || disk.Used > disk.Total {
			return
		}
	}
	s.mu.Lock()
	if s.hosts[host.deviceID] != host || !slices.Contains(host.caps, proto.CapVitals) {
		s.mu.Unlock()
		return
	}
	host.vitals = &proto.Vitals{CPU: *m.CPU, Memory: *m.Memory, Disks: *m.Disks, RTT: m.RTT}
	s.mu.Unlock()
	s.broadcastDevicesTo(func(c *conn) bool {
		return c.watching && s.store.CanAccess(c.user, host.deviceID)
	})
}

// forwardThumb relays a preview to whoever is looking and keeps the latest in memory (never on disk).
func (s *Server) forwardThumb(device, data, screen string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.previews[device] == nil {
		s.previews[device] = map[string]preview{}
	}
	if len(s.previews[device]) < 16 {
		s.previews[device][screen] = preview{data, time.Now()}
	}
	for c := range s.clients {
		if c.watching && s.store.CanAccess(c.user, device) {
			c.send(proto.Msg{Type: proto.TypeThumb, Device: device, Data: data, Screen: screen})
		}
	}
}
