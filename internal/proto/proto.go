// Package proto defines the JSON messages exchanged over signaling WebSockets. The single
// definition is proto/messages.json (channels signaling.browser and signaling.host);
// contract_test.go checks these types against it.
package proto

import "encoding/json"

// Version is the protocol version this server speaks. MinVersion is the oldest host version a
// browser accepts; the server refuses no one for it and only passes a host's version on
// (docs/protocol.md, "Versions and capabilities").
const (
	Version    = 5
	MinVersion = Version - 1
)

// Signaling message types.
const (
	TypeHello     = "hello"     // every side, first message: version and capabilities
	TypeDevices   = "devices"   // server -> browser: device list
	TypeApprovals = "approvals" // server -> browser: browsers and computers waiting to be let in
	TypeICE       = "ice"       // server -> browser: STUN and TURN servers, fresh credentials
	TypeWatch     = "watch"     // browser -> server: overview open or closed
	TypeThumbs    = "thumbs"    // server -> host: capture previews and optionally sample vitals
	TypeVitals    = "vitals"    // host -> server: requested load and latency
	TypeThumb     = "thumb"     // host -> browser, via server
	TypeStatus    = "status"    // host -> server: the session holding it, its displays
	TypeDeviceLog = "devlog"    // server -> host: the owner's signed device log, in Auth
	TypeAccount   = "account"   // server -> host: its account's sign-in name and display name
	TypeChallenge = "challenge" // browser -> host: ask for a connect nonce
	TypeNonce     = "nonce"     // host -> browser: the single-use connect nonce
	TypeOffer     = "offer"     // browser -> host, via server
	TypeAnswer    = "answer"    // host -> browser
	TypeRestart   = "restart"   // browser -> host: ICE restart offer for a running session
	TypeCandidate = "candidate" // both directions
	TypeAttach    = "attach"    // browser -> server: claim a running session after reconnecting
	TypeAttached  = "attached"  // server -> browser: the session is bound to this socket again
	TypePresence  = "presence"  // server -> browser: the computer's socket dropped or came back
	TypeBye       = "bye"       // both directions: an end or refusal through the server
	TypePair      = "pair"      // browser -> host: pair a device using the code shown on the host
	TypePairAsk   = "pairask"   // browser -> host: ask the person at the computer to let this device in
	TypePaired    = "paired"    // host -> browser: pairing accepted, with the host's public key
	TypeLocate    = "locate"    // browser -> server -> host: where a browser or AI client runs
	TypeHere      = "here"      // host -> server: that is this computer
	TypeAuditReq  = "auditreq"  // browser -> server -> host: the computer's record of AI clients' calls
	TypeAudit     = "audit"     // host -> server -> browser: that record

	TypeRelayList = "relay-list" // server -> host: relays to measure
	TypeRelayRTTs = "relay-rtts" // host -> server: the measured round-trip times
)

const (
	CapRelayRTT  = "relay-rtt"
	CapVitals    = "vitals"
	CapTerminals = "terminals"
)

// MaxAuth bounds the opaque end-to-end authentication blob carried with offers, answers and
// pairing messages. The server never interprets it: the host and browser verify it themselves.
const MaxAuth = 16 << 10

// Approval is a new browser or computer waiting for an approved browser to let it in.
type Approval struct {
	Kind    string `json:"kind"` // "browser" or "device"
	ID      string `json:"id"`
	Label   string `json:"label"`
	IP      string `json:"ip"`
	Code    string `json:"code,omitempty"` // shown on both browsers so a person can compare them
	Created int64  `json:"created"`
	// Device is the new browser's device key, for the approving device to sign.
	Device json.RawMessage `json:"device,omitempty"`
}

// View is the browser's viewer constraints, carried with an offer.
type View struct {
	W    int `json:"w"`
	H    int `json:"h"`
	MaxW int `json:"maxW"`
	MaxH int `json:"maxH"`
	FPS  int `json:"fps"`
}

// ICEServer mirrors the browser's RTCIceServer dictionary.
type ICEServer struct {
	URLs       []string `json:"urls"`
	Username   string   `json:"username,omitempty"`
	Credential string   `json:"credential,omitempty"`
}

// Device is a computer as shown to the account that owns it.
type Device struct {
	ID         string      `json:"id"`
	Name       string      `json:"name"`
	OS         string      `json:"os"`
	Online     bool        `json:"online"`
	LastSeen   int64       `json:"last_seen,omitempty"`
	Controller *Controller `json:"controller,omitempty"` // who holds it now; absent: nobody
	// Displays attached to the computer while it is online: id, x, y, w, h, primary.
	Displays json.RawMessage `json:"displays,omitempty"`
	// The terminal that is running without a browser attached, while online.
	WorkspaceAccess string           `json:"workspace_access,omitempty"`
	KeptTerminal    *KeptTerminal    `json:"kept_terminal,omitempty"`
	Terminals       []TerminalStatus `json:"terminals,omitempty"`
	// The current socket's hello while online, or the last saved version while offline.
	Version int      `json:"version,omitempty"`
	App     string   `json:"app,omitempty"`
	Machine *Machine `json:"machine,omitempty"` // hardware from the current hello
	Vitals  *Vitals  `json:"vitals,omitempty"`  // the current connection's latest sample
	// The host's capabilities while it is online.
	Caps []string `json:"caps,omitempty"`
	// AI clients on this computer: the switch, and who uses it now; nil: never switched.
	AI *AiView `json:"ai,omitempty"`
	// The browser or AI client this list is for runs on this computer (locate).
	Here bool `json:"here,omitempty"`
	// The AI tools the computer last reported; nil: it did not say.
	Tools []string `json:"tools,omitempty"`
}

type Machine struct {
	CPU     string `json:"cpu"`
	Cores   uint32 `json:"cores"`
	Threads uint32 `json:"threads"`
	Memory  uint64 `json:"memory"`
}

type Usage struct {
	// Pointers distinguish a missing reading from zero when decoding a host report.
	Used  *uint64 `json:"used"`
	Total *uint64 `json:"total"`
}

type DiskUsage struct {
	Name string `json:"name"`
	Usage
	System bool `json:"system,omitempty"`
}

type Vitals struct {
	CPU    float64     `json:"cpu"`
	Memory Usage       `json:"memory"`
	Disks  []DiskUsage `json:"disks"`
	RTT    *uint64     `json:"rtt,omitempty"`
}

// The states of AiView.
const (
	AIOff     = "off"
	AIOn      = "on"
	AIStopped = "stopped"
)

// AiView is AI clients on a computer, as its account sees it: the switch (from the latest
// signed one, or as the computer itself reports it once it is online) and the AI client using
// the computer now.
type AiView struct {
	State string `json:"state"` // AIOff, AIOn or AIStopped
	Admin bool   `json:"admin"`
	TS    int64  `json:"ts"`
	Name  string `json:"name,omitempty"`
	Since int64  `json:"since,omitempty"`
	// The computer refused the latest switch: no device it trusts signed it. It takes it once
	// one does (added at the computer, or endorsed by a device it trusts).
	Refused bool `json:"refused,omitempty"`
}

// Holder is the session holding a computer (established or still connecting), as its host
// reports it.
type Holder struct {
	Session     string `json:"session"`
	Cred        string `json:"cred,omitempty"` // the controller's device credential; empty with device checks off
	Name        string `json:"name"`           // the controller's device name
	User        string `json:"user"`           // the controller's account id
	Established bool   `json:"established"`
	Since       int64  `json:"since,omitempty"` // unix seconds: since when it holds the computer
}

// KeptTerminal describes the last attachment to a detached terminal.
type KeptTerminal struct {
	Since int64  `json:"since"`
	Name  string `json:"name"`
	Cred  string `json:"cred,omitempty"` // host status only
}

// TerminalStatus describes a running browser terminal and its last attachment.
type TerminalStatus struct {
	KeptTerminal        // Since is the shell's creation time here.
	ID           uint64 `json:"id"`
	Attached     bool   `json:"attached"`
	Left         int64  `json:"left,omitempty"`
}

// Controller is who holds a computer, as one browser of its account sees it.
type Controller struct {
	Device     bool   `json:"device"`     // the holder is this browser's device; else another device of the account
	Name       string `json:"name"`       // the holder's device name
	Connecting bool   `json:"connecting"` // the holder is still connecting
}

// Hello is the first message each way. Unlike Msg it always carries caps, even when empty.
type Hello struct {
	Type    string   `json:"type"`
	Version int      `json:"version"`
	Caps    []string `json:"caps"`
	Build   string   `json:"build,omitempty"` // to browsers: which client files the server serves
}

// Msg is the single signaling envelope.
type Msg struct {
	Type       string          `json:"type"`
	Version    int             `json:"version,omitempty"` // hello
	Caps       []string        `json:"caps,omitempty"`    // hello
	Tools      []string        `json:"tools,omitempty"`   // a host's hello: the AI tools it carries out
	App        string          `json:"app,omitempty"`     // host hello
	Machine    *Machine        `json:"machine,omitempty"` // host hello
	Vitals     bool            `json:"vitals,omitempty"`  // thumbs: request load and latency
	CPU        *float64        `json:"cpu,omitempty"`     // vitals
	Memory     *Usage          `json:"memory,omitempty"`  // vitals
	Disks      *[]DiskUsage    `json:"disks,omitempty"`   // vitals, including an empty list
	RTT        *uint64         `json:"rtt,omitempty"`     // vitals
	Build      string          `json:"build,omitempty"`   // hello, to browsers
	Session    string          `json:"session,omitempty"`
	Device     string          `json:"device,omitempty"`
	Epoch      *int            `json:"epoch,omitempty"` // offer, restart, answer: a pointer, so 0 is sent
	SDP        string          `json:"sdp,omitempty"`
	Candidate  json.RawMessage `json:"candidate,omitempty"`
	ICE        []ICEServer     `json:"ice,omitempty"`
	View       *View           `json:"view,omitempty"` // offer
	Devices    []Device        `json:"devices,omitempty"`
	Approvals  []Approval      `json:"approvals,omitempty"`
	Reason     string          `json:"reason,omitempty"`     // why a bye
	By         string          `json:"by,omitempty"`         // taken: the taker's device name
	Same       bool            `json:"same,omitempty"`       // taken: the taker is the same device
	Holder     *Holder         `json:"holder,omitempty"`     // status, and a host's busy bye
	AIHolder   *Holder         `json:"ai_session,omitempty"` // status: the AI client on the AI seat
	AIState    string          `json:"ai_state,omitempty"`   // status: off, on or stopped
	AIRefused  int64           `json:"ai_refused,omitempty"` // status: the time of the newest switch it refused
	Controller *Controller     `json:"controller,omitempty"` // a busy bye to a browser
	Host       string          `json:"host,omitempty"`       // attached, presence: online or away
	From       string          `json:"from,omitempty"`       // pairing: the account's display name, set by the server
	User       string          `json:"user,omitempty"`       // the browser's account, set by the server; account: the sign-in name
	Display    string          `json:"display,omitempty"`    // offer: initial display id; account: the name others see
	Force      bool            `json:"force,omitempty"`      // take over the session holding the computer
	On         bool            `json:"on,omitempty"`
	Data       string          `json:"data,omitempty"` // base64 JPEG thumbnail
	Nonce      string          `json:"nonce,omitempty"`
	Auth       json.RawMessage `json:"auth,omitempty"`
	Displays   json.RawMessage `json:"displays,omitempty"`
	Screen     string          `json:"screen,omitempty"` // which display a thumbnail shows
	AI         *AiSwitch       `json:"ai,omitempty"`     // devlog: the computer's latest AI switch
	Where      []string        `json:"where,omitempty"`  // locate: where a browser or AI client runs
	Probe      string          `json:"probe,omitempty"`  // matches a host's reply to a request
	Query      json.RawMessage `json:"query,omitempty"`  // auditreq: which recorded AI calls
	Page       json.RawMessage `json:"page,omitempty"`   // audit: a page of them, from the computer

	WorkspaceAccess string           `json:"workspace_access,omitempty"`
	KeptTerminal    *KeptTerminal    `json:"kept_terminal,omitempty"` // status: a terminal running without a browser attached
	Terminals       []TerminalStatus `json:"terminals,omitempty"`     // status: all running browser terminals

	Relays    *map[string]string `json:"relays,omitempty"` // relay-list: STUN addresses by ID; send even when empty
	RelayRTTs map[string]int     `json:"rtts,omitempty"`   // relay-rtts: milliseconds by relay ID
}
