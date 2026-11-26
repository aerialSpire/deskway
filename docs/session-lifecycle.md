# Session lifecycle

A session is one controller (a browser page, or an AI client's `deskway`) connected to one
computer. This document is the complete design of the session lifecycle. Every side implements
it, and every case below has a test (see "Cases").

## Principles

1. A session ends only when a person ends it, when someone takes over, when the computer ends it
   (because it was signed out, the device was removed, or someone pressed **Disconnect** at the
   computer), or when it cannot be established or recovered within its deadline. Losing
   signaling never ends a session.
2. The person always sees the session's state: connecting, connected, recovering (and why: the
   network, the computer being away, or the server being unreachable), or ended (and why, and
   what can be done next).
3. Nothing waits forever. Every waiting state has a deadline and can be left at any time.
4. There is no ghost control: a session that no longer exists stops being shown as controlling
   within 30 s.
5. Reconnecting takes one tap and asks the right question. Nobody is ever taken over silently.
6. Each seat of a computer has at most one session (the desk seat for a person, and the AI seat
   next to it). Taking over always requires confirmation.
7. Held keys and buttons are released on both sides on every transition out of `active`.

## Owners

| Side | Owner | Owns |
|---|---|---|
| Browser | `Connection` (`web/static/js/session/connection.js`) | the peer connection, negotiation, the state and its deadlines, re-attaching, the end |
| Server | `Sessions` (`internal/server/sessions.go`) | the session records: which browser each belongs to, which of its sockets carries it now, routing |
| Host | `peer::state::Machine` (per session, the same on every end) and `signal::Host` with its `Registry` | the peer connection, the state and its deadlines, the end; which session controls the computer |

Everything else (video, input, clipboard, files, the toolbar, the pages) reads the state from its
owner and asks the owner when it wants the session ended. Nothing else ends, restarts or times
out a session.

## Authority

- The peer connection decides whether a session is alive, through ICE consent freshness
  (RFC 7675) and the DTLS/SCTP state. There are no application-level keep-alives.
- The host decides which session controls a computer. Its `status` names the session holding the
  computer (established or still connecting) and that session's controller.
- The server decides neither. It routes messages, remembers whom a session belongs to, and
  passes on what the host reports. It keeps no timer on a live session.
- Deadlines belong to the two ends and are the same on both: 30 s to connect (counted from the
  challenge) and 30 s to recover (counted from entering `recovering`).
- Identity is the device. The controller is identified by its device credential (the device key
  the host verified), never by comparing names. All pages of one browser share its device key, so
  they count as one device.
- A session belongs to the browser that started it (its account and browser record), never to a
  single socket. Any socket of that browser may act on the session, and the session's messages
  to the page go to the socket that last spoke for it.
- Only the session that holds the computer can act on it. Actions that affect the whole computer
  (input, releasing held keys and buttons, capture and its pacing) belong to the holding session,
  so the last actions of an ended or replaced session never reach its successor.

## States

Both ends use the states `connecting -> active <-> recovering`, and any of them can move to
`ended(reason)`.

- `connecting`: from the challenge until the control channel opens.
- `active`: the control channel is open.
- `recovering`: the network path has been lost; the peer connection is kept and ICE is
  restarted. The page enters this state on ICE `disconnected` and on `failed` alike, because a
  phone returning from the background often reports `failed` immediately, and a restart may
  still recover the path. The host enters it when ICE starts checking again after it had
  connected, just as on `disconnected`. The recovery deadline ends this state.
- `ended(reason)`: final.

## Negotiation

Only one negotiation runs at a time, and negotiations are numbered. Every `offer` and `restart`
carries an `epoch` (0 for the offer, incremented by one for each restart), and every `answer`
carries the epoch it answers. The browser's `Connection` waits for exactly one epoch:

- A restart issued while another is outstanding replaces it. The epoch moves on, and nothing is
  sent twice for the same epoch.
- An answer for any other epoch does not belong to this negotiation and is ignored.
- An answer for the current epoch is verified against the host's signature over it. A bad
  signature (`forged`), or a key other than the one stored for the computer (`keyChanged`), ends
  the session, because the computer's identity could not be confirmed. These are reasons of the
  page's own; they are shown as a warning that someone may be impersonating the computer (with
  an offer to add the device again only for a changed key), and the computer is told `auth`.

The challenge is idempotent: however often it is asked, the host gives a session the same nonce
until the nonce is used or expires. The host answers each offer or restart once, with its epoch.
On a restart it binds its local addresses again (its own network may have changed), probes STUN
and the relays again, and trickles the new candidates. Sockets for addresses that no longer
exist are closed.

## Admission (the host)

- If nobody holds the computer, the new session starts.
- If a session of **the same device** holds it but is still connecting, the new session replaces
  it (this is the same person trying again).
- For any other holder (another device that is connecting, or any established session,
  including one of the same device), the new session is refused with `busy` and a description of
  the holder, unless the offer was signed with `force`. A forced offer takes over, and the old
  session ends with `taken`.

## Who controls, as the browser sees it

The host's `status` names the holding session, its controller's device credential, its device
name and account, and whether it is established. The server turns this into each browser's
device list as `controller: { device, name, connecting }`, which is absent when nobody holds the
computer. Only the account that owns a computer can reach it (docs/accounts.md), so the holder is
always a device of the browser's own account:

- `device`: the holder is this browser's device (identified by its device key); otherwise it is
  another device of the account.
- `name`: the holder's device name.
- `connecting`: the holder is still connecting.

When the computer has a holder, the page asks one of these questions (`views/takeover.js`):

- `device`: "This device still has a remote connection open. The network may have dropped, or it is in
  another window. End it and connect again?"
  (a session of this device that is still connecting is replaced without asking; see Admission);
- otherwise: "Your {name} is controlling this computer. Take over?" (or, if the holder is still
  connecting, a question that says so).

**Terminal** and **Files** offers start without force, as do picture offers when the cached
holder is this device. The host's current holder decides whether the page asks about taking
over, including if another tab of this browser is connected. Both actions open the same
kind of session. The toolbar switches views on the one connection. An ICE restart preserves
the file view and terminal attachment.

Opening Files starts no shell. The first visit to Terminal in that session starts or
reattaches the shell. Switching views leaves an attached terminal running. Taking over
detaches the prior browser. The new browser reattaches to the same shell when it shows
Terminal. A session with no terminal has an ordinary Disconnect dialog.

The host keeps one browser terminal independently of the desk session. Disconnect, the end
of a session, or a lost terminal channel leaves the shell and its jobs running. The computer's
page shows when the last device left and offers **Return** while the terminal is detached.
During recovery the terminal view says that the terminal is running on the computer and
that missed output will appear when the browser returns. An established connection that
recovers in place continues its existing stream. Reconnecting after an end reopens the
view that was showing when the connection ended.

The shell ends on exit, **Close the terminal and disconnect**, 24 hours without an attachment,
or host shutdown. The deliberate close is available from the attached terminal's Disconnect
dialog. The host discards the buffer and cleans up the PTY and jobs on every terminal end.

When **Each computer in its own window** is on (a per-device setting, off by default), pressing a
computer asks nothing on this page. Instead, the computer's own window (`windows.js`,
`?computer=<id>`) is brought to the front if it is open; otherwise the window opens and takes the
computer over, asking its question there. When the person ends the session in that window, the
window closes. If the window takes over as it opens, outside any tap, it does not enter full
screen on its own.

## The computer you are using

A browser or AI client may be running on one of the account's own computers. That computer is
marked as such before anything connects, and connecting to it works exactly as for any other
computer: the mark is a hint, never a rule. This is a separate module (`peer::here`,
`internal/server/here.go`, `web/static/js/here.js`), and nothing else depends on its result.

- **Where a controller runs.** On every socket, the controller sends `locate{where}`. A browser
  cannot see its own addresses, but WebRTC can name them: a peer connection configured with no
  servers gathers the browser's mDNS host names (`<uuid>.local`), and it is kept open for the
  life of the page so that the names continue to resolve. An AI client identifies its machine
  instead, as `machine:` followed by the SHA-256 of the system's machine id (`MachineGuid` on
  Windows, `IOPlatformUUID` on a Mac); the id itself never leaves the computer.
- **Who answers.** The server asks each online computer of the account, and each one that comes
  online later, using an opaque probe id. A computer answers `here{probe}` only when one of the
  locators refers to itself: the machine locator equals its own, or an mDNS name resolves to one
  of its own addresses (loopback or an interface address). Resolution uses mDNS on the local link
  and never a DNS server. Responders answer the same question at most once a second, so a
  computer caches each name's answer.
- **What shows.** The server marks that computer as `here` only in the device list of the socket
  that sent the locators; other pages of the account are not marked, and a new `locate` clears
  any earlier mark. The page shows **This computer** next to the computer's name in the list and
  on the computer's page. Pressing its picture first asks **This is the computer you are using**,
  with **Control anyway** and **Cancel**. **Retry** and **Reconnect** after a session ends do
  not ask again. The AI client's `computers` lists the computer with `this_computer: true`.
- **Tests.** The e2e browser runs on the computer it is testing, so its page reports where it
  runs only when a test asks for it (`d.locating(true)`). All other areas therefore connect
  without the question.

## Ending

The reasons on the wire form a single list in `proto/messages.json`, used by both `bye` and the
control channel's `end`: `left`, `taken`, `ended`, `busy`, `failed`, `gone`, `offline`, `auth`,
`unpaired`, `forbidden`, `permissions`, `no_desktop`, `no_terminal`, `pair`, `denied`, `full`, `ai_off`, `idle`.
The browser handles them in `views/takeover.js`, with offer and takeover messages in
`views/offer-message.js`. The page also has its own reasons, which never travel on the wire:
no device key, the server being unreachable, and the computer's identity not being confirmed
(`keyChanged`, `forged`).

- **An end always goes through the server (`bye`), in both directions, and is also sent on the
  control channel (`end`) while that channel is open.** Whichever arrives first ends the session,
  and the other finds it already ended. A page or computer that has just come back cannot know
  whether its channel still carries anything, so it never relies on the channel alone. This
  applies to every way a session can end: a person's choice, a deadline, a failed peer
  connection or a closed control channel.
- `old`: the computer's control-channel hello is older than the page understands. This is
  decided from that hello alone, since nothing that arrives on any channel before it says
  anything about the version.
- `taken` carries `by` (the taker's device name), and `same` when the taker is the same device
  (D1). The host sends the status naming the taker only after the replaced session has said
  goodbye, so the server never mistakes the replacement for the computer ending the session.
- `failed`: the session could not be established or recovered in time, or the host could not
  start it. It is sent by whichever end gives up.
- `gone`: the session is not known (or no longer known) where it was asked about.
- When the host's `status` no longer names a session that the server knows, that session ends:
  the server immediately sends its page `bye{ended}`. This happens when the computer restarted,
  or ended the session while the page was away.

## Signaling loss

A signaling socket dropping ends nothing.

- **The browser's socket** comes back on its own: it is renewed after 10 s in the background and
  when the network returns. A session the server has already answered for (a nonce arrived) is
  attached: `attach` binds it to the new socket and is answered with `attached{host}`, carrying
  `online` or `away`, or with `bye{gone}` if the server does not know the session. A session the
  server has not yet answered for sends its challenge again on the new socket (which is
  idempotent) and never sends `attach`, because what went out on the old socket may still arrive
  after it. Messages queued while the socket was being renewed go out on the new socket and are
  accepted, because the session belongs to the browser and not to the socket.
- **The host's socket** drops: the server sends `presence{away}` to every page with a session on
  that computer. When the socket comes back, the server reconciles the host's first `status`
  before sending `presence{online}`. A session missing from that status ends without any attempt
  to restart ICE with the reconnected host. A page that is recovering restarts again on `online`,
  since whatever it sent in the meantime was not delivered. A page that is connecting resends
  whatever was not answered (the challenge, or the same offer, to which the host sends the same
  answer again).
- **After a server restart,** sessions become known again from the host's first status. An
  `attach` for a session that is not yet known waits for that status (the page's own deadline
  bounds the wait), and if the status does not name the session, the answer is `bye{gone}`. A
  challenge for a computer the server has not heard from since it started waits in the same way:
  the page gets `presence{away}`, then `online` when the computer connects, and then asks again.
  Only a computer that the server saw go offline refuses the challenge immediately with
  `bye{offline}`.
- A session that never became live (no status ever named it) is removed when its nonce expires
  (60 s), and its page is sent `bye{failed}`.
- While a session is `active`, the server being unreachable is not shown, because it changes
  nothing for the session. It is reported only where it matters: when a picture is pressed, and
  while recovering.

## Resume

After the control channel opens, the host gives the page a resume token, valid for 8 h. A new
session from the same page presents the token instead of signing with the device key. The page
discards the token on `taken`, `ended`, `unpaired` and `auth`. On the host, the token is revoked
only for the controller that held the session, and it is lost when the host restarts (the page
then signs with its device key, without asking the person). Reloading the page counts as
leaving: the page sends `end{left}` and does not keep the token.

## Picture in picture

When the page goes to the background during a session, the picture floats over other apps where
the browser does this by itself (Media Session `enterpictureinpicture`). When the page comes back
to the front, the picture returns to the page. There is no control for this; it only follows the
page going away and coming back. A floating picture keeps the page running, so the session
continues in the background, but input waits until the page is in front again. Where the browser
does not float a video, going to the background behaves as it would without this feature (B6).

## History (the back gesture)

Opening a session pushes a history entry that carries the session's id. Going back ends the
session whose id that entry carries, and only that session; a session opened in the meantime is
not affected.

## Cases

Every row has a test, and the test named in the row is where the behaviour is checked. Unit
tests are written `file::name` (Rust: `module::tests::name` under host/src; Go:
`internal/server/file::TestName`; browser: `tests/unit/connection.test.mjs::"name"`). e2e checks
are written `tests/e2e/area.test.mjs::"check"`.

### Starting (pressing a computer's picture)

| # | Situation | Behaviour; what the person sees | Test |
|---|---|---|---|
| A1 | The computer is offline | its picture shows it offline and cannot be pressed | e2e `manage.test.mjs::"a lift that never came holds the page at most a moment…"` (offline: no pressable picture) |
| A2 | The server is unreachable (the page knows its socket is down) | at once: "Can’t reach the server right now. Try again once it is back."; no session starts | e2e `takeover.test.mjs::"A2 pressing while the server is unreachable…"` |
| A3 | The computer's version is too old | the update notice; no session | e2e `takeover.test.mjs::"A3 a computer too old…"` |
| A4 | This device has no key / is not paired | the pairing flow | e2e `devices.test.mjs::"a device removed at the computer can no longer control it"` |
| A5 | This device's own session holds it | the `device` question; confirmed: force, the old one ends `taken` | e2e `takeover.test.mjs::"A5 the same device holding it asks the device question"`; `signal::registry::tests::admission_by_device` |
| A6 | Another device of this account holds it | the account question | e2e `takeover.test.mjs::"A6 another device of this account…"`; `computers_test.go::TestTheDeviceListSaysWhoHoldsAComputerAsEachBrowserSeesIt` |
| A8 | Someone is still connecting to it | the same device: replaced without asking; another device: A6 with "Connecting" | `signal::registry::tests::admission_by_device`; e2e `takeover.test.mjs::"A8 still connecting says so"` |
| A9 | The computer refuses (Mac permissions, device removed, signature, full) | the reason; no automatic retry | `signal::tests::offers_are_refused_with_the_reason_the_browser_shows`, `signal::tests::an_offer_from_a_device_paired_here_with_a_wrong_proof_is_auth_else_unpaired` |
| A10 | No network path (direct and relays fail) | `failed` within 30 s: "Didn’t connect" with **Retry** and a network hint | `connection.test.mjs::"A10 connect deadline…"`; `peer::state::tests::connecting_that_never_connects_ends_with_bye_failed` |
| A11 | The computer goes away while connecting | "The computer is offline for now"; back within the deadline: carries on; otherwise "Didn’t connect" | `connection.test.mjs::"A11 connecting: the computer back asks again…"`; `signal::tests::a_repeated_offer_goes_to_the_session_that_holds_the_computer`; `session::tests::each_epoch_is_answered_once_and_older_ones_are_not_this_negotiation` |
| A12 | The server restarts while connecting | no nonce came yet: the challenge again on the new server (the same nonce), and the session carries on; attached once one came; an attach the computer's status does not place is answered `bye{gone}`: "Didn’t connect" with **Retry** at once | e2e `lifecycle.test.mjs::"A12 a server restart while connecting…"`; `sessions_test.go::TestSessionsAChallengeForAComputerNotHeardFromSinceTheServerStartedWaitsForIt`, `TestSessionsAttachForAnUnknownSessionWaitsForTheHostsFirstStatus` |
| A13 | The person leaves (**Disconnect** / back) while connecting | ends at once; the computer is told | `connection.test.mjs::"B1 A13 ending here…"`; `peer::state::tests::a_bye_from_the_browser_ends_it_in_any_state` |

### Connected

| # | Situation | Behaviour; what the person sees | Test |
|---|---|---|---|
| B1 | **Disconnect** / back | `end{left}`; ends at once; back ends only its own session | `connection.test.mjs::"B1 A13 ending here…"`; e2e `takeover.test.mjs::"B1 going back ends the session"`, `"back after take-back keeps the new session"` |
| B2 | Tab closed / app killed | `end{left}` if it can; otherwise the host ends it when consent fails (about 30 s) | `peer::state::tests::no_recovery_by_the_deadline_ends_failed_on_the_channel_and_through_the_server` (the host's side; the page's `pagehide` is `leave()`, as B1) |
| B3 | The browser's network drops briefly | "Network interrupted; reconnecting"; input paused, keys released; restarted on the same peer connection | `connection.test.mjs::"B3 recovering…"`; e2e `lifecycle.test.mjs::"an ICE restart keeps the same session and its picture"` |
| B4 | The browser changes network (Wi-Fi to mobile data) | as B3; the restart gathers the new addresses | as B3 (the browser's restart gathers anew) |
| B5 | The browser's network is gone longer than 30 s | both ends `failed`: "The connection was lost" with **Reconnect** (resume token) | `connection.test.mjs::"B5 recover deadline…"`; `peer::state::tests::no_recovery_by_the_deadline_ends_failed_on_the_channel_and_through_the_server` |
| B6 | The phone locks / the page goes to the background | back within 30 s: carries on, `failed` included (restarted); later: as B5, and the computer is told at once | `connection.test.mjs::"B6 failed recovers…"`, `"B1 A13 ending here: through the server always…"`; `peer::state::tests::the_computer_ends_it_through_the_server_always_and_on_the_channel_while_open` |
| B19 | Back from the background after the session ended there | the computer was told: pressing again connects without a question | `connection.test.mjs::"B19 an end while the socket renews goes through the server…"`; `sessions_test.go::TestSessionsBelongToTheBrowserNotToOneSocket` |
| B20 | The socket renews while a session is connecting | the session carries on: its challenge again on the new socket, the same nonce, the offer answered | `connection.test.mjs::"B20 the signaling socket back…"`; `sessions_test.go::TestSessionsOpenOnAChallengeAndTheHostIsAskedAgainOnlyByItsBrowser`; `signal::registry::tests::asking_again_gives_the_same_nonce_until_it_is_used_or_expires` |
| B21 | The path goes while the computer still sees its other candidates (ICE checking) | the computer recovers with its deadline as on `disconnected` | `peer::state::tests::checking_after_connected_is_recovering_and_while_connecting_nothing` |
| B22 | A session is taken over while the old one's network backs up | the new session's picture and keys are untouched by the old one | `platform::local::tests::acts_of_a_session_not_holding_the_computer_are_ignored`, `platform::tests::every_method_is_one_call_and_back` (a session's acts carry it) |
| B23 | The page goes to the background where the browser floats a video | the picture floats; the session goes on; back in front, the picture is in the page | e2e `session.test.mjs::"picture in picture: floating, the session goes on"`, `"back in front, the picture is in the page again"`, `"no button for it in the toolbar"` |
| B7 | The computer's network drops briefly | "The computer is offline for now; waiting for it to come back"; on `presence{online}` the page restarts | `connection.test.mjs::"B7 attached or presence…"`; `sessions_test.go::TestSessionsTellPagesTheirComputerWentAwayAndCameBack` |
| B8 | The computer changes network | the restart rebinds its addresses; recovers | `peer::net::tests::a_restart_binds_new_addresses_and_closes_vanished_ones` |
| B9 | The computer sleeps | `failed` after 30 s: "The computer is offline"; **Reconnect** once it is online | `connection.test.mjs::"B7 attached or presence…"` (the cause `host`) and `"B5 recover deadline…"` |
| B10 | The computer or Deskway restarts / updates | its status no longer names the session: `bye{ended}` at once, "The computer restarted" with **Reconnect** | `sessions_test.go::TestSessionsARebootEndsTheOldHolderBeforeAnnouncingPresence`; `connection.test.mjs::"B10 bye ended through the server…"`; e2e `host-restart.test.mjs` kills and restarts an isolated Mac host after 4 s and 24 s |
| B11 | The Windows agent restarts (the service lives) | the session goes on; capture resumes | none here: not changed by the lifecycle (platform::Remote reconnects the agent pipe) |
| B12 | The computer ends it (signed out, its **Disconnect**, this device removed) | the reason; no automatic reconnect | `signal::tests::the_person_here_ends_the_session_with_its_reason`; e2e `lifecycle.test.mjs::"after a server restart, Disconnect at the computer ends the session once…"` |
| B13 | Another device takes over | "Taken over by {name}" with **Take back**; no automatic reconnect | `signal::tests::a_takeover_ends_the_holder_and_the_status_waits_for_its_goodbye`; `connection.test.mjs::"B13 D1 the computer ends it on the channel…"`; e2e `takeover.test.mjs::"B13 Take back takes the computer back…"` |
| B14 | The server restarts / is unreachable, any time | the session goes on; both ends register again when it is back; nothing shown | `signaling_test.go::TestAttachAfterAServerRestart`, `TestSocketLossSendsNoBye`; e2e `lifecycle.test.mjs::"a server restart during a session…"` |
| B15 | The server is unreachable and the network drops | no restart possible: recovers by itself or ends at the deadline | `connection.test.mjs::"B7 attached or presence…"` (cause `server`, no restart until attached) and `"B5 recover deadline…"` |
| B16 | The computer locks / a display is unplugged / an elevation prompt | their notes; the session goes on | e2e `touch.test.mjs::"the toolbar says when the remote computer is locked"`, `session.test.mjs` capture checks |
| B17 | A relay congests | frames are skipped to stay real time (session::desk::pace) | `session::desk::pace::tests::encoding_waits_while_the_queue_is_full_and_resumes_when_half_has_gone` |
| B18 | This browser is signed out elsewhere / removed / the password changed | back to sign-in; the session ends `left` | e2e `devices.test.mjs::"removing a browser signs it out at once"` (sign-in calls `leave()`) |

### Ended

| # | Reason | What the person can do | Test |
|---|---|---|---|
| C1 | `failed` / `gone` / `offline` | **Retry** / **Reconnect** (offline: once the computer is online) | `connection.test.mjs::"A12 bye gone…"` (gone ends it at once); e2e `host-restart.test.mjs` (**Reconnect**) |
| C2 | `taken` | **Take back** | e2e `takeover.test.mjs::"B13 Take back takes the computer back…"` |
| C3 | `ended` / `forbidden` / `permissions` | the reason only | e2e `lifecycle.test.mjs::"after a server restart, Disconnect at the computer…"` |
| C4 | `unpaired` / `auth` | pair where the computer can prompt, or sign in again and get approval from a trusted device | e2e `devices.test.mjs::"a device removed at the computer can no longer control it"` |
| C5 | `keyChanged` / `forged` (the page's own: the computer's identity not confirmed) | "Connection stopped", someone may be posing as it; add again only for `keyChanged` | `connection.test.mjs::"a bad signature on the current epoch ends the session as forged, told to the computer as auth"`; e2e `security.test.mjs` (changed key, forged signature) |

### Several devices and pages

| # | Situation | Behaviour | Test |
|---|---|---|---|
| D1 | Two pages of one browser | the second asks as A5; confirmed, the first sees "This device started a remote session in another window" | e2e `takeover.test.mjs::"D1 confirmed: the first tab sees…"`; `signal::tests::a_takeover_ends_the_holder_and_the_status_waits_for_its_goodbye` (`same`) |
| D2 | Two devices of one account | A6 | as A6 |

### AI sessions (the AI seat)

An AI client's `deskway` takes the AI seat, alongside whoever holds the desk seat. It never asks
a question, never takes over and is never taken over
([architecture.md](architecture.md), "Two seats side by side"). It is told about each of these
situations through a tool error that says what to do next (`cli/src/link.rs::refusal`).

| # | Situation | Behaviour | Test |
|---|---|---|---|
| E1 | AI never allowed here, or stopped at the computer | `ai_off`; nothing starts | `signal::tests::an_ai_client_takes_the_ai_seat_only_while_ai_is_on_and_beside_the_person`; e2e `ai.test.mjs::"before it is turned on, a call is refused as ai_off"` |
| E2 | Another AI client holds the AI seat | `busy`, naming it; never taken over (an offer with media or `force` is `forbidden`) | `signal::tests::an_ai_client_takes_the_ai_seat_only_while_ai_is_on_and_beside_the_person` |
| E3 | No tool call for 10 minutes | the seat is given up (`idle`); the next call connects again without asking anyone | `session::ai::tests::an_ai_session_with_no_tool_call_for_its_idle_time_gives_the_seat_up` |
| E4 | Stop at the computer, the switch turned off, the AI client removed | the AI seat ends (`ai_off`, `unpaired`); the person's session is untouched; the computer does not lock | `signal::tests::stop_switch_off_and_removal_end_the_ai_seat_and_only_it`; e2e `ai.test.mjs::"turned off, the next call is refused as ai_off"` |
| E5 | A person holds the desk seat | the AI client takes the AI seat beside them; each one's input is its own hand, and the AI's never stops the picture or touches the shared clipboard | `signal::tests::an_ai_client_takes_the_ai_seat_only_while_ai_is_on_and_beside_the_person`; `platform::local::tests::the_ai_clients_session_makes_input_and_nothing_else`; `input::tests::the_persons_sync_leaves_the_ai_clients_modifier_down` |
| E6 | The same AI client again (its `deskway` started again) | it takes its own place, even established | `signal::tests::an_ai_client_takes_the_ai_seat_only_while_ai_is_on_and_beside_the_person`; `signal::registry::tests::on_the_ai_seat_the_same_device_always_takes_its_place_and_no_other_does` |
| E7 | A lock screen, UAC or another secure desktop | the AI's input is dropped where it is injected; `act` tools say `secure_desktop` first | `input::tests::an_ai_clients_input_never_lands_on_a_secure_desktop`; `mcp::gate::tests::the_gate_lets_a_call_through_only_when_every_rule_holds` |
| E8 | The link ends with calls waiting | every one is answered with why and what to do | `cli::link::tests::every_way_a_link_ends_is_told_to_the_ai_client_with_what_to_do` |
