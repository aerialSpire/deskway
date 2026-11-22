package server

import "deskway/internal/proto"

// A computer's record of what AI clients did there, read by its account's browsers
// (docs/security.md, "AI clients at the computer"): a page asks for a page of calls
// (auditreq, with its query), the server asks the computer on behalf of that socket, and the
// computer's answer (audit) goes back to it. Read only; an AI client's login may not ask
// (aiMay).

const (
	auditQueryBytes = 4 << 10
	auditPageBytes  = 1 << 20
)

// onAuditReq asks computer `device` for a page of its calls, for browser c.
func (s *Server) onAuditReq(c *conn, device string, query []byte) {
	if len(query) == 0 || len(query) > auditQueryBytes || !s.store.CanAccess(c.user, device) {
		return
	}
	if h := s.hostOf(device); h != nil {
		h.send(proto.Msg{Type: proto.TypeAuditReq, Probe: c.probe, Query: query})
	}
}

// onAudit passes a computer's page to the browser that asked.
func (s *Server) onAudit(h *conn, m proto.Msg) {
	if m.Probe == "" || len(m.Page) == 0 || len(m.Page) > auditPageBytes {
		return
	}
	s.mu.Lock()
	var asked *conn
	for c := range s.clients {
		if c.probe == m.Probe {
			asked = c
		}
	}
	s.mu.Unlock()
	if asked == nil || asked.ai || !s.store.CanAccess(asked.user, h.deviceID) {
		return
	}
	asked.send(proto.Msg{Type: proto.TypeAudit, Device: h.deviceID, Page: m.Page})
}
