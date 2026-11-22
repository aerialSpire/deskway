package server

import (
	"encoding/json"
	"testing"

	"deskway/internal/proto"
)

// A page of a computer's recorded AI calls goes to the page of its account that asked, and to
// no other: not another account's, and an AI client's login cannot ask.
func TestARecordedPageGoesOnlyToThePageThatAsked(t *testing.T) {
	f := signalingFixture(t)
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	other := f.browserSocket(f.srv.URL, f.signedIn("me"))
	friend := f.browserSocket(f.srv.URL, f.signedIn("friend"))
	pc1 := f.hostSocket(f.srv.URL, "pc1")
	query := json.RawMessage(`{"limit":50,"tool":"run"}`)

	friend.send(proto.Msg{Type: proto.TypeAuditReq, Device: "pc1", Query: query})
	pc1.none(proto.TypeAuditReq, "")
	b.send(proto.Msg{Type: proto.TypeAuditReq, Device: "pc1", Query: query})
	ask := pc1.next(proto.TypeAuditReq)
	if ask.Probe == "" || string(ask.Query) != string(query) {
		t.Fatalf("asked: %+v", ask)
	}
	page := json.RawMessage(`{"calls":[],"total":0,"clients":[],"tools":[]}`)
	pc1.send(proto.Msg{Type: proto.TypeAudit, Probe: "forged", Page: page})
	pc1.send(proto.Msg{Type: proto.TypeAudit, Probe: ask.Probe, Page: page})
	if got := b.next(proto.TypeAudit); got.Device != "pc1" || string(got.Page) != string(page) {
		t.Fatalf("answered: %+v", got)
	}
	other.none(proto.TypeAudit, "")
	friend.none(proto.TypeAudit, "")
	if !aiMay(proto.Msg{Type: proto.TypeLocate}) || aiMay(proto.Msg{Type: proto.TypeAuditReq}) {
		t.Fatal("an AI client's login may say where it runs, not read the record")
	}
}
