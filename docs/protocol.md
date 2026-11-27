# Protocol

This page is the readable companion to `proto/messages.json`, which defines every message, field
and type, `proto/version.json`, which defines the current protocol version, and `proto/vectors.json`,
which defines every signed or MACed string. The Go
(`internal/proto`), Rust (`crates/wire`) and JS (`web/static/js/proto.js`,
`web/static/js/auth.js`) implementations are checked against them by contract tests
(`go test ./internal/proto`, `cargo test -p deskway-wire`, `node --test "tests/contract/*.test.mjs"`).

## Channels

| Channel | Between | Transport | Discriminator |
|---|---|---|---|
| `signaling.browser` | browser and server | WebSocket `/ws/client` (login cookie) | `type` |
| `signaling.host` | host and server | WebSocket `/ws/host?id=` (device token) | `type` |
| `control` | browser and host | reliable, ordered data channel `input` for a picture, `control` for a data-only browser session | `t` |
| `move` | browser and host | data channel `move`: unordered, no retransmits | `t` |
| `file` | browser and host | data channel `file`: reliable; text = messages, binary = bytes of the one transfer active in that direction | `t` |
| `browse` | browser and host | reliable, ordered data channel `browse` with paged directory listings and folder operations | `t` |
| `file-transfer` | browser and host | reliable, ordered data channel `file-transfer` for regular file uploads and downloads | `t` |
| `terminal` | browser and host | data channel `terminal`: reliable, ordered. Text carries messages and binary carries raw PTY bytes | `t` |
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

- **Compatibility window.** Each release's server and web client support the current protocol
  version and keep all existing features working with hosts on the previous protocol version.
  The current version is 5 and the minimum is 4. Version 1 predates `hello`.
- Every connection starts with `hello {version, caps}` in both directions: between browser and
  server, between host and server, and between browser and host on the control channel once it
  opens.
- The host's signaling hello adds `app` (the installed host program version) and `machine`
  (CPU model, physical cores, logical threads and total memory in bytes). The server saves
  the reported protocol version and `app` for offline display. It uses the protocol version
  for `upgrade-check`. The online list reports the current connection's `version`, `caps` and
  `machine` once its hello arrives. The server accepts signaling
  sockets regardless of version. The browser refuses to connect when the current socket reports
  a version below the minimum and checks the version again in the host's authoritative
  control-channel `hello`. The browser waits for that `hello` before handling other host messages,
  including pointer positions that can arrive first on the move channel.
- Optional features are gated by capabilities. The browser adapts to
  `caps` and never infers support from the OS name, the browser, timing or a missing reply:
  | Cap | Without it |
  |---|---|
  | `input` | view only: no input is sent, input controls are hidden |
  | `files` | no file sending, no drop target |
  | `clipboard` | no shared clipboard; `clipshare` and `cb*` are not sent |
  | `cursor` | the pointer stays in the video; no `lc` |
  | `displays` | no display switching |
  | `start-display` | the host starts on the primary display and the browser restores its choice on the control channel if `displays` is supported |
  | `relmouse` | no Pointer Lock mode |
  | `layout` | the on-screen keyboard draws default labels |
  | `chars` | characters with modifiers are not sent as `kc` |
  | `capture` | no "no display" state |
  | `desk` | no lock-screen note |
  | `terminal` | no browser terminal |
  | `terminal-keep` | the browser terminal ends with its session |
  | `browse` | no file view on the data-only browser session |
  | `file-transfer` | Files has browsing and folder operations only |
  | `ai` | no AI seat: AI clients cannot use the computer |
  | `relay-rtt` | the host's signaling socket does not receive `relay-list` |
  | `vitals` | the server omits `vitals` on preview requests |
- A newer peer sends a new message or field only when the other side's version or capabilities
  show that it is understood. Receivers ignore unknown fields.
- To add a message or field, add it to `messages.json` with its `since` and update the
  implementations that use it. When a change requires a protocol bump, update `protocol_version`
  in `version.json` and the constants checked against it. Preserve interoperability with the
  previous host protocol.

On the host signaling socket, `relay-list {probe, relays}` gives STUN addresses by relay ID
on connect. When a node comes online or is removed, the server sends an update if the
addresses differ from the last list. The computer replies with
`relay-rtts {probe, rtts}`, carrying the shortest measured round-trip time in milliseconds
for each relay that answered. The server matches the reply to the current list using `probe`.

## Computer previews and vitals

When a browser sends `watch {on:true}`, it receives the cached previews. A host can be asked
for a refresh while a browser of its account is watching, when its previews are missing or at
least 30 seconds old. The same check applies when a host sends its hello. The server sends
`thumbs {on:true}` at most once every 30 seconds on a host connection. It adds `vitals:true`
for hosts with the capability. A headless host with that capability answers with vitals alone.

The host sends `vitals {cpu, memory, disks, rtt?}`. CPU is a percentage rounded to one decimal
place. Memory is `{used,total}` in bytes, and each disk is `{name,used,total,system?}`, with
capacities in bytes and the name taken from its mount point or drive letter. The host marks
the operating system's disk with `system:true`. The optional `rtt` is the latest
host-to-server WebSocket keepalive round trip in milliseconds. It first appears after the host
receives a matching pong. The server accepts one valid sample per request, includes it in
`devices`, and sends the updated list to watching browsers of the account. Going offline
clears the sample, previews, and request cooldown.

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
  |         view,display?,   |         display?,ice,user,  |
  |         auth} ---------->|         auth} ------------->|   auth covers the nonce
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
- A picture `offer` can carry `display`, the unsigned string id of the display to capture first.
  The browser sends it when the device list reports `start-display` and the remembered id is in
  that computer's display list. The server forwards it unchanged. The host uses the primary
  display when the field is omitted or the id is no longer attached. If the offer omits `display`,
  the browser restores its remembered choice via `display{v}` when the control channel is ready
  and the choice is in the live `displays` list.
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
  taker's device name and `same` set when the taker is the same device. The browser device
  that owns a session with transfers may use the same page's authenticated resume token to
  replace its own still-registered session without signing `force`. The old session ends
  with `failed`.
- A browser offer with a single enabled application media section selects the data-only
  browser session. The SDP hash in the existing proof binds this selection. Both browser
  session kinds share desk admission and the account's signed device-check policy.
  Linux refuses a picture with `no_desktop`. Hosts without the terminal capability refuse
  the data-only offer with `no_terminal`. The data-only session negotiates no video and its
  signaling offer carries zero viewer constraints. Linux, macOS and Windows support this
  session kind for control. The optional `workspace_access` field in host status is forwarded in the
  browser's device report. The values `locked`, `no_user_session` and `view_only` are also
  refusal reasons. Windows and macOS locks can end a data-only session with `locked`.
  Switching away from the Mac's login session also reports `locked`. On Windows, sign-out
  ends it with `no_user_session` and a direct change to a different user with `user_changed`.

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
  `permissions`, `no_desktop`, `no_terminal`, `pair`, `denied`, `full`, `ai_off`, `idle`
  (the last two apply to AI sessions).
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

## Browser files

The browser offers Files when the live signaling hello reports `browse`. After the signed
answer and the control hello confirm the host and its capabilities, the browser opens the
`browse` channel on that peer connection. The channel carries JSON text and accepts one
request awaiting a reply. A request or reply is at most 64 KiB.

`list` with no `path` starts at the host user's home. A path is an array of unpadded base64url
components. The empty array is the root. On Unix the root is `/` and the tokens preserve the
original bytes. macOS resolves existing names and rename collisions using the volume's
case and normalization rules.
On Windows the first token of a filesystem path is a drive or UNC share. Windows names use
UTF-8 where possible, and a 0xff byte followed by UTF-16LE units for names with unpaired
surrogates. The drive listing has `roots:true` and supports navigation. The host resolves the
directory on the first page, and `listing` returns that path, entries and an optional cursor.
Entries identified as directories by the directory scan come first, with each group ordered
by name bytes. The cursor starts the next page strictly after the last key, even if that entry is
deleted. A name inserted before the cursor will appear after refreshing the directory.
Listing a very large folder page by page costs one pass over its directory entries per page.
The browser sends the resolved path when requesting the next page or changing an entry.

`mkdir`, `rename` and `delete` reply with `changed` on success or `error{why}` on failure.
New names are one UTF-8 path component. The browser uses the listed name token to act on a
file with a non-UTF-8 name. Directory deletion requires an empty directory. Deleting a
symbolic link or Windows junction removes the link itself. Entries carry Unix `mode`
and `owner` where available, or an `attributes` list containing any of `read_only`,
`hidden` and `system`. A filesystem failure stays on the browse channel. This includes macOS
privacy denials, which appear as `error{why:"denied"}` at the requested folder.
When the host's two filesystem workers are occupied, the host replies with `error{why:"busy"}`
on the browse channel. A worker stays occupied until its call returns.

## Browser file transfers

The browser opens a separate reliable, ordered `file-transfer` channel after the control hello
confirms the capability. There is one active transfer. Attempts use increasing nonzero 32-bit
IDs in the browser page, with the same ID across reconnects for the same attempt. Each binary
frame starts with that ID and a 64-bit offset from the start, both unsigned
big-endian, and carries up to 16,372 bytes. The complete frame is at most 16 KiB. A receiver
grants four payloads of credit initially and returns credit through the absolute offset in
`ack` after consuming data. A sender waits for credit before reading any further. The host
services the terminal and browse channels before queuing another transfer frame. Waiting for
data, credit or a reply times out after 30 seconds and interrupts the channel attempt.

- For `upload{id,path,name,size}`, the host validates the destination and stages the file before
  sending `ready{id,size,offset}`, which grants initial credit at the staged offset. An existing
  regular file instead prompts with `conflict{id,size,modified}` (Unix seconds). The browser
  answers `replace{id}` or `cancel{id}`. The host acknowledges writes and the browser sends
  `finish{id,offset,sha256}` after all bytes are acknowledged. The host hashes the staged file
  and, under the desk-seat guard, checks the staging path still names that file and publishes it
  before replying `done{id,offset}`. On Windows, the host waits up to five seconds for the
  worker's publish reply after sending the request. If that wait expires, it records
  `error{id,why:"unconfirmed"}` as the terminal result. The file may already have been
  published. The host prompts if a destination appears before the no-replace rename. If an
  approved destination changes in identity, modification time or size, the host sends a new `conflict` with `changed:true` and waits for a fresh answer. Interrupting
  that question keeps the original approval, and a resumed attempt checks the destination
  and asks again if it still differs.
  Only regular files are eligible for replacement. Browse reports `in_use` when a rename or
  delete targets a live partial upload.
- For `download{id,path,entry,limit?}`, the host opens one regular file and checks its size
  against the optional byte limit. It replies `ready{id,size,offset:0}`. The browser grants initial
  credit with `start{id,offset}` at its written offset and acknowledges bytes as it consumes them.
  The host checks the original size and modification time, hashes the open file to EOF, and
  checks the count against the advertised size. It rereads the written prefix on resume.
  After the last acknowledgment it sends `finish{id,offset,sha256}`. The browser compares the
  digest with its incremental hash, closes a streaming destination or retains the verified bytes
  in memory for Save, then sends `done{id,offset}` and receives the host's echo. After `finish`,
  a resume at EOF reuses the digest. A browser that has finished locally can reply to `ready`
  with `done` at EOF. Digests are lowercase SHA-256 hex in both directions.
- An authenticated page reopens the channel on recovery and sends `resume{id}`. It may resend the
  initial request if it has received no offer response. The host repeats any terminal result
  still pending on that ID. The start offset of a new attempt is zero.
- The browser can send `cancel{id}`, or `error{id,why}` for local I/O, protocol, or integrity
  failures. The host sends `error` when a transfer fails or is cancelled, after cleaning up
  any staged upload.

## Browser terminal

The browser offers a terminal only when the live signaling hello reports `terminal`, verifies
the answer as usual, and waits for the control hello to confirm the capability. These additions
are gated by that capability within protocol 4.

The browser sends `open{rows, cols}` the first time the Terminal view is shown and can
size the emulator. After `open`, the host attaches the desk session to its one login shell,
starting it if needed, and sends `opened` before any binary output. Binary messages carry raw
bytes in both directions. `resize{rows, cols}` changes the PTY size. Ending the desk session,
losing the terminal channel or sending `close` detaches the browser. An ICE restart on the
same connection preserves its attachment and credit.

A host with `terminal-keep` accepts `terminate` only from an attached session and replies
with `closed` after the shell and jobs are gone. On failure it sends `error{why: "close"}`
and keeps the attachment. The host sends `exit{code}` after the final output, using a negative
number for a signal. Once the browser has processed the preceding output, `closed` or `exit`
completes a requested close. Otherwise the browser shows the exit result. An attachment
failure carries `error{why}`. On older hosts the shell ends with the session.

The host keeps the most recent 256 KiB (262,144 bytes) of PTY output in memory and keeps
draining the PTY while detached. When trimming, it tracks UTF-8 continuation bytes and
the terminal parser's escape states and discards through the next ground boundary. This
also discards an escape string longer than the whole buffer. On return, an `opened` message
with `replay` gives the byte count before live output, including any DEC private mode
settings that precede the retained bytes. The scanner keeps a bounded list of the latest
DECSET and DECRST settings at the cut. The browser returns its ordinary output credit for
replay and shows a localized notice when `opened.dropped` is true.
Once the emulator acknowledges replay, the host applies the latest size and attempts to
notify the foreground program even when the dimensions are identical. It logs a failure
and continues with live I/O. On Windows the size is set with `ResizePseudoConsole`.
ConPTY controls whether it emits a redraw at the same size. Deskway forwards the emulator's
UTF-8 bytes and replay restores the modes observed in ConPTY's output. ConPTY can remove
bracketed-paste markers for a program without virtual terminal input enabled, in which case
the paste arrives as text.

Host `status` carries optional `kept_terminal` only while the shell is detached. It carries
`since` (Unix seconds), `name`, and the verified device credential when available. The server
resolves the account's current device name and forwards the time and name to the account's
browsers in `devices`. The host measures the 24-hour detached lifetime with a monotonic
clock. Older servers and browsers can ignore the display field.

Messages are at most 16 KiB so one message cannot monopolize the SCTP association. The PTY
reader drains into a bounded queue. The host pauses delivery to the browser when the channel
buffers 64 KiB and resumes below 16 KiB, leaving room for control messages while avoiding
constant stop and start. The host also caps output
at 128 KiB awaiting `ack{bytes}`, which the browser sends after xterm.js processes the
bytes. That second limit keeps rendering bounded even when delivery is fast. Live input can
continue while live output is paused. The browser starts with 64 KiB of input credit, enough
for four full frames while bounding the host's pending input. It queues the remainder
of a paste and sends it in order as the host returns credit with `ack{bytes}` after writing
those bytes to the PTY.

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
  as a JSON array with their SHA-256 hashes, and `GET /download/{file}` serves them.
  `GET /api/ai/clients?details=1` returns an object with that array as `files` and an
  `errors` object mapping unavailable filenames to `download`, `checksum` or `storage`.

## IPC (Windows)

`platform::Remote` implements `Platform` over the pipe with two enums: `platform::Call` (video,
input, clipboard, files, power, desktop, service settings) and `PlatformEvent` (displays,
capture, cursor, thumbnails, input desktop, layout, clipboard, transfer results, indicator
note). They are sent as length-prefixed bincode frames (`service/ipc.rs`). Both ends come from
the same build, so there is no version exchange; the HMAC handshake comes first. `VideoStart`
carries the viewer's constraints and the optional initial display id. A new platform
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
