package server

import "deskway/internal/proto"

// The computer a browser or AI client runs on (docs/session-lifecycle.md, "The computer you
// are using"): it says where it runs (locate), the server asks each online computer of its
// account, and one that answers here is marked so in that socket's device list. A hint the
// page and the AI client show; nothing else depends on it.

const (
	locateMost    = 8
	locateLongest = 128
)

// onLocate takes where a browser or AI client runs: what an earlier one marked goes, and its
// account's online computers are asked.
func (s *Server) onLocate(c *conn, where []string) {
	var kept []string
	for _, w := range where {
		if len(kept) == locateMost {
			break
		}
		if w != "" && len(w) <= locateLongest {
			kept = append(kept, w)
		}
	}
	s.mu.Lock()
	c.where, c.here = kept, nil
	ask := proto.Msg{Type: proto.TypeLocate, Probe: c.probe, Where: kept}
	hosts := make([]*conn, 0, len(s.hosts))
	for _, h := range s.hosts {
		hosts = append(hosts, h)
	}
	s.mu.Unlock()
	if len(kept) > 0 {
		for _, h := range hosts {
			if s.store.CanAccess(c.user, h.deviceID) {
				h.send(ask)
			}
		}
	}
	c.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(c)})
}

// locateOn asks a computer just online about every socket of its account that said where it
// runs.
func (s *Server) locateOn(h *conn) {
	type asking struct {
		user string
		ask  proto.Msg
	}
	s.mu.Lock()
	var all []asking
	for c := range s.clients {
		if len(c.where) > 0 {
			all = append(all, asking{c.user, proto.Msg{Type: proto.TypeLocate, Probe: c.probe, Where: c.where}})
		}
	}
	s.mu.Unlock()
	for _, a := range all {
		if s.store.CanAccess(a.user, h.deviceID) {
			h.send(a.ask)
		}
	}
}

// onHere takes a computer's word that the socket it was asked about runs on it.
func (s *Server) onHere(h *conn, probe string) {
	if probe == "" {
		return
	}
	s.mu.Lock()
	var asked *conn
	for c := range s.clients {
		if c.probe == probe && len(c.where) > 0 {
			asked = c
		}
	}
	s.mu.Unlock()
	if asked == nil || !s.store.CanAccess(asked.user, h.deviceID) {
		return
	}
	s.mu.Lock()
	if asked.here == nil {
		asked.here = map[string]bool{}
	}
	asked.here[h.deviceID] = true
	s.mu.Unlock()
	asked.send(proto.Msg{Type: proto.TypeDevices, Devices: s.deviceList(asked)})
}
