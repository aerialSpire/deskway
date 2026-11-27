# Deskway security design

This document is the security contract between the host, the browser and the server. It also
states what is protected and what is not. Any change to the code that affects it must update
this document at the same time.

## Threat model

| Attacker | Goal | What stops it |
|---|---|---|
| Someone who learns the account password | Control a computer | A new browser must be approved by an approved browser, and every session needs a signature by a device key the computer trusts. An account that turned device checks off has only the password and the approval |
| The signaling server, or whoever takes it over | Start a session, or sit in the middle of one | Hosts accept only offers signed by a trusted device key or a resume token they issued over an authenticated connection. Signatures bind both ends to their DTLS fingerprints. The server needs an existing browser's endorsement to add a trusted device. If remote MCP is enabled, it holds approved remote applications' AI device keys and can act as those devices |
| The network between any two parties | Read or change traffic | TLS for signaling, and DTLS/SRTP for media and input, with fingerprints bound by the signatures above |
| A bug in the network code (WebRTC, TURN, JSON) | Run code as SYSTEM | In service mode the network code runs as a low-privilege virtual account. Only a small SYSTEM agent touches the screen and input |
| A program running as a normal user on the host | Become SYSTEM through Deskway | Admin-only install folder, private data folder, authenticated pipes, a SYSTEM side that reads no user-controlled files, and process mitigations |
| Another account on the same server | Reach, watch or slow down another account's computers | Nothing is shared between accounts: a computer is listed, watched and reached only by the account that owns it, and trusts only that account's devices. The relay limits each account's traffic (below) |
| The admin, who made the account | Use the member's account | The admin can issue and redeem one-time links. Each redemption approves one browser. The member chooses their own password, and computers with device checks on enforce their own device trust |
| A thief with an unlocked phone | Control a computer | Nothing on the phone: a device key signs without a prompt. **My devices** on another device, or the computer itself, removes the phone; it is signed out of the website at once and computers stop trusting it when they receive the log |

The page's Content Security Policy limits scripts to the server's own files. It allows inline
styles because xterm.js creates stylesheets and sets cell styles at runtime. Inline styles
cannot execute scripts. Allowing these styles keeps the upstream emulator intact and lets
the terminal share the page's existing keyboard and layout. The client builds elements
through DOM APIs and parses only its own fixed SVG icons as markup. Names, files and
terminal output do not supply markup to the page.

The following are not covered:

- **The web page itself.** The server delivers the page's JavaScript. Someone who controls the
  server can change the page so that, the next time you open it, it uses your device key to sign
  offers for their sessions. Closing this gap completely would require an installed client that
  does not load its code from the server.
- **A browser's first connection to a computer.** A browser that has not paired with a computer
  directly (because it was added through the device log, or its storage was cleared) pins the
  host key from the first signed answer it receives. A server that controls that first answer
  could present a fake computer, for example a fake lock screen. Pairing that browser with the
  code removes this risk.
- **Signing a Windows computer in or out needs an administrator.** On Windows, Deskway runs as
  system services and can reach the lock and sign-in screens, so whoever signs the computer in to
  an account controls the whole computer. The panel signs in as the person (into their own
  folder), and then an elevated copy (`sign-in --from`) moves that sign-in to where the service
  expects it. The password never appears on the elevated command line, and if the person
  declines the elevation prompt, a computer that the server has just added is removed again.
  Signing out (`sign-out`, elevated) restarts the services, which then wait for a new sign-in,
  and keeps the computer (see "Signing out keeps the computer" below). The installer only
  installs.
- **The installer is not code-signed.** An attacker who can already write to the folder you run
  it from could replace the file before you approve the UAC prompt.
- **Pairing tries.** Anyone who can reach the computer through the account or the server can use
  up a pairing window's five tries. This is a nuisance, not a way in: the person opens a new
  window.
- **Windows passwords typed at the lock screen** reach the host as keystrokes over the encrypted
  channel. Anyone who can already control a session could log them, and that is the owner of a
  trusted device.

## Accounts and passwords

### Who makes accounts

The server operator issues a link using `deskway-server admin-link` as the service user.
Redeeming the first link creates the admin. The admin creates members and manages their links
on the website (`/api/members`). See [Accounts](accounts.md).

### One-time account links

The server generates 32 random bytes per link and encodes them in base64url. Its separate links
file stores the SHA-256 of the code, the intended account when known, the kind of link, the
issuer and the expiry. The code stays in the URL fragment until the setup page removes it from
the address bar. The page sends the hash to obtain the link's details and sends the code when
redeeming the link. Invalid codes count toward the same per-address limit as wrong passwords.

A new link replaces an outstanding link for the same account. A link works once. Member links
expire after seven days and admin setup and recovery links after one hour. An admin login link
expires after two minutes and leaves the account password intact. Redeeming a link approves the
browser that uses it and signs it in. Its registered key joins the device log unsigned. A computer that
already trusts devices requires its own approval or a signed endorsement from a trusted device
before accepting that key.

The server operator can also issue an admin enrollment token for a computer. It expires
after ten minutes, approves that computer atomically with enrollment, and leaves the
admin's password and browser keys alone.

### Local server certificates

A local-mode server creates a ten-year P-256 root CA with path length 1. During creation it
uses the root key to sign one P-256 intermediate CA and then discards it. The root key stays
in memory during creation. The intermediate has path length 0 and a single extended key
usage, `serverAuth`. Its critical X.509 name constraints permit the hub's exact
`<name>.local` name and `localhost`. The hub keeps advertising and issuing for the
normalized name stored in the intermediate when the CA was created. The remaining scope is
private addresses, including IPv4 private, loopback and link-local ranges and IPv6 loopback,
unique-local and link-local ranges. Verifiers that skip constraints on trust anchors still
encounter them on the intermediate.

The server keeps the two certificates and the intermediate key in `local-ca/` beside its
state file. The key has mode `0600` on Unix, and the server refuses to load a key with group
or other access. On Windows the key file has a protected DACL granting access to the server
account, SYSTEM and Administrators. The intermediate signs the TLS leaves. The server sends
the leaf and intermediate.

The Host and `deskway` CLI take the root's SHA-256 fingerprint from the `#ca=<64 hex digits>`
part of the address. They fetch `/local/ca.crt` over HTTPS without certificate verification
or credentials, compare the SHA-256 of the returned DER with the pin and save a matching
root as PEM. For that address, HTTP and signaling use native TLS verification of the
server name and chain with the saved root as the sole trust anchor. Public addresses use system
roots. The six-digit approval still authenticates the device joining the account.
A new device still needs approval on the real hub even if an impersonating hub captures the
account password.

A Host joining another hub uses the saved root only for its own connections, whether the
person selected a discovered hub or entered the address. Browsers on that computer use
the hub's HTTP certificate guide just like other devices on the network.

The Host can add browser trust for the hub it runs itself, using the CA in its local
service data. On Windows the panel asks for administrator consent when it first opens
its hub's setup or account link. The elevated program reads the local CA, verifies its
fingerprint against the panel's request, and saves the authorized DER in the protected
`server-trust/` directory. The SYSTEM agent uses that copy for `LocalMachine\Root`.
DeskwayNet holds the local service's state and CA under the machine-wide Deskway data
directory, accessible to SYSTEM, Administrators and DeskwayNet. On macOS the app and
bundled server run as the signed-in user. The app reads its local CA from the user's
private data directory and asks for SSL trust in the login keychain. Signing out removes
a root the Host added for its own hub. Windows uninstall also removes it.

Before installing, compare the fingerprint in the HTTP guide with the startup log on the hub
computer itself. Verify that the downloaded root certificate also has that fingerprint. On
iOS, check that the profile contains only the expected `Deskway <name>` certificate.

### An account is its id

An account's computers, browsers, account links and sessions refer to a random id that never
changes. The name the account signs in with is just one field of the account; it is unique, and
the account page can change it (after the account password is entered). Changing the name signs
nothing out and moves nothing.

### Account password

Signing in from a new browser requires the account password and approval from a browser that is
already approved. The account page changes the password, and asks for the current password
first. Computers are not affected, because they use their own tokens. When changing the
password, the person can also sign out every other browser.

### Limits per account

An account can have at most 10 computers (`maxComputersPerUser`, counting those waiting for
approval). One more is refused with 403, which the host's sign-in shows. At most 5 computers can
be waiting for approval at once, and likewise at most 5 browsers; beyond that, the oldest request
is dropped.

The TURN relay's credentials name the account: the username is REST-style `expiry:account`,
HMAC-signed with that relay's TURN secret. Each relay polices an account's relayed traffic, both
directions of all its allocations together, with a single token bucket at `-relay-account-mbps`
(default 30), and drops whatever exceeds it, so one account cannot crowd out another. The relay
offers only UDP relays, so no allocation can escape the limit.
TURN over TLS adds an outer TLS connection to the relay for reachability on port 443. The
relay still only forwards the peers' end-to-end encrypted DTLS and SRTP traffic and cannot
read its contents.

### Relay nodes

An additional relay node sees the account ID in the TURN username, the network endpoints
and traffic metadata, and the encrypted WebRTC packets. It cannot read the session content.
Each node's secret can mint TURN credentials for that node and authenticate its reports to
the main server. It grants no access to accounts or signaling and cannot mint credentials
for the main relay or other nodes.

A computer learns the online relays' STUN addresses and can influence relay choice only for its own sessions.

Only the admin can create a join token or manage nodes. The node exchanges the token once
over HTTPS and stores its own secret in a private file with mode `0600` on Unix. The main
server verifies the HMAC over the node ID and timestamp on each report and checks that the
timestamp is recent and newer than the node's last accepted timestamp in memory. Removing a
node erases its secret from the main store and stops new ICE offers for it. Already issued
credentials can work until they expire, and stopping the node closes active allocations.

## Device keys

### The key itself

- Each browser generates its own ECDSA P-256 key with WebCrypto, once and without a prompt. The
  private key is stored in IndexedDB as a non-extractable key. The public key and a random id
  (`cred`) are kept in localStorage, and the browser sends both to the server
  (`POST /api/login/device`). A waiting browser sends its key for comparison with the
  approver. A signed-in browser registers its key before opening the computers and their
  approval requests.
- Every signature is plain ECDSA with SHA-256 over the 32-byte challenge, encoded as base64url
  r||s, with no `ad` and no `cd`. Computers also verify log entries and vouches that carry a
  WebAuthn passkey assertion (`ad`, `cd`, `sig`) the way a relying party would, but browsers do
  not create such assertions.
- A connection offer carries `{k:"dk", nonce, cred, sig}`.
- Proofs for approve, remove, rename, vouch and policy are `{ts, by, sig}`.
- A rename (`deskway-rename-v1`) changes only the name a computer shows for a device it already
  trusts, and only when the rename is signed by a device it trusts. The server keeps the newest
  rename for each device. Names are displayed, never trusted: a new device is identified by its
  approval code.

### AI client keys

`deskway` protects its P-256 device key with DPAPI on Windows and stores it in the login
keychain on macOS. On Linux, it stores the unsealed key in `device.json` (mode `0600`)
inside its state directory (mode `0700`). Linux protects the key through file permissions
alone. Anything running as that user, and root, can use it (R2).

### First use: adopting the account's devices

A computer that has never trusted any device accepts the account's device list as the server
sends it, once, and writes an `adopted` marker. The computer has just been signed in, at the
computer itself, with the account password or its hub's one-time enrollment link. This is a trust-on-first-use moment, like SSH
seeing a host key for the first time. After that, only signed changes count. **Remove all** at
the computer does not cause the list to be adopted again.

As a result, new computers need no QR code. The QR code and **Request control** are for adding a
device at a computer that already trusts other devices.

### Signing out keeps the computer

Signing out at a computer only stops it using the account. The server keeps the computer
(offline), and its folder keeps the sign-in aside (`signed-out.json`) together with its host key,
the devices it trusts and its AI switch.

Signing in again names the computer kept there (`back`: its id, which is not secret). When the
account password is right and the account owns that computer, the server answers with the same
id and no token, and the computer takes back its own sign-in: still approved, trusting what it
trusted before. The password and an id give nothing that a new computer would not get; the token
never leaves the computer (on Windows it stays in the service's folder, and the panel only learns
the id).

Any other answer means a new computer: another account, another server, or this computer removed
from the account in the meantime. Then everything that tied it to the earlier account is removed
first (its sign-in, host key, trusted and blocked devices, the `adopted` marker, pairing state, AI
switch, and the record of what AI clients did, which the next account must not be able to read),
so it adopts the new account's devices on first use. Only removing a computer from the account
(**My devices**) ends it: its token stops working.

### Approving a computer

A computer waiting for approval shows six digits derived from its id
(`SHA-256("deskway-computer-code-v1\n" + id)`, in the same way as for a device's key),
and the approval on the website shows the same digits. Someone who has the password can sign in a
computer of their own under the same name; the digits tell the person's own computer apart from
it. A computer says at enrollment that it shows the digits (`shows_code`); for an older one that
does not, the approval shows none.

### Device checks can be turned off

A trusted device can turn device checks off. The computer then accepts the account password, as
checked by the server, without a device signature. The policy entry is
`{op:"policy", keys:false, ts, by, sig}`, signed over
`SHA-256("deskway-policy-v1\n{0|1}\n{ts}")`.

Device checks are on by default. The switch is on **My devices**, and the corresponding checkbox
is in the approve and add dialogs. The server cannot turn checks off on its own, because the
entry must be signed by a device the computer trusts. With checks off, the account password
alone reaches a shell on Linux or an unlocked macOS or Windows workspace. The picture and the terminal inherit the same
policy. The host treats the server as the authority for an unsigned browser offer under
that policy.

### Cost

Without a face or fingerprint step, an unlocked phone in someone else's hands can control the
computers until it is removed.

Clearing browser data, or Safari deleting a site's data after 7 days without use, means the
device has to sign in and be approved again. Adding the site to the home screen avoids the 7-day
rule. When the key is created, the browser is asked to keep the site's storage
(`navigator.storage.persist()`), and **My devices** shows a note on any device whose browser
refused.

## Devices: who may control the computers

The account has a single list of devices: the browsers on phones, tablets and computers used as
controllers, each with its own device key. The same list determines who can sign in to the
website and whom the computers let in, so a person only ever adds a device once.

### Adding a device

There are two ways to add a device.

At a computer, the panel's **Add a device** shows a QR code for
`https://<server>/#pair=<computer id>.<code>`. The code has 80 random bits and is valid for 10
minutes, for one use, with at most 5 wrong tries. Because it comes after the `#`, it never
reaches the server, and the page removes it from the address bar immediately. Once signed in, the
phone sends the pairing message below, signed with its key. A device without a camera sends a
request instead: it sends `pairask` with its key, the panel shows the device's name and six
digits derived from its key, and the person at the computer clicks **Allow** if the device shows
the same digits.

From another device, a new browser signs in with the password, generates its key and shows six
digits derived from it. A device already in the list shows the same request with the same
digits. When someone presses **Approve** there, that device signs an endorsement with its key.
The server records the endorsement in the account's device log, and every computer that trusts
the approving device then trusts the new one as well. No visit to each computer is needed.

Headless computers use this signed approval path. A browser signed in through a link can
request signed approval by signing out from **My devices** and signing in again with its
password. Approve the request on a device the computer already trusts.

### The pairing message (QR path)

```json
{ "type": "pair", "session": "...", "device": "<computer id>",
  "auth": { "cred": "...", "alg": -7, "key": "<SPKI base64>", "name": "...", "mac": "<base64>",
            "vouch": { "list": [ { "cred", "alg", "key", "name" } ], "sig": "..." } } }
```

- `mac = HMAC-SHA256(code, "deskway-pair-v1\n{host}\n{cred}\n{alg}\n{key}\n{name}")`. The code
  is upper-case and contains only letters and digits; O is read as 0, and I or L as 1.
- The reply is `paired` with `hk` and a MAC over `"deskway-paired-v1\n{host}\n{cred}\n{hk}"`.
- The optional `vouch` lists the account's other devices. It is signed by the new device over
  `SHA-256("deskway-vouch-v1\n{host}\n" + for each device sorted by cred:
  "{cred}\n{alg}\n{key}\n")`. The computer trusts those devices too, so whichever device is added
  first at a computer brings the rest along.
- `pairask` carries the same `auth` without `mac`. The reply is `paired` with `hk` once the
  person allows it, or `bye` with the error `denied`.
- The six digits are derived from `SHA-256("deskway-code-v1\n" + key)`: the first four bytes,
  big-endian, modulo 10^6, zero-padded.

### The device log

The server sends the device log to each computer (`{"type":"devlog","auth":[entries]}`) when it
connects and after every change.

- An add entry is `{op:"add", cred, alg, key, name, ts, by, sig}`, signed by `by` over
  `SHA-256("deskway-endorse-v1\n{cred}\n{alg}\n{key}\n{name}\n{ts}")`.
- A remove entry is `{op:"remove", cred, ts, by, sig}`, signed by `by` over
  `SHA-256("deskway-remove-v1\n{cred}\n{ts}")`.
- An AI client's `deskway` is also a device. Its add entry has `role:"ai"` and is signed over
  `deskway-endorse-v2`, so the role is part of the signed string (its six digits come from
  `deskway-code-v2`). A server that strips or adds the role breaks the signature. An AI client:
  - signs for nobody: add, remove and policy entries, vouches and AI switches count only when
    they come from a browser;
  - is never adopted, vouched for or added at a computer (by QR code or `pairask`): only a
    browser's endorsement lets it in, and the server accepts no pairing message from an AI
    client's login;
  - may use, with its login, only `GET /api/me`, `GET /api/devices`, `GET /api/ai/clients`, `POST /api/logout` and the
    signaling socket, and
    on the socket only what a session needs. Its offers are always signed with its device key,
    so a computer with device checks off still recognizes it as an AI client. The server
    enforces both rules;
  - a new client requests access by account name while a browser of the account has opened
    AI sign-ins, for 10 minutes after the prompt on **Add an
    AI client** is copied. At any other time the request is refused, and the refusal is the same
    for an unknown name. The client then waits for a browser's approval like any new device. An
    approved client signs in again with its device key over `deskway-ai-login-v1`, with a time
    within 5 minutes of the server's, and each signature is accepted only once.
- **The AI switch.** AI clients can control a computer only while a browser of the account allows
  it for that computer, with `{host, ai, admin, ts, by, sig}` signed over `deskway-ai-v1`. The
  server sends the switch together with the log (`devlog.ai`, the latest switch per computer). It
  is kept separate from the log so that it never pushes entries out of it. The computer accepts a
  switch only from a browser it trusts and only if it is newer than every switch it has seen, and
  stores it (`ai.json`). So the server can hold a switch back, but cannot make a computer accept
  an older one. A switch it refuses because no device it trusts signed it is reported
  (`status.ai_refused`, its time, and `AiView.refused` on the website, which says what to do) and
  accepted as soon as its signer is trusted, by being added at the computer or endorsed in the
  log. Stop, pressed at the computer, ends AI control there until a
  browser signs a newer switch. It does not depend on the server, which could hold back an
  "off". On Windows, `ai.json` is in the service's protected folder. On macOS and Linux it is
  in the host user's state directory. On Linux that user can run `deskway-host stop-ai`, which
  signals the running host or applies the stop to the saved switch while the host is down.

A computer works out whom it trusts as follows:

1. It starts from the devices added at the computer (by QR code or `pairask`, and the devices
   they vouched for).
2. It walks the log in order. An entry counts only if its signer is trusted at that point and the
   signature verifies.
3. A signed removal also removes a device that was added at the computer, unless that device was
   added there again after the removal's time.

Entries signed by nobody (with `by` empty) are ignored. They come from a device that signed in
through an account link, or that sent its key to the server only after approval.

What the server can and cannot do:

- It can hide entries, or the whole log. That never adds anyone, and devices added at a computer
  remain trusted.
- It cannot forge or alter an entry.
- It can hold back a removal, which leaves a removed device trusted. Removing a lost device at a
  computer, or pressing **Remove all** there, does not depend on the server.

### Removing a device

**Remove** on **My devices** signs a removal with the current device's key. The removed device
is signed out of the website immediately, and each computer stops trusting it as soon as it
receives the log.

## AI clients at the computer

`deskway gateway` serves MCP over HTTP. Every local process can reach its loopback TCP listener.
Its `Host` and `Origin` checks protect that listener against browser DNS rebinding. On Linux and
macOS, `--listen unix:PATH` serves HTTP on a Unix socket in a private directory. Filesystem
permissions restrict access to processes running as the gateway user. Those permissions are the
access control for this listener and there are no `Host` or `Origin` checks. Windows offers the
TCP listener.

Anyone who can reach either listener can act as the approved AI device in that process's Deskway
profile. Put an HTTPS server that authenticates and authorizes callers in front of the gateway for
remote access. The computer applies its AI switch and workspace rules and records actions under the
profile's approved device identity. See [Serve MCP over HTTP](ai-clients.md#serve-mcp-over-http).

A local process that can reach the listener can make the HTTP library allocate memory for an
unread body even if the gateway rejects the request. The fronting server must cap request
bodies at 2 MiB before forwarding them.

### Remote applications through the Deskway server

With remote MCP enabled, the server stores a separate AI device key for each approved
application and can act as that device. Treat the server, its state directory and backups as
having the same AI access. The browser signs the existing AI device endorsement. The host
still verifies the device, applies its AI and administrator switches, and records the
device's actions. Browser and local AI client keys stay on their own devices.

Authorization errors are shown by Deskway. Approve and Cancel on the approval page send the
browser to the callback host shown there. For a repeat approval, the endorsement must be
strictly newer than the application's last accepted endorsement and within the five-minute
clock-skew allowance.

The application receives an OAuth credential for this server's MCP endpoint. It can list the
computers available to the approved AI client and see the results of its MCP calls, including
any data a permitted tool returns. The application's grant provides no Deskway account
password, browser session, or device key.

Access tokens last ten minutes. An issued refresh token rotates on use within its original
30-day lifetime. Each sign-in has its own family, and reuse invalidates only that family's
credentials. The grant holds up to eight families. A new approval drops the oldest unredeemed
code at capacity and is refused when all eight families are active. Removing the remote
application in **My devices** invalidates all its credentials, stops its gateway and deletes
its server-side profile. The signed removal also tells computers to stop trusting the key.
Signing out of the website leaves approved applications connected.

Issuing or redeeming a password reset link leaves approved local AI clients and remote
applications connected and able to start new signaling connections.

This section describes what an AI client can do once a computer lets it in. How it works is
described in [architecture.md](architecture.md), "AI control".

- **A single checkpoint.** Every tool call passes through `mcp::gate`, which checks that AI is
  on, that the tool is known and its arguments match `proto/tools.json`, that the workspace
  user is available for tools that need one, and that no input is sent to a view-only
  computer. It also writes a start record and an end record to the audit log (`audit.rs`: JSON
  lines, append-only, bounded; in the service's folder on Windows). A secure desktop (the lock
  screen, UAC) is checked again where input is injected, and the AI's input is dropped there,
  regardless of what the gate decided a moment earlier.
- **As the person, never as SYSTEM** (Windows). The network service does not act as the person.
  The AI client's workspace is the **user worker**, `deskway-host worker`, which the `Deskway`
  service starts as the person signed in at the console (with `WTSQueryUserToken` and their
  environment) whenever someone is signed in, inside the service's kill-on-close job. The worker
  receives a new 32-byte key on its standard input, which is the only handle it inherits. The
  service writes the same key to its own protected folder (`service\worker.key`), where the
  network service reads it. The two meet on `\\.\pipe\deskway-worker`, which SYSTEM and the
  network service may serve and interactive users may only open, and each proves knowledge of
  the key to the other (`handshake_*`, as on the agent's pipe). No other process of the person's
  can therefore impersonate either end. Commands and file operations then run with exactly the
  person's rights.
- **As the host user** (Linux and macOS). An AI client that may run commands has the access
  of the user running Deskway Host, including that user's files and the host's own state.
  On Linux, the service runs as the ordinary user named in the unit. Use a dedicated
  account and grant it sudo rights only if approved AI clients should have those rights.
  See [Install Deskway Host on Linux](install-host-linux.md).
- **Administrator rights only when allowed, and only the person's own.** The AI switch has a
  second part, administrator rights, which is signed together with it and is off unless a
  browser turns it on. Linux refuses `elevated` with `elevation_unavailable`. Windows and
  macOS refuse it with `elevation_off` while the switch is off. On Windows the `Deskway` service
  starts the **admin worker** only while the switch stored next to the trusted devices allows it
  and the signed-in person is an administrator. The admin worker
  runs with the person's elevated token (UAC's linked token) and has its own key and pipe
  (`\\.\pipe\deskway-worker-admin`). Its programs are numbered separately, so a program's id shows
  which worker owns it. Turning the switch off, pressing Stop, or the person signing
  out ends that worker (but not the programs it started). A person who is not an administrator
  gets no rights beyond their own (`elevation_unavailable`). On a Mac, each elevated `run` goes
  through the system's own administrator prompt, which the person at the Mac answers every time,
  and a terminal is never elevated.
- **Programs outlive the AI session.** A program an AI client starts is an ordinary program of the
  workspace user. Ending the session, idling out or pressing Stop leaves these programs
  running. On Windows, programs can leave the service's job (`CREATE_BREAKAWAY_FROM_JOB`),
  letting them run past a worker restart. Use `proc_kill` to end a running program.

The following risks remain and are accepted:

- **R1 Prompt injection.** Anything the AI client reads (a web page, a file, a window) can steer
  it, and nothing here can detect that. The person's safeguards are the switch, Stop,
  and the indicator.
- **R2 The AI's machine.** Malware on the machine an AI client runs on can use its device key just
  as the AI client would.
- **R3 A browser on the same computer.** If Deskway is signed in in a browser on the computer the
  AI controls, the AI can operate that browser, for example to turn the switch on for another
  computer or to approve a device. The page marks the computer it is running on
  (**This computer**) but cannot prevent this.
- **R4 The network service.** A compromised `DeskwayNet` could already inject input through the
  agent; through the worker it can also run programs as the person. It never gets more than the
  person's rights.
- **R5 Mac permissions.** On a Mac, programs an AI client starts inherit Deskway's own Screen
  Recording and Accessibility permissions.
- **R6 Mac stop.** On a Mac, `ai.json` (the last switch seen, and the Stop state) is in the
  signed-in person's own folder, so any program of theirs, including one started by an AI, can
  change it.
- **R7 The indicator.** The strip that shows an AI client is working is drawn by the person's
  panel process, which the AI, running as the person, can close.
- **R8 Holding back an off.** The server can hold back a switch that was turned off;
  Stop at the computer does not depend on the server.
- **R9 Vouching.** A device that is vouched for or adopted takes its role from the server's list.
  The computer drops entries with `role:"ai"` there, but a server that strips the role can pass
  an AI client off as a browser in a vouch.

## Starting a session

An approved browser can open Terminal or Files on Linux, macOS and Windows under the same
admission and takeover rules as a picture session. The signed SDP selects the session kind.
These actions require control rights and are unavailable on a view-only host. On Linux and
macOS, the workspace belongs to the account running Deskway Host. On Windows, it belongs to
the ordinary worker of the console user. The shell starts in the user's home or profile directory
with ordinary rights, regardless of the AI administrator switch. On macOS, Terminal and Files
work without Screen Recording or Accessibility. The shell runs with the host app's privacy
permissions, which can differ from those of a terminal app the user opens directly.
The shell and its programs survive the session that opened them. Any browser admitted to the
desk seat can return to the shell and receive its most recent 256 KiB of output, kept in memory.
The shell ends on exit, when an attached browser explicitly closes it, after 24 hours detached,
when the host stops, or when the Windows sign-in ends or changes users. Locking Windows or
macOS blocks new data sessions and ends any attached one while a kept shell continues to run.
This also applies on the Windows secure desktop and when fast user switching takes the Mac
app's session off the active console. The host reads these states independently of capture.
It checks access before queueing each terminal output frame, including replay, and stops a
directory scan when its visitor detects revocation. The Windows worker checks access before
reading attached output and before resuming a new PowerShell process. A synchronous ConPTY
write already in progress at lock may still deliver bytes typed before the lock. A Windows
sign-out or user switch clears the old workspace. Logging out of macOS stops Deskway Host.
The host logs when the terminal opens, is left, is returned to or closes, including the
device and the reason where applicable. These events go to the systemd journal on Linux and
the Deskway network service log on Windows. Terminal input and output stay out of the logs.

Files uses the ordinary workspace user’s privileges across the filesystem. An
admitted browser can see directory entries, including hidden names, wherever this account
can read them, and can create folders, rename entries and remove files, links or empty
directories wherever this account can make those changes. A trusted browser can download
regular file contents and upload files as the host user. Uploads use a private staged
file, verified with SHA-256, and a no-replace rename unless the person approves replacing a
regular file. Under the desk-seat guard the host checks the staged path's identity and the
approved destination's identity, size and modification time without following links.
macOS publishes with `rename` for an approved replacement and `renamex_np` with `RENAME_EXCL`
otherwise. APFS and HFS+ resolve the destination's spelling according to the volume's case rules.
The host uses the stored entry name to recognize aliases of live partials.
Windows publishes with `MoveFileExW` in the same directory, setting `MOVEFILE_REPLACE_EXISTING`
only for an approved replacement. This is a single filesystem rename with no copy fallback.
Data is verified and synced before the move. Windows file sharing and filesystem restrictions
can cause the rename to fail. The staged file's open handle carries delete access and shares
deletion. Windows can unlink the partial while other compatible handles are still open on
volumes that support POSIX deletion. Other volumes can mark it for deletion on last close.
Another local process can change an entry between the metadata checks and the rename.
This access includes configuration and credentials owned
by the host account. Files performs filesystem operations without starting a shell or
running a file. The host output records the device, operation, path and outcome on one
line per browse operation. It records the device, direction, path, size, replacement and outcome
with any failure reason on one line per transfer, including resumed transfers. The same logs
carry these lines. File contents stay out of the logs.

The host checks transfer ownership at desk admission using the verified browser credential
and the page identity carried by its authenticated resume token, and checks the current seat
before each transfer filesystem step. A takeover discards the previous device's staged upload.
An upload may return while its peer session remains live or recovering, or after a connection
failure or a Windows or macOS lock while the page's existing resume token is valid (up to eight hours from device proof).
The host locks that staged file against browse rename, delete and age sweeping during this
lifetime. It unlinks the partial when the record is dropped. After host restart the sweep uses
the modification age of remnants.

Before sending its offer, the browser asks the host for a nonce (`challenge`). The host answers
with 16 random bytes (`nonce`), valid for one offer on that session and for 60 seconds. Every
proof covers that nonce instead of a timestamp, so no clock needs to be correct, and a captured
offer cannot be replayed: its nonce is used up whether or not the offer was accepted.

The offer carries `auth` in one of two forms.

The device key:

```json
{ "k": "dk", "nonce": "<base64url>", "cred": "<base64url>", "sig": "<r||s, base64url>" }
```

- `sig` is ECDSA P-256 over `SHA-256("deskway-connect-v3\n{host}\n{session}\n{nonce}\n{force}\n{hex(SHA-256(offer sdp))}")`,
  where `force` is `1` for taking over a running session and `0` otherwise, so the server cannot
  turn an ordinary offer into a takeover.
- The host checks that the nonce is the one it issued for this session, unused and not expired,
  and verifies the signature against a device it trusts.

A resume token, which needs no device key:

```json
{ "k": "rs", "nonce": "<base64url>", "id": "<token id>", "mac": "<base64>" }
```

- `mac = HMAC-SHA256(token key, "deskway-resume-v3\n{host}\n{session}\n{nonce}\n{force}\n{hex(SHA-256(offer sdp))}")`.
- The host sends a fresh token on every authenticated session, on the control channel after its
  hello: `{"t":"resume","id":"...","k":"<base64 32 bytes>"}`.
- Tokens exist only in the host's memory and the page's memory. They expire 8 hours after the
  last device-key check (renewed tokens keep that deadline), they are lost when the host
  restarts, and they are invalidated when their device is removed. A session that ends with
  `taken` (someone took over) or `ended` (**Disconnect** pressed at the computer) revokes its
  controller's tokens, so that browser does not reconnect on its own.

An ICE restart (`restart`) needs no new proof. The host accepts it only when its DTLS fingerprint
equals the fingerprint in the session's authenticated offer, and signs its answer as described
below.

A browser that has never created a device key sends its offer without `auth`. The host answers
`unpaired` when device checks are on, and the page opens pairing instead.

The host refuses with these `bye` errors:

- `unpaired`: there are no paired devices, or this device is not paired.
- `auth`: the nonce, signature or token check failed. A browser that used a resume token drops it
  and tries again with the device key.

The host signs its answer, which carries the host's DTLS fingerprint:

```json
{ "type": "answer", "session": "...", "sdp": "...", "auth": { "sig": "<r||s, 64 bytes, base64>", "hk": "<SPKI base64>" } }
```

- The signed message is `"deskway-answer-v1\n{host}\n{session}\n{hex(SHA-256(offer sdp))}\n{hex(SHA-256(answer sdp))}"`.
- The signature is ECDSA P-256 with SHA-256, so WebCrypto can verify it directly.
- The browser compares `hk` with the key it pinned. If they differ, it stops and shows a warning.
  If no key is pinned yet (a device that never paired with this computer), it pins `hk` and
  tells the person.

## Lock screen state

Whichever process captures the screen reports the input desktop on the reliable channel at the
start of a session and whenever it changes: `{"t":"desk","v":"default"|"winlogon"|"other"}`.
`winlogon` covers the lock screen and the sign-in screen, and `other` covers UAC prompts and
other secure desktops. The browser shows **The remote computer is locked** whenever the value is
not `default`.

On macOS, a host with the Screen Recording and Accessibility permissions can capture the lock
screen of its signed-in session and send input to it. macOS verifies the unlock password.
Logging out of macOS ends the host together with the user session.

## Service mode on Windows (lock screen and login screen)

The service mode consists of three processes, all from one binary:

| Process | Account | Does | Does not |
|---|---|---|---|
| `Deskway` service (`service`) | LocalSystem, session 0 | Starts the agent in the console session and restarts it when the session changes; writes the pipe key | Network, parsing, reading user files |
| Agent (`agent`) | SYSTEM token, console session; privileges stripped except SeChangeNotify and SeImpersonate | Capture, encode, input, file transfers (as the signed-in user), input desktop tracking (Default, Winlogon, UAC), and applying administrator-authorized browser trust for a local server | Network |
| `DeskwayNet` service (`net`) | `NT SERVICE\DeskwayNet` (a virtual account): SeChangeNotify, SeCreateGlobal and SeImpersonate only | Signaling, WebRTC, TURN, device key checks, pairing | Run as SYSTEM, touch the screen |

The user worker serves browser Terminal and Files through its authenticated pipe. The admin
worker serves elevated AI requests, as described under
[AI clients at the computer](#ai-clients-at-the-computer).

The desktop panel and the tray icon run as the signed-in user and talk to `DeskwayNet` over
`\\.\pipe\deskway-ui`. The pipe accepts only the panel's requests (`PanelRequest` in
`host/src/settings.rs`):

- show the status;
- change a panel setting: auto-lock, clipboard sharing, language, keep awake, start with Windows;
- disconnect;
- start or stop showing the QR code for adding a device;
- answer a device's request to be let in;
- remove allowed devices;
- stop AI control.

The pipe's DACL gives SYSTEM and DeskwayNet full control. Interactive users can read and write
but cannot create pipe instances. The panel checks that the process serving the pipe is the one
that the service manager reports as running DeskwayNet.

Adding a device is something the person sitting at the computer does, so it does not require an
administrator. The trade-off is that a program running as that person could also open a QR code
and add its own device. Such a program already controls the person's desktop. The device it adds
would also reach the lock screen and UAC prompts, which Microsoft does not treat as a security
boundary against an administrator's own programs.

Some actions do require an administrator. They run `deskway-host` elevated, which shows a UAC
prompt, and they change files that only administrators can write:

- `install` and `uninstall` (through the installer window);
- `unpair` and **Remove all**.

The per-user panel handles signing in and out. DeskwayNet enrolls its own Host when its
local service has an admin. The panel uses one-time admin links to open that local service
in a browser. Authorizing the hub's root for Windows browsers requires administrator consent.
Stopping with **Also delete the local service data** also requires administrator consent.
The elevated command saves the off switch and clears the hub's active enrollment in
`host.json` before moving the data folder aside. It restores the previous configuration if
the move fails. After the move it removes the hub's browser trust and erases the renamed
folder. The next service start retries the deletion of any leftover renamed folders.

DeskwayNet is not restricted further for this reason: Windows' TLS and DTLS implementation
(SChannel) fails with "no credentials available" when the service runs with a write-restricted
SID or without SeImpersonatePrivilege. That privilege is the one that "potato" exploits use to
turn code execution inside a service into SYSTEM. A remote code execution bug in the network
code is therefore still contained to a non-SYSTEM account, but the containment is weaker than
intended. Moving TLS and DTLS to a library that does not use SChannel would make it possible to
drop the privilege.

### Files

- Only administrators can write the Host and its bundled server in `C:\Program Files\Deskway\`.
- Both folders are located through the system's known-folder paths, never through environment
  variables. The installer creates the data folders with their final permissions. If either
  folder already exists and is owned by anyone other than SYSTEM or Administrators (for example,
  a normal user created it in advance and put files in it), the installer moves it aside instead
  of adopting it. No sign-in is carried over: the computer is signed in from the panel afterwards
  (see "Signing a Windows computer in or out" above). The SYSTEM service checks the owners of
  both folders again at every start.
- `C:\ProgramData\Deskway\` has a protected DACL: SYSTEM and Administrators have full control,
  `NT SERVICE\DeskwayNet` can modify, and users have no access. It holds:
  - the device token;
  - the host key;
  - the trusted device keys;
  - the pairing window.
- `C:\ProgramData\Deskway\service\` gives SYSTEM and Administrators full control and `DeskwayNet`
  read-only access. It holds the pipe key. This is the only place a SYSTEM process writes to, so
  the network account cannot plant a link there.

### Pipe between agent and network service

- Name: `\\.\pipe\deskway-agent`.
- DeskwayNet creates it with:
  - `FILE_FLAG_FIRST_PIPE_INSTANCE`;
  - `PIPE_REJECT_REMOTE_CLIENTS`;
  - a DACL that allows only SYSTEM and DeskwayNet.
- Process ids can be forged, so neither side trusts them. Before anything else crosses the pipe,
  both sides prove that they know the pipe key with an HMAC challenge:
  1. The agent sends a nonce.
  2. The server answers with its own nonce and `HMAC(key, "deskway-ipc-server" | both nonces)`.
  3. The agent verifies it, then sends `HMAC(key, "deskway-ipc-client" | both nonces)`.
  4. A squatter that does not know the key learns nothing, because the agent sends no input or
     screen data until step 2 has been verified.
- After the handshake, the pipe carries length-prefixed frames of at most 8 MB, parsed strictly.

### Agent hardening

The agent sets:

- `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`;
- extension points disabled;
- strict handle checks;
- image load policy: no remote images, no low-label images, prefer System32;
- redirection guard;
- no child processes.

The SYSTEM service and the administrator commands set the same mitigations. The agent does not
block dynamic code or require Microsoft-signed binaries, because GPU drivers and hardware
encoders may need both.

The agent:

- reads no file as SYSTEM except the pipe key;
- writes nothing as SYSTEM except its own log, `service\agent.log`, in the folder that only SYSTEM
  and administrators can write;
- puts received images and files, and the controller's clipboard placeholders, on the clipboard
  only while the Default desktop is active, never at the lock screen;
- reads the clipboard only while sharing is on and a session is running, never on the lock
  screen or a secure prompt (see "Shared clipboard").

The network service reaches the agent only through a generic platform proxy
(`platform::Remote`). One message type carries every call and one carries every event, so the
pipe protocol has no per-feature messages.

### Images and files

Images and files travel on their own data channel, `file`, inside the same encrypted WebRTC
connection, so the server never sees them. The flow is implemented in `host/src/transfer/`.

- **Who touches the disk.** Every file the controller sends is written by the agent while
  impersonating the signed-in console user (using a token taken from that user's Explorer). This
  is why the agent keeps SeImpersonate. A junction planted by the user cannot make SYSTEM write
  somewhere else. When nobody is signed in, nothing is read or written.
- **On macOS** the host already runs as the signed-in user. It writes incoming files to that
  user's Downloads folder, under `Deskway`. macOS may require permission to access Downloads or
  other files. The host checks the console session before transferring.
- **Where files land.** Files are written only to the signed-in user's `Downloads\Deskway`. Names
  sent by the controller are reduced to a single file name: no folders, no `..`, no device names
  (`CON`, `NUL`, `COM1`…) and no characters that Windows rejects. An existing file is never
  replaced; a new name such as `a (2).txt` is used instead.
- **Nothing runs.** Files are saved and put on the clipboard as files (as if they had been copied
  in Explorer). The agent never opens or runs them.
- **Limits.** 25 MB per file, 10 files and 50 MB per transfer, and 20 MB per image. Sizes are
  declared first, and anything that exceeds them stops the transfer. A cancelled or failed
  transfer deletes what it has written.
- **Locked computer.** Nothing is sent or received while the lock screen or a secure prompt is
  shown; a transfer in progress stops and its files are deleted.
- **From the computer.** Only shared-clipboard data that the controller requested is sent (see
  below).
- **Clipboard answers.** A `put` with `gen` answers a paste that is waiting on this computer. Its
  bytes are held in memory and given only to that paste; nothing is written to disk. View-only
  hosts accept these too (they only answer a paste made at the computer) and refuse every other
  `put`.
- **Indicator.** While files arrive, and afterwards, the on-screen indicator says so.
  **Open folder** opens only the folder the service named.
- **View-only hosts** answer `unsupported` and accept nothing.

### Shared clipboard

The clipboard follows the RDP CLIPRDR model (`host/src/clipboard/`): a copy announces only its
formats, and data moves only when something pastes.

- **Who switches it.** Sharing is controlled by the computer's setting `clip_share`, which is off
  by default. The controlling browser switches it (`clipshare`, from its toolbar), because the
  computer is the one being controlled, and what it does is decided and observed from the
  controlling side. The network service saves the setting, and the session sees the new state
  immediately.
- **What the host reads, and when.** The host reads the clipboard only while sharing is on and a
  session is running, and only on the console session's Default desktop, never on the lock
  screen or a secure prompt. On a copy it reads the list of formats, the text itself when it is
  at most 48 KB (that text travels with the announcement), the size of HTML and pictures, and the
  names and sizes of copied files (as the signed-in user). Pictures, larger text and file
  contents are read only when the controller requests that generation (**Get it here**); files
  are read as the signed-in user and up to 25 MB each. A request for an older generation is
  refused (`gone`).
- **What the controller's copies do.** They produce placeholders only (delayed rendering). The
  browser's data is fetched when a program on this computer pastes, and a paste waits at most
  30 s. The placeholders are marked so that clipboard history and the cloud clipboard do not read
  them (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory = 0`,
  `CanUploadToCloudClipboard = 0`). Programs that read every copy regardless (WSLg's `msrdc`)
  make the browser answer immediately. When sharing is turned off or the session ends,
  placeholders still on the clipboard are removed, and a waiting paste gets nothing.
- **The browser** reads its own clipboard only while sharing is on, and only with the browser's
  clipboard permission. It announces formats and sizes, never content, until a paste on the
  computer requests it.

### The pipe name stays held

DeskwayNet creates the agent pipe once and keeps it, reusing the same handle for each agent
(`DisconnectNamedPipe`). As a result, a normal user cannot take over the name while the console
session changes. The UI pipe gives interactive users read and write access (`0x12019B`) but not
`FILE_CREATE_PIPE_INSTANCE`, so they cannot add server instances of their own.

### Locking

When a session ends with auto-lock on, the agent locks the console. Running as SYSTEM, it
disconnects the console session (`WTSDisconnectSession`), which shows the lock screen and keeps
every program running; `LockWorkStation` works only from the signed-in user's own processes. The
agent also locks the console on its own if the network service disappears in the middle of a
session.

### Removing devices at a computer

- The panel's **Remove all** removes every device this computer trusts; in service mode this
  requires an administrator.
- `deskway-host passkeys` lists the trusted devices.
- `deskway-host unpair ID` or `unpair --all` removes them.
- Their resume tokens are invalidated immediately.
- `uninstall --purge` also deletes the machine-wide data: the device token, the host key and the
  trusted device keys.

### Starting with Windows

By default both services start with Windows, so a restarted computer can be reached at its
sign-in screen. The panel's **Start with Windows** switch sets their start type to manual, or
back to automatic.

- The panel sends the request to the network service, which passes it to the SYSTEM agent. The
  agent calls `ChangeServiceConfigW` with only the start type set; every other setting is
  `SERVICE_NO_CHANGE`. The network service's low-privilege account cannot change services
  itself, and no account gains the right to change a service's program path or account.
- Anyone signed in at the computer can turn the switch off. Doing so only makes the computer
  unreachable after a restart.
- While the switch is off, the sign-in entry (`--at-login`) exits immediately, so nothing runs.
  Opening Deskway manually runs `start` as an administrator (with a UAC prompt), which restores
  automatic start and starts both services.
- The switch cannot be changed from a browser: if it were turned off remotely, the computer could
  not be reached to turn it back on.

### Ctrl+Alt+Del

On Windows 11 the lock screen can be dismissed with any key, so Ctrl+Alt+Del is not needed by
default. Deskway does not change the `SoftwareSASGeneration` policy. If a computer requires
Ctrl+Alt+Del, the browser tells the person.
