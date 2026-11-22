package server

import (
	"time"

	"deskway/internal/proto"
)

// Previews: the overview's thumbnail of each computer's displays, kept in memory only.

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

// syncThumbs gives a browser that just opened the overview the previews already here, and asks
// a computer for new ones only if someone is looking and the ones here are old or missing.
// Previews are single pictures, not a stream: the computer captures once and stops.
func (s *Server) syncThumbs(viewer ...*conn) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, c := range viewer {
		for device, screens := range s.previews {
			if !s.store.CanAccess(c.user, device) {
				continue
			}
			for screen, p := range screens {
				c.send(proto.Msg{Type: proto.TypeThumb, Device: device, Data: p.data, Screen: screen})
			}
		}
	}
	now := time.Now()
	for id, h := range s.hosts {
		if !s.anyWatcherLocked(id) || now.Sub(s.asked[id]) < previewMaxAge {
			continue
		}
		fresh := len(s.previews[id]) > 0
		for _, p := range s.previews[id] {
			fresh = fresh && now.Sub(p.at) < previewMaxAge
		}
		if fresh {
			continue
		}
		s.asked[id] = now
		h.send(proto.Msg{Type: proto.TypeThumbs, On: true})
	}
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
