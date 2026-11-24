# Protocol

This page is the readable companion to `proto/messages.json`, which defines every message, field
and type, and `proto/vectors.json`, which defines every signed or MACed string. Those two files
are the definition; this page explains how the messages fit together. The Go
(`internal/proto`), Rust (`crates/wire`) and JS (`web/static/js/proto.js`,
`web/static/js/auth.js`) implementations are checked against them by contract tests
(`go test ./internal/proto`, `cargo test -p deskway-wire`, `node tests/contract/*.test.mjs`).

## Channels

| Channel | Between | Transport | Discriminator |
|---|---|---|---|
| `signaling.browser` | browser and server | WebSocket `/ws/client` (login cookie) | `type` |
| `signaling.host` | host and server | WebSocket `/ws/host?id=` (device token) | `type` |
| `control` | browser and host | data channel `input`: reliable, ordered | `t` |
| `move` | browser and host | data channel `move`: unordered, no retransmits | `t` |
| `file` | browser and host | data channel `file`: reliable; text = messages, binary = bytes of the one transfer active in that direction | `t` |
| `ai` | AI client and host | data channel `ai`: reliable, ordered; MCP JSON-RPC in fragments (AI sessions) | JSON-RPC |
| `tunnel:<port>` | AI client and host | data channel: reliable, ordered; the bytes of one TCP connection to a loopback port (AI sessions) | none |
| `ipc` | Windows network service and agent | named pipe; length-prefixed bincode frames of `platform::Call` (service to agent) and `PlatformEvent` (agent to service), after an HMAC handshake; video frames and transfer bytes are raw frames | `t` |

Both picture modes use the H.264 video track.

The server relays signaling between a browser and a host. It sets identity itself (`user` and
`from` come from the login, never from the browser) and never reads `auth`: the host and the
browser verify offers, answers and pairing end to end.

A type name has the same meaning everywhere. When the same name appears on two channels
(`hello`, `bye`, `offer`), it is the same message as seen on each leg.

## Versions and capabilities

- `PROTOCOL_VERSION` is 4 (`protocol_version` in `messages.json`). Version 1 denotes everything
  from before `hello` existed.
- Every connection starts with `hello {version, caps}` in both directions: between browser and
  server, between host and server, and between browser and host on the control channel once it
  opens.
- The server copies a host's `version` and `caps` into the device list, so the browser knows
  them before it connects. The host's `hello` on the control channel is authoritative, since the
  server cannot alter it; if the two differ, the browser uses the control-channel one.
- A peer that sends no `hello`, or whose version is below `min_version` (3), is too old. The
  browser shows that the computer, the page or the server needs an update, and does not
  connect. There is no compatibility code for older versions.
- The server refuses no one because of their version; it only passes a host's `version` on in
  the device list. The check against `min_version` is made by the browser, both on the host's
  version in the device list and on its control-channel `hello`. On the control channel the
  browser accepts nothing from the host before that `hello`. Pointer positions on the move
  channel can arrive first, but they say nothing about the version, and `old` is decided from
  the `hello` alone.
- Within a version, optional features are gated only by capabilities. The browser adapts to
  `caps` and never infers support from the OS name, the browser, timing or a missing reply:
  | Cap | Without it |
  |---|---|
  | `input` | view only: no input is sent, input controls are hidden |
  | `files` | no file sending, no drop target |
  | `clipboard` | no shared clipboard; `clipshare` and `cb*` are not sent |
  | `cursor` | the pointer stays in the video; no `lc` |
  | `displays` | no display switching |
  | `relmouse` | no Pointer Lock mode |
  | `layout` | the on-screen keyboard draws default labels |
  | `chars` | characters with modifiers are not sent as `kc` |
  | `capture` | no "no display" state |
  | `desk` | no lock-screen note |
  | `ai` | no AI seat: AI clients cannot use the computer |
- A newer peer sends a new message or field only when the other side's version or capabilities
  show that it is understood. Receivers ignore unknown fields.
- To add a message or field, add it to `messages.json` with the new `since`, bump
  `protocol_version` if older peers must be distinguished, and update all three
  implementations. The contract tests fail until all of them agree.

## The computer you are using

```
browser / AI client          server                        host (each online computer of the account)
  |-- locate{where} -------->|-- locate{probe,where} ------->|   on every socket; again when a host comes online
  |<-- devices{..here..} ----|<--------- here{probe} --------|   only from the computer that is that place
```

This exchange is separate from connecting, and nothing else reads its result
(docs/session-lifecycle.md, "The computer you are using"). `where` holds at most 8 locators of
at most 128 characters each. `probe` is the server's id for the socket, so the host never
learns who asked.

## Connecting

```
browser                     server                        host
  |<------ ice{ice} ----------|   on every socket, renewed every 6 h (credentials last 12 h)
  |-- challenge{session} ---->|-- challenge{session} ------->|
  |<------ nonce{nonce} ------|<--------- nonce{nonce} ------|   single use, 60 s
  |-- offer{epoch 0,sdp,      |-- offer{epoch 0,sdp,view,    |
  |         view,auth} ------>|         ice,user,auth} ----->|   auth covers the nonce
  |<-- answer{epoch 0,sdp,auth}<------- answer -------------|   signed: deskway-answer-v1
  |<-> candidate ... ---------|<-> candidate ... ------------|
  |==================== data channels open ===================|
  |<------------- hello{version, caps} (control) -------------|
  |-------------- hello{version, caps} ---------------------->|
  |<------------- resume{id, k}, clipshare{on}, displays -----|
  |-------------- view{...}, sync{locks, mods}, lc{on} ------>|
```

- Nothing on the path to the answer waits for a network round trip that could be made earlier
  or later. The browser already has its STUN and TURN servers (`ice`) before a session starts.
  The host answers immediately with its host candidates; the address seen by a STUN server and
  the relay addresses follow as candidates when they are found. The `view` in the offer lets
  the host start capture and the encoder at the right size while ICE and DTLS are being set up.
  Frames are sent once DTLS is connected, starting with a keyframe. The `view` is not signed:
  the first `view` on the control channel confirms it end to end, and receiving the same
  constraints again changes nothing.
- While building its offer, the browser asks the host for a **nonce** (`challenge`). The host
  generates 16 random bytes (base64url), remembers them for this session for 60 seconds, and
  accepts one offer that uses them. The challenge is idempotent: if it is asked again for the
  same session, the host returns the same nonce as long as it is unused and still valid, so an
  offer already signed with it remains usable. There is no clock check and no list of seen
  session ids; an offer with a used, expired or unknown nonce is refused with
  `bye{reason:"auth"}`.
- With `auth.k = "dk"`, `sig` is the device key's ECDSA P-256 signature (r||s, base64url) over
  the SHA-256 of `deskway-connect-v3`. With `auth.k = "rs"`, `mac` is an HMAC-SHA256 over
  `deskway-resume-v3`, keyed with the resume token's key. Both cover the host id, the session,
  the nonce, `force` and the hash of the SDP (and with it the browser's DTLS fingerprint).
- The host signs the answer with its host key over `deskway-answer-v1`, and the browser checks
  the signature against the key it pinned when pairing.
- **Epochs.** Every `offer` and `restart` carries an `epoch` (0 for the offer, incremented by one
  for each restart), and every `answer` carries the epoch it answers. The browser waits for
  exactly one epoch at a time: a new restart replaces an unanswered one, an answer for any other
  epoch is ignored, and a bad signature on the current epoch's answer ends the session with
  `auth`. The host answers each epoch once. When the same offer or restart arrives again (the
  browser repeats whatever was unanswered when the computer or its own socket comes back), the
  host sends the same answer again.
- **Admission** (decided by the host). If nobody holds the computer, the session starts. If a
  session of the same device (the same device credential) is still connecting, the new one
  replaces it. Anyone else, or any established session, gets `bye{reason:"busy", holder}` (the
  server turns the holder into the browser's `controller`), unless the offer is signed with
  `force`. A forced offer takes over: the old session ends with `taken`, with `by` set to the
  taker's device name and `same` set when the taker is the same device. Reconnecting never takes
  over on its own.

## Session lifecycle

The complete lifecycle is specified in [session-lifecycle.md](session-lifecycle.md); this
section lists its messages.

- **Liveness** is determined by ICE consent freshness and the DTLS/SCTP state. There is no
  application-level keep-alive.
- **Recovery.** Losing the path does not end the session. The browser enters recovery on ICE
  `disconnected` or `failed` after the session was established; the host does so when ICE is
  disconnected or checking again after it had connected. The browser calls `restartIce()`,
  creates an offer and sends `restart{session, epoch, sdp}`. The host accepts it only if its
  DTLS fingerprint equals the session's. No new proof is needed, because the fingerprint ties
  the restart to the authenticated connection. The host then binds its local addresses again
  (opening new sockets for new addresses, contacting STUN and the relays again, trickling the
  new candidates and closing sockets of addresses that have disappeared) and replies with a
  signed `answer{epoch}`.
- **Deadlines** are the same on both ends: 30 s to connect (counted from the challenge) and 30 s
  to recover (counted from entering recovery). Whichever end gives up ends the session with
  `failed`.
- **Reasons** form a single list, used by both `bye` and the control channel's `end`: `left`,
  `taken`, `ended`, `busy`, `failed`, `gone`, `offline`, `auth`, `unpaired`, `forbidden`,
  `permissions`, `pair`, `denied`, `full`, `ai_off`, `idle` (the last two apply to AI sessions).
- **End.** Every end goes through the server (`bye{reason}`), in both directions and in every
  state, and is also sent on the control channel (`end{reason}`) while that channel is open.
  Whichever arrives first ends the session. A deadline, a failed peer connection and a closed
  control channel are ends like any other, and the other side is told about them. On `taken`
  and `ended` the host revokes the resume tokens of the controller that held the session (there
  are none when device checks are off), so the browser does not reconnect on its own.
- Held keys and buttons are released on every transition out of Active, and input that arrives
  outside Active is not applied.

## Signaling loss and server restart

Losing signaling ends nothing, and the server sends no `bye` when a socket drops.

```
host                       server (restarted)                 browser
  |-- hello, status{holder:{session S}} ->|                        |
  |                                       |<-- hello, attach{S} ---|
  |                                       |--- attached{S,online}->|  the holder's user is the login's
  |<------- restart{S, epoch, sdp} -------|<-- restart{S, ...} ----|  (only if ICE needs it)
```

- A host's `status` names the session holding it (`holder`: the session, the controller
  credential, the device name, the account, and whether it is established or still connecting).
  The host sends it after every connect and on every change. The server keeps the session
  records (`internal/server/sessions.go`). A status makes the session it names live, so the
  session is known again after a server restart; a live session that a status no longer names
  is over, and its page is sent `bye{ended}` immediately.
- A session belongs to the browser that started it (its account and browser record), not to a
  single socket. Any socket of that browser may speak for the session, and the session's
  messages go to the socket that last spoke for it. After a server restart, the session belongs
  to the browser whose device credential the host's status names.
- Whenever its socket comes back, the browser sends `attach{session, device}` for each session
  the server has already answered for (a nonce arrived). A session that has not been answered
  for yet sends its `challenge` again instead, and the same nonce comes back. The server binds
  the session to the socket and replies `attached{host}` (`online` or `away`), or `bye{gone}` if
  it does not know the session or the session belongs to another browser. After a server
  restart, an attach for a session that is not yet known waits for the host's first status (the
  page's own deadline bounds the wait). A challenge for a computer that has not been heard from
  since the server started waits for that computer to connect (`presence{away}`, then
  `online`, after which the page asks again). A computer that the server saw go offline has its
  challenge refused with `bye{offline}`.
- When a host's socket drops or comes back, every page with a session on that host is told with
  `presence{host: away|online}`. A recovering page restarts again when the computer is back, and
  a connecting page resends whatever was not answered.
- A session that never became live is removed when its nonce expires (60 s), and its page is
  sent `bye{failed}`.
- The device list's `controller {device, name, connecting}` is built for each browser from the
  host's holder, with `device` determined by comparing against that browser's own device
  credential. The list contains only the account's own computers (nothing is shared between
  accounts; see docs/accounts.md), so the holder is always a device of the same account.
- A browser starts or claims sessions only on its own account's computers, and speaks only for
  its own sessions. A host speaks only for sessions addressed to it.
- The ICE servers sent to a browser, and those given to a host with an offer, carry TURN
  credentials that name the account (the browser's account, or the session's), which the relay
  uses to limit each account's traffic.

## Clipboard (RDP CLIPRDR model)

The host's clipboard state `{gen, owner, formats}` is the source of truth. The host assigns
`gen`, and the latest announcement wins. The formats are `text`, `html`, `image` (PNG, `w`, `h`,
`size`) and `files` (`items` with names and sizes). Nothing is sent while `clipshare` is off.

```
copy on the host:      host -> cb{gen:7, owner:"host", formats, text?}    (text inline if <= 48 KB)
copy in the browser:   browser -> cb{formats, id}  host -> cb{gen:8, owner:"browser", formats}   (once placed)
                                             or -> cberr{gen:8, id, why}                   (not placed)
paste on the host:     host -> cbreq{gen:8, format, id}
                       browser -> cbdata{gen:8, format, id, text|html}            (small)
                            or -> cbdata{gen:8, format, id, size} + file put{id, gen:8, ...}
                            or -> cberr{gen:8, id, why}
Get it here (host owns): browser -> cbreq{gen:7, format:"image"|"files", id, i?}
                       host -> cbdata{gen:7, format, id, size} + file meta{id, gen:7}, bytes, fin{id}
```

- The browser announces only the list of formats. The host puts placeholders on its OS clipboard
  (delayed rendering) and requests the data with `cbreq` when something on the host pastes. The
  host answers the announcement only after the platform reports that the placeholders are in
  place (`ClipboardAnnounced`). If they cannot be placed, it answers with `cberr` carrying the
  announcement's `id` (`denied` when sharing is off or a secure desktop is active, `io` when the
  clipboard is busy), and the browser announces again at its next check. A paste waits at most
  30 s (`clipboard::RENDER_DEADLINE`). A format the browser did not provide stays empty for
  that generation (Windows asks again within the same paste).
- Text or HTML larger than 48 KB (`INLINE_MAX`), and every picture, goes over the file channel.
  The browser answers with `cbdata{size}` and sends `put{id, kind: "image"|"text"|"html", gen}`
  with the request's id; in the other direction the host sends `meta{id, gen}`, the chunks and
  `fin{id}`. The browser also requests host text larger than 48 KB on its own (`cbreq` with
  format `text`) and writes it to its clipboard when it arrives.
- A request for an older `gen` is answered with `cberr{why:"gone"}`, and with sharing off with
  `cberr{why:"denied"}`. The requester may cancel with `cberr{why:"cancelled"}` (and with the file
  channel's `cancel` for bytes already in transit); the host then gives the OS no data.
- `clipshare{on}`: the host sends the setting when the channel opens and whenever it changes,
  including changes made in its panel. The browser requests a change with the same message.
- Paste keys are ordinary keys.

## Video, input, files

- `view{w, h, maxW, maxH, fps}` is sent in the offer, when the channel opens, when the stage's
  size or the device pixel ratio changes, and when a zoom gesture ends (never on each step of
  one). `w` and `h` are the stage in device pixels multiplied by the zoom. `maxW`, `maxH` and
  `fps` describe the largest size that MediaCapabilities reports as smooth for H.264 High at the
  level that size requires (the viewport is used when the API is missing). The host encodes at
  `min(display, constraints)`, preserving the aspect ratio, at the lowest H.264 level that can
  hold it. Once the viewer has vouched for a decode box, the size is limited by the encoder's
  highest level; until then the offer's `profile-level-id` limits it, because browsers offer
  level 3.1 regardless of what they can decode. Each new size starts with a keyframe.
- `capture{v}` is `no-display` or `ready`. On `ready` the browser requests `capture-keyframe`
  and waits for the next presented frame (`requestVideoFrameCallback`).
- Keys are sent as positions (`k{c, d}`, `KeyboardEvent.code`). The host looks them up in a
  single table of every code (Windows set-1 scan codes and macOS virtual keys, as Chromium maps
  them). A code with no matching key on the host is logged once and skipped.
- `sync{locks, mods}`: browsers report modifier state only together with an event. So after the
  session starts, and whenever the page gets focus or becomes visible, the first key or pointer
  event sends a `sync` first, including modifiers the page itself holds (such as those of the
  on-screen keyboard). A modifier released without having been pressed on this page also sends
  one. The host sets its toggle keys and modifiers to match. On a Mac, browsers deliver no keyup
  for keys released while Command is held, so the browser releases them when Command goes up.
- `kc{ch, mods}` carries a character with modifiers (for example Ctrl tapped on the on-screen
  keyboard, followed by a letter from the phone's input method). The host finds the character's
  key in its own layout and types the combination, or types the character as text if the layout
  has no key for it.
- `layout{name, labels, shift}` describes what each key types on the host, without and with
  Shift. It is sent at session start and when the active layout changes (detected at the next
  key press). The on-screen keyboard draws these labels, and draws a US keyboard until they
  arrive.
- Pointer: `m{x, y, s}` (absolute) and `mr{dx, dy, s}` (relative, used in Pointer Lock, a
  session mode that Esc leaves) are sent on the move channel with a single increasing sequence
  number. After an `mr`, button presses leave the pointer where it is until the next `m`. When no
  new move follows by the next animation frame (the motion has stopped), the last `m` or `mr` is
  sent again on the control channel with the same sequence number. The host drops the duplicate
  if the first copy arrived, so a move lost at the end of a motion never leaves the pointer
  behind. `cp{x, y, v, s}` carries the last sequence number applied when the position was read.
  In absolute mode the browser ignores reports older than its latest move. In Pointer Lock the
  browser draws the visible cursor at the latest reported position, even while newer moves are
  in flight. The first movement after Pointer Lock starts or ends is discarded, because Chrome
  reports a spurious jump there.
- Files to the host: `put{id, kind, items}`, then `ready`, then the bytes, then `done{id, names}`
  (or `err{id, why}`). Either side may send `cancel{id}`. Files are saved to Downloads/Deskway
  and put on the clipboard; an image is only put on the clipboard. The end of a download is
  `fin`, because the name `end` is already used for the control channel's session end.

## AI sessions

An AI client's `deskway` connects the same way a browser does (challenge, signed offer with
`auth.k = "dk"`, signed answer, epochs and restarts), with the differences listed below. The
design is described in [architecture.md](architecture.md), "AI control".

- **Channels.** There is no media track and no bandwidth estimation. `control` carries `hello`
  and `end`. `ai` carries MCP JSON-RPC 2.0 as text. A message is at most 1 MiB (`MAX_MESSAGE`)
  and is sent in fragments of at most 64 KB (`FRAGMENT`, `wire::mcp`), which the receiver joins
  in order. An answer over the limit is replaced by a `too_large` error. `tunnel:<port>`
  channels are opened by the AI side during the session. Each carries one TCP connection to that
  loopback port of the computer and is accepted only for a port that this session's `tunnel`
  call allowed.
- **Admission on the AI seat.** The same AI device always replaces its own session, whether or
  not that session was established. Any other AI device gets `bye{busy}`, and an offer with
  media or with `force` is refused as `forbidden`. A person's session on the desk seat is not
  affected, and an AI session is not given a resume token.
- **Reasons.** `ai_off` means AI is not allowed on this computer or has been stopped at the
  computer (`computer_info` and the device list say which). `idle` means there was no
  `tools/call` for 10 minutes; the proxy connects again on the next call.
- **Status.** The host's `status` keeps `holder` for the desk seat and adds `ai` (the AI seat's
  holder) and `ai_state` (`off | on | stopped`). The server ends every session that neither field
  names. The device list keeps `controller` for the desk seat and adds `ai` (`AiView`: the
  state, whether administrator rights are allowed, and the client using it) and `tools`, the
  computer's tool names from its hello.
- **MCP.** The computer answers `tools/list`, `tools/call` and `ping`. The proxy itself answers
  `initialize` and the tool list for the MCP client, from `proto/tools.json`. Tool errors carry
  a code the AI client can act on (for example `ai_off`, `busy`, `secure_desktop`,
  `no_user_session`, `view_only`, `invalid_args`, `unsupported`, `too_large`, `elevation_off`),
  together with what happened and what to do next (`mcp::gate::ToolError`).
- **Sign-in.** A new client signs in with `POST /api/login/ai {user}`, which works only while the
  account accepts AI sign-ins (`POST /api/ai/window`). An approved client sends `{ts, sig}` over
  `deskway-ai-login-v1` instead. `GET /api/ai/clients` lists the programs the server distributes,
  with their SHA-256 hashes, and `GET /download/{file}` serves them.

## IPC (Windows)

`platform::Remote` implements `Platform` over the pipe with two enums: `platform::Call` (video,
input, clipboard, files, power, desktop, service settings) and `PlatformEvent` (displays,
capture, cursor, thumbnails, input desktop, layout, clipboard, transfer results, indicator
note). They are sent as length-prefixed bincode frames (`service/ipc.rs`). Both ends come from
the same build, so there is no version exchange; the HMAC handshake comes first. A new platform
capability needs one call, one event and two implementations. The host's proto tests check the
`ipc` entries of `proto/messages.json` that are marked current against these enums.

## Signed strings

Every string joins its parts with `\n` and has no trailing newline, except `vouch`, whose
entries each end in one. `sha256(x)` below is lower-case hex. The golden vectors in
`proto/vectors.json` give the inputs, the exact string and its SHA-256, and for MACs also the
key and the MAC.

| Format | Parts | Use |
|---|---|---|
| `deskway-connect-v3` | host, session, nonce, force 0/1, sha256(sdp) | device key signs SHA-256 of it, in an offer |
| `deskway-resume-v3` | host, session, nonce, force 0/1, sha256(sdp) | HMAC with the resume key, in an offer |
| `deskway-answer-v1` | host, session, sha256(offer), sha256(answer) | host key signs it |
| `deskway-endorse-v1` | cred, alg, key, name, ts | a device lets another in (device log) |
| `deskway-endorse-v2` | cred, alg, key, name, role, ts | a browser lets an AI client in (role `ai`; device log) |
| `deskway-ai-v1` | host, ai 0/1, admin 0/1, ts | a browser's AI switch for one computer (devlog `ai`) |
| `deskway-ai-login-v1` | account name, ts | an approved AI client's `deskway` signs in again with its device key (not built by the web client) |
| `deskway-remove-v1` | cred, ts | a device removes another (device log) |
| `deskway-rename-v1` | cred, name, ts | a device names a device of the account, itself or another (device log) |
| `deskway-policy-v1` | keys 0/1, ts | device checks on or off (device log) |
| `deskway-vouch-v1` | host, then `cred\nalg\nkey\n` per device sorted by cred | a new device vouches for its siblings |
| `deskway-code-v1` | key | six digits: first 4 bytes of SHA-256, big endian, mod 10^6 |
| `deskway-code-v2` | role, key | the same for an AI client (role `ai`) |
| `deskway-pair-v1` | host, cred, alg, key, name | HMAC with the normalized pairing code |
| `deskway-paired-v1` | host, cred, host key | HMAC with the normalized pairing code |

`web/static/js/auth-messages.js` (pure, with no browser state) builds each string with a function
named after its purpose (`connectChallenge`, `resumeMac`, `answerMessage`, `endorseChallenge`,
`removeChallenge`, `renameChallenge`, `policyChallenge`, `vouchChallenge`, `keyCodeMessage`,
`pairMessage`, `pairedMessage`, `aiChallenge`, plus `keyCode` and `normalizeCode`).
`tests/contract/vectors.test.mjs` maps each format to its builder and checks every vector marked
`current`. A format marked `new` gets its mapping once the client builds it, and the format it
replaces is then deleted together with its vectors and builders.
