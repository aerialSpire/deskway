package server

import (
	"strings"
	"testing"

	"deskway/internal/proto"
)

// marked is whether the device list b receives next marks computer id as where b runs.
func marked(b *peer, id string) bool {
	for _, d := range b.next(proto.TypeDevices).Devices {
		if d.ID == id {
			return d.Here
		}
	}
	return false
}

// Where a browser runs reaches only its own account's computers, and the computer that says it
// is that one is marked so for that socket alone: not for another page of the account, not on
// a forged answer, and not after it says it runs somewhere else.
func TestTheComputerABrowserRunsOnIsMarkedForThatBrowserOnly(t *testing.T) {
	f := signalingFixture(t)
	f.s.store.putDevice("pc2", &DeviceRecord{Owner: f.id("me"), Name: "PC2", TokenHash: hashToken("tok-pc2")})
	b := f.browserSocket(f.srv.URL, f.signedIn("me"))
	other := f.browserSocket(f.srv.URL, f.signedIn("me"))
	friend := f.browserSocket(f.srv.URL, f.signedIn("friend"))
	pc1 := f.hostSocket(f.srv.URL, "pc1")

	// Empty and too long locators are dropped, and no more than locateMost are passed on.
	where := []string{"", strings.Repeat("a", locateLongest+1)}
	for i := 0; i < locateMost+2; i++ {
		where = append(where, "n.local")
	}
	b.send(proto.Msg{Type: proto.TypeLocate, Where: where})
	ask := pc1.next(proto.TypeLocate)
	if ask.Probe == "" || len(ask.Where) != locateMost || ask.Where[0] != "n.local" {
		t.Fatalf("asked: %+v", ask)
	}
	friend.send(proto.Msg{Type: proto.TypeLocate, Where: []string{"f.local"}})
	pc1.none(proto.TypeLocate, "")

	for _, c := range []struct {
		name  string
		probe string
		want  bool
	}{
		{"an answer for no socket", "forged", false},
		{"the computer says it is where b runs", ask.Probe, true},
	} {
		pc1.send(proto.Msg{Type: proto.TypeHere, Probe: c.probe})
		pc1.send(proto.Msg{Type: proto.TypeStatus}) // a device list for everyone, either way
		for marked(b, "pc1") != c.want { // until the list after the answer
		}
		if marked(other, "pc1") {
			t.Fatalf("%s: marked for another page", c.name)
		}
	}

	// A computer coming online is asked too.
	pc2 := f.hostSocket(f.srv.URL, "pc2")
	if again := pc2.next(proto.TypeLocate); again.Probe != ask.Probe {
		t.Fatalf("pc2 asked: %+v", again)
	}
	// Somewhere else now: what was marked goes.
	b.send(proto.Msg{Type: proto.TypeLocate, Where: []string{"m.local"}})
	for marked(b, "pc1") { // lists sent before it may still be on their way; none after
	}
}
