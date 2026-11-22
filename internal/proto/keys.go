package proto

import (
	"crypto/ecdsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"math/big"
)

// The account's device keys and their signed log, as the HTTP API carries them and the host's
// devlog message delivers them (docs/security.md, "Devices").

// Operations of a LogEntry.
const (
	OpAdd    = "add"
	OpRemove = "remove"
	OpPolicy = "policy"
	OpRename = "rename"
)

// RoleAI marks the key of an AI client's deskway command; a browser's has no role. The role
// is part of what its approver signs (deskway-endorse-v2) and of its six digits (code-v2).
const RoleAI = "ai"

// BrowserKey is the public key a device signs with, as the computers check it: a browser's, or
// an AI client's (Role "ai"). The server only stores it.
type BrowserKey struct {
	Cred string `json:"cred"` // credential id, base64url
	Alg  int64  `json:"alg"`
	Key  string `json:"key"` // SubjectPublicKeyInfo, base64
	Name string `json:"name"`
	Role string `json:"role,omitempty"` // RoleAI, or empty for a browser
}

// Valid reports whether the key is one computers can check, within the server's size limits.
func (k *BrowserKey) Valid() bool {
	return k != nil && k.Cred != "" && len(k.Cred) <= 1024 && (k.Alg == -7 || k.Alg == -257) && k.Key != "" && len(k.Key) <= 2048 && len(k.Name) <= 120 && (k.Role == "" || k.Role == RoleAI)
}

// Code is the six digits the device and the one approving it both show. Each side computes
// them from the key (and an AI client's role) without trusting the server.
func (k BrowserKey) Code() string {
	s := "deskway-code-v1\n" + k.Key
	if k.Role == RoleAI {
		s = "deskway-code-v2\n" + RoleAI + "\n" + k.Key
	}
	h := sha256.Sum256([]byte(s))
	return fmt.Sprintf("%06d", binary.BigEndian.Uint32(h[:4])%1_000_000)
}

// AILogin is what an approved AI client signs to sign in again, without the account password:
// the account name it signs in as, and the time.
func AILogin(name string, ts int64) string {
	return fmt.Sprintf("deskway-ai-login-v1\n%s\n%d", name, ts)
}

// Verify checks sig on message as browsers and deskway sign: ECDSA P-256 with SHA-256 over the
// SHA-256 of the message, r||s, base64url.
func (k BrowserKey) Verify(message, sig string) bool {
	if k.Alg != -7 {
		return false
	}
	der, err := base64.StdEncoding.DecodeString(k.Key)
	if err != nil {
		return false
	}
	pub, err := x509.ParsePKIXPublicKey(der)
	ec, ok := pub.(*ecdsa.PublicKey)
	if err != nil || !ok {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(sig)
	if err != nil || len(raw) != 64 {
		return false
	}
	inner := sha256.Sum256([]byte(message))
	digest := sha256.Sum256(inner[:])
	return ecdsa.Verify(ec, digest[:], new(big.Int).SetBytes(raw[:32]), new(big.Int).SetBytes(raw[32:]))
}

// AddEntry is the log entry that adds this key, signed by p. With an unsigned p (only TS set)
// computers ignore the entry until the key is added at the computer itself.
func (k BrowserKey) AddEntry(p Proof) LogEntry {
	e := p.Entry(OpAdd)
	e.Cred, e.Alg, e.Key, e.Name, e.Role = k.Cred, k.Alg, k.Key, k.Name, k.Role
	return e
}

// LogEntry is one signed change to an account's device list. Computers replay the log and
// accept an entry only if a device they already trust signed it.
type LogEntry struct {
	Op   string `json:"op"` // OpAdd, OpRemove, OpPolicy or OpRename
	Cred string `json:"cred"`
	Alg  int64  `json:"alg,omitempty"`
	Key  string `json:"key,omitempty"`
	Name string `json:"name,omitempty"`
	TS   int64  `json:"ts"`
	By   string `json:"by,omitempty"`
	AD   string `json:"ad,omitempty"`
	CD   string `json:"cd,omitempty"`
	Sig  string `json:"sig,omitempty"`
	// Keys, for OpPolicy: whether computers require a device signature (default) or accept
	// the account password alone.
	Keys *bool `json:"keys,omitempty"`
	// Role, for OpAdd: RoleAI for an AI client, signed as deskway-endorse-v2.
	Role string `json:"role,omitempty"`
}

// AiSwitch is a browser's signed choice for one computer: whether AI clients may control it,
// and whether with administrator rights (deskway-ai-v1). The server keeps the latest per
// computer, outside the log; the computer takes it only if it is newer than the one it has.
type AiSwitch struct {
	Host  string `json:"host"`
	AI    bool   `json:"ai"`
	Admin bool   `json:"admin"`
	TS    int64  `json:"ts"`
	By    string `json:"by"`
	AD    string `json:"ad,omitempty"`
	CD    string `json:"cd,omitempty"`
	Sig   string `json:"sig"`
}

// Proof is a signature from one of the account's devices: a browser key's (just Sig) or a
// passkey assertion (AD, CD and Sig).
type Proof struct {
	TS  int64  `json:"ts"`
	By  string `json:"by"`
	AD  string `json:"ad"`
	CD  string `json:"cd"`
	Sig string `json:"sig"`
}

// Valid reports whether the proof names a signer and carries a signature, within the server's
// size limits. The computers verify the signature itself.
func (p *Proof) Valid() bool {
	return p != nil && p.By != "" && len(p.By) <= 1024 && len(p.AD) <= 4096 && len(p.CD) <= 4096 && len(p.Sig) <= 2048 && p.Sig != ""
}

// Entry is the log entry for op signed by this proof; the caller adds what it is about.
func (p Proof) Entry(op string) LogEntry {
	return LogEntry{Op: op, TS: p.TS, By: p.By, AD: p.AD, CD: p.CD, Sig: p.Sig}
}

// Switch is the AI switch for computer host signed by this proof.
func (p Proof) Switch(host string, ai, admin bool) AiSwitch {
	return AiSwitch{Host: host, AI: ai, Admin: admin, TS: p.TS, By: p.By, AD: p.AD, CD: p.CD, Sig: p.Sig}
}
