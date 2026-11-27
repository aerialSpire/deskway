# Architecture

This document describes how Deskway is built and the rules that every change follows. For what
the product does, see [the overview](overview.md); for running it, see
[self-hosting](self-hosting.md).

Every change follows these rules:

- No patches. When a mechanism is replaced, it is deleted completely: its code, state,
  messages, documentation and tests. The replacement is a self-contained module with its own
  tests.
- Each problem is solved at the right layer, with the standard mechanism for it. The clipboard
  is modelled as clipboard state (RDP CLIPRDR), a session lives exactly as long as its peer
  connection (WebRTC), the video size is negotiated (H.264 level, MediaCapabilities), and
  keyboard state is synchronized (the RDP sync event). Where a signal exists, no heuristic is
  used instead.
- Every piece of state has a single source of truth, named below.
- Core modules (`session`, `clipboard`, `video`, `input`, `transfer`, `power`, `signal`)
  contain no `#[cfg(target_os)]`. Platform differences live behind `platform::Platform`.
- There is a single protocol definition (`proto/`), with a version and capabilities. Tests check
  every side against it.

## Processes

| Platform | Processes |
|---|---|
| Windows | `DeskwayNet` (network service, low privilege): signaling, sessions, WebRTC, AI clients' tool calls. `Deskway` agent (SYSTEM, in the console session): the `Platform` implementation. User worker (`deskway-host worker`, as the person signed in at the console, started by the `Deskway` service): the ordinary workspace (browser Terminal and Files, AI programs, files, clipboard text, apps and windows). Admin worker (`worker --admin`, the same with the person's elevated token, only while administrator rights are allowed). Panel/tray per signed-in user. |
| macOS | One process: everything, `Platform` and the workspace implemented in-process. |
| Linux | One host process, run as an ordinary user under systemd or in the foreground. It handles signaling, the desk seat's terminal, and the AI seat with its in-process workspace for commands, files and tunnels. |
| AI client's machine | `deskway` (cli/): `login`, `mcp` for MCP over stdio, `gateway` for MCP over HTTP, and a proxy that holds the profile's sign-in and a link to each computer in use. Clients share the profile's proxy by default. A supervised gateway can own its proxy in-process. |

Each platform has exactly one process model. The network service talks to the agent only
through `platform::Remote`, a generic proxy that implements `Platform` over the IPC pipe (calls
travel in one direction, events in the other). A new platform capability consists of one trait
method, one event and two implementations; the proxy forwards it without any feature-specific
code.

## Shared crates (`crates/`)

The Rust programs form one Cargo workspace (`Cargo.toml` at the repository root, built into
`target/`).

```
wire/              wire types for the server and browser channels (serde), checked against
                   proto/messages.json; challenge.rs: every string either side signs or MACs,
                   the six-digit code and the pairing code's form; crypto.rs: SHA-256, hex and
                   randomness; contract.rs (tests, and the `contract` feature): the definition
                   and proto/vectors.json as contract tests read them
peer/              what a WebRTC peer needs whatever it is for: net.rs, the network paths (a
                   socket per local address, IPv4 + IPv6; the STUN address, stun.rs; TURN
                   relays, turn.rs; binding again on every ICE restart); state.rs, the
                   session's state machine, the same on both ends
```

## Host modules (`host/src`)

```
main.rs            command line only
product.rs         the program's name as people see it (Deskway Host), also for build.rs
icon.rs            the shared Deskway D mark for the host, browser client and README. build.rs
                   makes the Windows .ico, the mac-icon example draws the macOS iconset, and
                   the icon test writes and checks the browser and README PNGs
config.rs          the host's config (host.json) and where it lives; private writes
settings.rs        what the panel asks (PanelRequest) and the settings the host carries out:
                   saved first, then applied (keep-awake, language, the signaling host)
i18n.rs            the texts of host/i18n (en, zh-Hans, zh-Hant) and the language in use
display.rs         the displays attached (the wire's Display), the one to capture
auth/              who may connect: the trust store (mod.rs), pairing window and codes
                   (pairing.rs), devices asking to be let in (asks.rs), the account's signed
                   device log (log.rs), offer checks and resume tokens (offers.rs), signature
                   and WebAuthn checks (webauthn.rs)
platform/
  features.rs      the host's feature set selects seats, capabilities and tools
  linux.rs         host commands, operating-system signals, and per-user state directory
  linux/lock.rs    the running host's process id in its lock file, and the signal sent to it
  linux/file_metadata.rs Linux no-replace rename and procfs file handles
  linux/programs.rs process setup and enumeration on Linux
  unix/programs.rs  login shell, process groups and pseudo-terminals on Linux and macOS
  unix/file_metadata.rs Unix ownership, mode, modification time and partial files
  macos/file_metadata.rs Darwin file handles, stored names and no-replace rename
  mod.rs           trait Platform + PlatformEvent (and Call, what crosses the agent pipe);
                   Local (in this process); Remote (agent pipe proxy, Windows)
  windows/         capture (DXGI), encoder (MF), input (SendInput), clipboard (owner window,
                   delayed rendering) and dib (its bitmaps), power (PowerCreateRequest),
                   displays (WM_DISPLAYCHANGE, QueryDisplayConfig ids), desktop (input
                   desktop, lock), ui (window glue, the border), known folders, system language
  macos/           ScreenCaptureKit, VideoToolbox, CGEvent, NSPasteboard (data provider),
                   IOPM, CGDisplay reconfiguration callback, ui (AppKit window glue, login
                   item, permissions), system language
session/
  mod.rs           the driver every session has, whatever controls the computer: one peer
                   connection and its network paths (peer::net), one answer per epoch and ICE
                   restarts on the same connection, the state machine (peer::state) and its
                   deadlines, the greeting on the control channel, the end; trait Role for
                   what the session is for
  desk.rs          the desk role, a person's browser: the picture, input, clipboard and files
                   on the browser's channels (input, move, file)
  desk/            channels.rs: those channels' messages, read once and routed; pace.rs:
                   real time over a congested path, encoding waits while the queue is full;
                   media.rs: the picture's track (payload, mid, keyframes)
  data.rs          the desk seat's data-only role, routing control, terminal, browse and transfers
  file_transfer.rs admitted page's transfer record and resumable channel, with the same worker limit as browsing
  credit.rs        byte credit shared by terminal and file transfers
  terminal.rs      one PTY attachment's resize and credit
  terminal/        registry.rs: the host's one browser PTY, attachment and idle timeout;
                   replay.rs: the bounded recent output and its safe start
  ai.rs            the AI role, an AI client's deskway: control and ai channels (MCP in
                   frames, wire::mcp), no media; idle for 10 minutes gives the seat up
mcp/               an AI session's MCP requests, answered off the session thread: mod.rs
                   (JSON-RPC, Facts: what the AI side knows of this computer, kept by the
                   host), gate.rs (the one checkpoint: Granted, the rules, the audit records)
tools/             the tools of proto/tools.json: mod.rs registers base, work and desktop tools
workspace/         commands and files as the workspace user: mod.rs (Op, Workspace and errors),
                   local.rs (in this process), procs.rs (programs and retained output), files.rs
                   and files/browse.rs (paged directory selection and browser operations),
                   files/transfer.rs (staged writes, streaming reads and partial cleanup),
                   desktop.rs (DesktopOp), remote.rs (Windows user worker pipe)
audit.rs           what AI clients did here: JSON lines, appended only, bounded
video/             capture loop (Pipeline: commands, capture, encode step), encoder choice,
                   size from viewer constraints
input/             injector, key state, lock/modifier sync, key maps (full USB HID tables)
clipboard/         shared clipboard state machine (pure logic), format model, PNG (image.rs)
transfer/          file transfer (ids, offers, chunks) on the file channel
power.rs           keep-awake policy
signal.rs          server connection, host status, the two seats (signal/seats.rs: the desk
                   seat for a person, the AI seat for an AI client, side by side; each a
                   signal/registry.rs)
vitals.rs          startup hardware and a short-lived worker for requested CPU, RAM and disk samples
wake.rs            channel senders that wake the receiving thread's poller
service/           Windows services, agent, IPC; the machine-wide folder; worker.rs, the user
                   and admin workers (their keys, their pipes, reconnecting)
ui/                tray, panel and indicator (mod.rs, indicator.rs), the panel's backend:
                   the network service over its pipe (service.rs, Windows) or the host in
                   this process (inprocess.rs, macOS); installer window (setup.rs); the
                   pages, and what they share, put in at markers like design.css: page.js
                   (texts, messages, elements made from text) and lang.js (the language menu)
```

The panel's state is the host's `signal::Status`, combined with the backend's own facts (whether
the computer is signed in, how it starts, the Mac's permissions). Every setting the panel changes
is a `settings::PanelRequest`, which the process running the host carries out in
`settings::Settings`. The controlling browser's clipboard switch and an administrator's edits to
host.json also arrive there.

The platform selects the host feature set once at startup. It defines the signaling hello's
capabilities and AI tool names, which seats can be admitted, and the tool table consulted by the
MCP gate. Every host offers the base tools. A workspace adds work tools, including loopback
tunnels, and a desktop adds desktop tools. Desktop hosts supply a `Platform` and the picture role.
`host/build.rs` sets `desktop_host` for Windows and macOS. The browser offers desktop control
according to the live hello's `desk` capability. Linux, macOS and Windows offer `terminal`,
`terminal-keep`, `browse` and `file-transfer` through the ordinary user's workspace. View-only hosts omit
those capabilities. The host status reports why a browser workspace is unavailable.
Verified data-only and picture browser offers contend for the same desk seat. A picture offer to a Linux host receives
`no_desktop`. A data-only browser offer to a host without terminal support receives `no_terminal`.
The AI seat uses the same device proofs and signed AI switch on all hosts.

The data-only role runs one browse operation and one transfer per session at a time, with at most
two filesystem workers across the host. Before calling the workspace a worker checks the desk
seat using the terminal registry's admission state. The workspace resolves the directory on
the first page, selects each sorted page with bounded memory and applies filesystem changes.
The browser uses the returned path and cursor for successive pages. The AI listing scans
until it has up to 2,000 readable entries, with its own `more` field. The browser workspace
checks access before describing each page entry and before each transfer filesystem step.
A transfer holds one worker slot so a second slot can service browsing. Its channel and receiver credit limit the amount of transfer data
competing with terminal and browse traffic. On Windows, file handles and ConPTY live in the
ordinary user worker. A worker request for PTY bytes or file contents carries up to one frame.
Publishing an upload, the one request made while holding the registry guard, waits up to five
seconds for its reply; an unanswered publish fails the transfer as unconfirmed and releases the guard. ConPTY has a bounded
input queue and supplies up to the requested output capacity. The Windows foreground host
uses the same filesystem and ConPTY code in-process. On macOS and Linux the workspace uses
the shared Unix PTY and filesystem metadata code.
The host keeps the current transfer record and final reply in memory, keyed to the device and
the authenticated page's resume chain. A new channel takes over the same record after the prior
worker returns it. The workspace owns the staged upload, its write position, and the pinned
download source and original metadata. The browser's session view owns its list, local source
and destination, and the offsets and incremental digests used to resume. The browser hashes
SHA-256 blocks incrementally as it sends or writes each chunk. Web Crypto's digest API takes
the entire input.

The host's terminal registry owns the PTY even with no desk session. Only the admitted desk
session can attach. It owns its own input queue and credit, while the registry owns the recent
output. A worker drains the PTY when detached, including when signaling is unavailable.
The registry reports a detached shell's last device and departure time through host status.
It drops the shell, jobs and replay when the terminal closes or the host stops. The signaling
host processes queued workspace transitions before judging offers. A lock revokes the current
data seat and releases it without auto-lock. The workspace checks the live console state before
attached output, input and filesystem calls.

- Windows uses WTS session notifications for console lock and sign-in changes. The user worker
  checks for secure prompts when activity begins and every 100 ms while it is answering a
  request, has an attached Terminal or Files session, or holds an open browser PTY, upload, or
  download. When activity ends, availability follows WTS alone. If WTS registration fails,
  the worker polls continuously. Browser workspace operations check the live desktop state at access.
  A temporarily detached console has no active console session ID. The worker checks its own
  WTS logon identity and reports it locked during detachment. Logoff or a switch to another
  console session ends the old workspace, including its shell, replay and transfer record.
- macOS samples `CGSessionCopyCurrentDictionary` every 100 ms and checks it at workspace access.
  `CGSSessionScreenIsLocked` reports the lock and `kCGSSessionOnConsoleKey` reports whether the app's
  session is on the console. Switching away locks access to that app's workspace. Logging out
  stops the app and its workspace. These checks are independent of capture.

The Linux host keeps its state in `$XDG_STATE_HOME/deskway` or `~/.local/state/deskway`.
Starting or enrolling the host takes the same exclusive `host.lock` for that state directory.
Enrollment saves the sign-in and exits. The running host prints the six approval digits to
the terminal or systemd journal whenever it enters the waiting-for-approval state.
SIGTERM and SIGINT enter the signaling host's normal quit path. `deskway-host stop-ai` sends
SIGUSR1 to the process recorded in the held lock file, whatever the saved switch says. The signal thread delivers `Control::StopAi` and the command
checks the saved `ai.json`. When the command owns the lock itself, it stops the saved AI switch
through `auth`. Linux uses str0m's dimpl DTLS implementation with system OpenSSL for
cryptography and the existing native-tls stack for HTTPS and WebSockets.

### `platform::Platform`

The trait offers the capabilities a session needs, at the level at which a session uses them:

- `video`: start and stop, keyframe, bitrate, viewer constraints, display choice. It produces
  encoded frames, the cursor shape and position, and `CaptureAvailable(bool)`.
- `input`: key (by position), button, move (absolute or relative), wheel, text, character with
  layout, synchronizing locks and modifiers, releasing everything. It reports the layout's labels
  (`LayoutChanged`).
- `clipboard`: announcing formats (delayed rendering), answering a render request, reading a
  format. It reports `ClipboardChanged(FormatList)` and `ClipboardRender { gen, format }`.
- `files`: receiving into Downloads, offering copied files.
- `displays`: listing them, and `DisplaysChanged` from the OS event (`WM_DISPLAYCHANGE`, or
  Quartz's reconfiguration callback). Each display is identified by a stable id: the hash of the
  monitor's `QueryDisplayConfig` device path on Windows, and the display UUID on macOS.
- `desktop`: locking, and the input desktop (on Windows the input desktop itself; on macOS the
  session's lock state).
- `language`: the language of notes shown at the computer.

Keep-awake is not a `Platform` call. `power.rs` is called by the process that knows the reason
for it (for the setting, the always-running part; for a session, `Local`), and each of these
holds its own native request.

Only the session that holds the computer can act on it. Every call that is an action of a
session (input and its release, capture and its pacing, clipboard, files, `video_stop`) carries
the session's id, and `Local` carries it out only for the session that most recently started
video. This single check, made in `Local` where the calls arrive in order, prevents late actions
of an ended or replaced session from reaching its successor. Calls that concern the whole
computer (`video_start`, previews, locking, start-up, language) carry no session id.

There are two implementations:

- `platform::Local` runs in-process. It builds shared pipelines (`video/`, `input/`,
  `clipboard/`) on top of the native pieces in `platform/windows` or `platform/macos`. The macOS
  app and the Windows agent use it.
- `platform::Remote` (in the Windows network service) implements the same trait over the IPC
  pipe. Calls and events are each a single generic enum; the agent runs `Local` and serves them.

## The AI client (`cli/src`)

```
main.rs            the commands: login, logout, mcp, gateway, computers, call, proxy, update
account.rs         new clients: a browser opens the sign-in window and approves after
                   comparing six digits (code-v2); approved clients sign in with their key
keys/              the device key (the system's key store where there is one)
store.rs           what it keeps on this machine, in a folder only its user reads
mcp.rs             the shared MCP tool list from proto/tools.json and tool dispatch
stdio.rs           line-delimited MCP for `deskway mcp`
gateway.rs         Streamable HTTP for `deskway gateway`: loopback with Host and Origin checks,
                   private Unix sockets, version handling, and remote call limits
proxy_client.rs    a persistent connection to the profile's proxy, reused across MCP requests
proxy.rs           the shared or gateway-owned proxy: the signaling socket, the account's
                   computers, where this machine is (peer::here), one link per computer in use
link.rs            one connection to one computer: offer signed with the device key, the
                   computer's signed answer checked against its pinned key, MCP in frames
os.rs              per-platform state files, detaching the shared proxy, gateway sockets and shutdown
update.rs          keeping the program itself up to date from the server
copy.rs            copy_to and copy_from, done on this side
```

## AI control

AI clients (Claude Code, Claude Desktop, Cursor and other MCP clients) use a Deskway computer as
a resource: they can see the screen, operate the interface, run commands, read and write files,
and start and stop programs. The security rules are in [security.md](security.md) ("Devices",
"AI clients at the computer"), and the messages in [protocol.md](protocol.md) ("AI sessions").
Setup is described in [ai-clients.md](ai-clients.md).

The design follows these principles:

- An AI client is a controller, just like a browser. Its `deskway` profile holds a device key
  and goes through the same end-to-end checks. With remote MCP enabled, the server runs
  the profiles for approved remote applications and holds their keys.
- The computer is the authority on tools: it implements and checks them, and it reports which
  ones it has. `deskway` does only what belongs on the AI client's machine (choosing the
  computer, sharing one connection, `copy_to`/`copy_from`). The server contains no tool code.
- An AI client cannot widen its own rights. Allowing AI, allowing administrator rights and
  approving devices all require a browser's signature. Narrowing rights (Stop, switching AI off,
  removing a client) requires no proof.
- Safety comes from visibility (the indicator and the page), the ability to stop the AI (Stop),
  auditing (the computer's audit log) and removal. There is no per-call confirmation and no
  keyword list.
- AI and a person control the computer side by side. Programs an AI client starts are ordinary
  programs of the computer and outlive its session.

### Topology

By default, clients of a profile share the proxy and one link per computer as shown below.
A gateway started with `--own-proxy` has its own in-process proxy and links.

```
MCP client window 1 -stdio-> deskway mcp -+
MCP client window 2 -stdio-> deskway mcp -+-127.0.0.1-> deskway proxy (one per profile)
HTTP client -HTTP-> deskway gateway -----+
                                                          | signaling /ws/client (login cookie)
                                                          | one WebRTC connection per computer:
                                                          |   "control", "ai", "tunnel:<port>"
                                                          v
DeskwayNet (Windows) / the app (macOS) / deskway-host (Linux)
   session (ai role) -> mcp (gate) -> tools -> Platform (screenshots, input)
                                           +-> Workspace (programs, files, clipboard text, apps;
                                               on Windows in the user worker)
```

### Identity

- `deskway login` signs in with the account name only, never with the account password. A new
  client is accepted only while a browser of the account has opened AI sign-ins
  (`POST /api/ai/window`, for 10 minutes, `aiWindowTTL`, held in server memory). A browser opens
  them by copying the prompt on **Add an AI client**. The client then waits for approval like any
  new device: `deskway` shows six digits (`deskway-code-v2`, which includes the role), and a
  browser compares them and signs `deskway-endorse-v2`. At any other time, and for an unknown
  name, the answer is the same 403.
- An approved client signs in again with its key (`deskway-ai-login-v1`, with a time within 5
  minutes of the server's, and each signature accepted once). The proxy does this on its own
  when the server answers 401.
- The device key is a P-256 key stored for the current user (`cli/src/keys`). See
  [Device keys](security.md#ai-client-keys) for how each platform keeps it.
- An AI client appears on **My devices** with an AI tag.

### The AI switch

AI control is set per computer. It is off by default and is turned on in **My devices** on that
computer's entry. The switch is one signed entry, `{host, ai, admin, ts, by, sig}` over
`deskway-ai-v1`, which carries both AI control and administrator rights. The server keeps the
latest entry for each computer separately from the device log (`POST /api/computers/{id}/ai`,
sent as `devlog.ai`). The computer accepts only entries that are signed by a trusted browser and
are newer than any it has seen (`host/src/auth/ai.rs`, stored in `ai.json`). Stop at the
computer (in the panel or indicator, or with `deskway-host stop-ai` on Linux) ends AI control
until a browser signs a newer entry.
When the state stops being on, the computer ends the AI seat (with `ai_off`, or `unpaired` for a
removed client). The host reports `ai_state` (`off | on | stopped`) in its status, for display
only.

### Rules the server enforces

An AI client's login may use only `GET /api/me`, `GET /api/devices`, `GET /api/ai/clients`,
logout and the signaling socket, and on the socket only what a session needs. `pair` and
`pairask` are refused. Its offers must be signed with its device key (`dk`), so that a computer
with device checks off still recognizes it as an AI client (`internal/server/auth.go`,
`signaling.go`).

### The local proxy

By default the first `deskway mcp` or `deskway gateway` starts `deskway proxy` (`cli/src/proxy.rs`), which
every MCP client using that state directory shares as one AI device. The proxy holds the
sign-in, the signaling socket and one link for each computer in use (`link.rs`). It listens on
127.0.0.1, at the port recorded in `proxy.json` together with an access token (a file only this
user can read). Both transports number their proxy requests on a persistent connection.
The gateway keeps its connection open across HTTP requests and the computer's idle rule
releases unused seats. Calls can overlap on either transport. When the last proxy client
disconnects, the proxy releases every computer at once, and after 10 minutes with no client
and no call it exits. A newly started MCP process that finds a proxy of a different version
replaces it.

`deskway gateway --own-proxy` runs the same proxy loop in the gateway process and submits calls
directly to its request queue. The owned proxy has no local network listener or `proxy.json`.
The supervisor keeps the write end of the gateway's stdin pipe, whose EOF requests shutdown.
On Linux and macOS, a signal waiter also requests shutdown on `SIGTERM` and `SIGINT`.
Orderly shutdown gives active links time to send goodbyes. Process exit closes every link socket,
including on `SIGKILL`. The host ends a link lost abruptly through its existing peer liveness rules.
The saved profile sign-in governs the owned proxy's lifetime. Clearing it with `deskway logout`
or failing to renew it releases the links and makes the gateway exit nonzero with an error.
For Unix sockets, the gateway removes its socket on orderly exit. If the path already exists,
startup fails. After a forced exit, the supervisor confirms the process is gone and removes its
socket before restarting.

### The channel

An AI session has no media. The `control` channel carries hello and end, and the `ai` channel
carries MCP JSON-RPC messages of at most 1 MiB each, sent in 64 KB fragments (`wire::mcp`, queued
by `peer::outbox`). A `tunnel:<port>` channel carries one TCP connection to a loopback port of
the computer that this session's `tunnel` call allowed (`peer::pipe`, `tools/tunnel.rs`). The
host ends the session with `idle` after 10 minutes without a tool call (`session/ai.rs`), and the
proxy reconnects on the next call.

### Two seats side by side

A computer has a desk seat (for a person's browser) and an AI seat (`signal/seats.rs`), each a
`Registry`. The two seats do not exclude each other. On the AI seat, the same AI device always
replaces its own session, any other AI device gets `busy`, and `force` and offers with media are
refused. The status reports both holders (`holder`, `ai`). In the device list, `controller`
describes only the desk seat and `Device.ai` the AI seat, so an AI client never triggers the
takeover question. The page's toolbar says "{name} is also controlling this computer".
**Disconnect** at the computer ends the desk seat, Stop ends the AI seat, and auto-lock follows only
the desk seat.

`platform::Local` decides which session's calls it obeys. The desk holder may make every `held`
call; the AI holder may make only input calls. Video, clipboard and file calls are never
available to the AI.

### Input apart per session

Input state is kept per hand (`input::Hand`: `Person`, `Ai`): each hand has its own held keys
and buttons and its own display that its moves refer to. A key held by both hands is released
only when both let go of it, and `sync` comes only from the person's browser. The AI's input is
dropped at the point of injection when the input desktop is a secure one (the lock screen,
UAC); the gate's earlier `secure_desktop` answer is only a hint.

### MCP and tools

`proto/tools.json` is the single definition of the tools (name, `screen`: `none | read | act`,
`needs_user`, arguments and MCP annotations). `crates/wire` reads it at build time, and the
shared MCP server answers `tools/list` before any request reaches a computer. The host implements its tools in
`host/src/tools` and reports its list in its hello (`tools`, which the server keeps in
`Device.tools`). The proxy answers `unsupported` for a tool that a computer lacks, and
`computers` lists the `missing_tools`. The client catalog adds `computers`, `release`,
`copy_to` and `copy_from` (which transfer 256 KiB pieces through `fs_write`/`fs_read`, then run
`fs_hash` on both ends). The shared MCP server uses the catalog's `local_only` flag to
exclude `copy_to`, `copy_from`, and `tunnel` from HTTP discovery and dispatch.

The tools are `computer_info`, `audit_read`, `screenshot`, `wait_for`; input (`click`, `move`,
`drag`, `scroll`, `key`, `type`, with coordinates in the pixels of the last screenshot);
programs (`run`, `proc_start` in a terminal, `proc_write`, `proc_read`, `proc_kill`,
`proc_list`); files (`fs_list`, `fs_stat`, `fs_read`, `fs_write`, `fs_hash`, `fs_move`,
`fs_delete`); clipboard text (`clipboard_get`, `clipboard_set`); `tunnel`; apps and windows
(`windows`, `window_act`, `app_list`, `app_open`, `app_quit`); and controls (`ui_tree`,
`ui_act`). When nobody is signed in, only tools with `needs_user: false` work, so that an AI
client can type a password at the sign-in screen.

### The gate

Every call passes through `mcp::gate` (`host/src/mcp/gate.rs`), the only place that creates a
`Granted`. The gate checks that AI is on, the computer offers the requested tool, and the
arguments are valid. Tools marked `needs_user` require an available workspace user. The gate
blocks input on view-only computers and checks both availability and consent for administrator
rights. For `act` tools it reports `secure_desktop` early. It writes an audit record at the start
and at the end of each call.

### Workspace and process lifetime

`workspace::Workspace` runs commands and works with files (`Op`). Desktop hosts also implement
clipboard text, apps, windows and accessibility controls (`DesktopOp`). Both request types
use tool names as tags. On Windows they cross the user worker's JSON pipe with the same
request and reply shape. The service starts that worker as the person signed in at the
console. On macOS and Linux the workspace runs in the host process as its user.

On macOS and Linux, ordinary commands use `$SHELL -lc` and an interactive terminal starts `$SHELL -l`.
The defaults when `SHELL` is unset are `/bin/zsh` on macOS and `/bin/sh` on Linux. On both
systems, `proc_kill` ends a piped command's process group or a terminal's session, including
its job-control groups.

Programs belong to the computer and can outlive the AI session. The computer keeps their
output in a bounded buffer, read by offset. The host has no shutdown sweep for programs it
started; on Linux, stopping the systemd service ends them with it, as systemd stops the unit's
whole control group. On Windows programs can leave the service's job, and a terminal program loses its
console if the user worker restarts.

### Audit

`host/src/audit.rs` writes one JSON line at the start and one at the end of each call. The log
is append-only and is kept next to the trusted devices (on Windows in the service's folder, which
the person cannot write to). It is rotated to `audit.old.jsonl` once it exceeds 4 MB. It is read
back by `audit_read` and by the account's browsers.

### Distribution and updates

The server distributes `deskway` for each system from the `-clients` folder (by default
`clients`, next to the state file), under the fixed names listed in
`internal/server/clientprograms.go`. The release workflow stamps the server with the
GitHub release tag and hashes of its final client assets. One server-owned goroutine fetches
missing release programs at startup, verifies them and installs them atomically in the
`.release` cache inside `-clients`, using one path per filename. The fetcher treats a cached
program with a different SHA-256 as missing and retries failed fetches after a delay.
Shutdown cancels the goroutine and waits for it to clean up. `-fetch-clients=false` disables
outbound fetching. The handlers read from disk, looking for operator-supplied files first,
then for a cached program with the expected SHA-256.

`GET /api/ai/clients` lists available programs with their SHA-256 hashes. The browser adds
`?details=1` to also see why programs are missing.
`GET /download/{file}` serves them to anyone, because a client downloads the program before it
signs in. `scripts/build-ai-clients.*` builds them. `deskway mcp` compares its own file
with the server's at start-up and every hour. If they differ, it downloads the new file,
checks its SHA-256 and puts it in place for the next launch. On Linux a single rename replaces
the file. On Windows and macOS the updater moves the old file aside first. `deskway update`
does the same immediately.

## Session lifecycle

The complete lifecycle (owners, authority, states, negotiation, ending, signaling loss, who
controls, resume, history) is described in [session-lifecycle.md](session-lifecycle.md).

## Clipboard (RDP CLIPRDR model)

The source of truth is the host's clipboard state, `{gen, owner: local|remote, formats}`. The
host assigns `gen`, and the latest announcement wins.

From the user's point of view, the clipboard behaves as follows:

- Copying on one side triggers nothing on the other side: no copy and no transfer.
- Pasting on the other side puts the content directly at the cursor, with no intermediate
  "copied to the clipboard" step.
- A paste that takes time shows a small progress indicator in the floating notes, with ✕ to
  cancel it (in which case nothing is pasted).
- Pictures and files behave the same way, in both directions, on every device.
- Pasting into a target that does not accept the format is handled the native way: several
  formats are offered and the target picks one (files are also offered as their full paths as
  text, and pictures as image data). A target that accepts none of them gets nothing, just as
  with a local paste.
- Sharing is off by default and is a setting of the computer, switched from the toolbar. All of
  the clipboard UI floats over the screen.

The formats are `text` (UTF-8), `html`, `image` (PNG, width, height, size) and `files` (name,
size, and their full paths as text).

- Remote copy: the platform reports `ClipboardChanged`, and the host announces
  `{t:"cb", gen, formats}`. Small text (<= 48 KB) travels with the announcement.
- Local copy: the browser detects the copy (with `clipboardchange` where supported; otherwise on
  focus and visibility changes, and with a check once a second while the page is in front) and
  announces only the list of formats. The host takes ownership of the remote clipboard using
  delayed rendering, that is, by putting placeholders on the OS clipboard.
- Paste on the remote computer (by any means: keys, menu or button): the OS asks for the data,
  and the host sends `{t:"cbreq", gen, format}`. The browser reads its clipboard and answers
  `{t:"cbdata", gen, ...}` (sending large data on the file channel), or `{t:"cberr"}`. Progress is
  shown for slow answers, and cancelling answers the OS with no data.
- Paste on this device (when the remote computer is the owner): text is written to this device's
  clipboard immediately if the page is in front, or otherwise when the page comes to the front
  again, provided it is still the latest. Images and files remain a description until the person
  asks for them (**Get it here**), because the browser cannot yet provide data lazily to its OS,
  and nothing is transferred speculatively.
- Paste keys are ordinary keys, and nothing in the browser inspects them.
- Browsers cannot read files copied on this device. Such files reach the remote computer by being
  dropped on the screen or sent with **Send files or images**, both of which also put them on the
  remote clipboard.
- Sharing on or off is a setting of the computer, shown and changeable both in its panel and in
  the session.

The state machine (`Sync`), `RenderWait`, the format model and the `Native` trait live in
`host/src/clipboard/`. Windows uses an owner window and a format listener in
`platform/windows/clipboard.rs`. The macOS watcher in `platform/macos/clipboard.rs` checks
`changeCount` as soon as sharing turns on, then twice a second while sharing is on. The watcher
blocks while sharing is off. The `NSPasteboardItemDataProvider` in `clipboard/promise.rs`
dispatches Cocoa events while it waits for the browser's answer. A new announcement, or turning
sharing off, revokes the promise. Large data is carried by `transfer/` under the clipboard's ids
and generations. On the browser side the clipboard is implemented in
`web/static/js/session/clipboard.js`, with its UI in `clipboard-ui.js`.

## Video

The source of truth is the viewer's constraints.

- The browser sends `{t:"view", w, h, maxW, maxH, fps}` with the offer (so that the host starts at
  that size while the connection is being set up), when the control channel opens, whenever the
  stage changes size (window resizing, rotation, full screen; detected with a `ResizeObserver`),
  when the device pixel ratio changes (moving to another monitor; detected with a
  `matchMedia('(resolution: Ndppx)')` query that is re-armed after each change), and when a zoom
  comes to rest (a pinch's fingers lifting, or a zoom set in one step; there is no wheel zoom, so
  there is no gesture without an end signal). `w` and `h` are the stage in device pixels
  multiplied by the zoom. `maxW` and `maxH` are the largest rung of a ladder of sizes (720p to
  8K, landscape) that `navigator.mediaCapabilities.decodingInfo({type:"webrtc"})` reports as
  supported and smooth for H.264 High at that rung's level. The browser asks once per session;
  without the API, the viewport is used instead (`web/static/js/session/constraints.js`).
- Encoder selection (`video::open_encoder`) follows one rule on every platform. The platform
  lists its encoders, best first (Media Foundation: hardware, then software; VideoToolbox:
  low-latency hardware, hardware, software), and opens one by index. Each new encoder is tried
  starting from the preferred one. Failing to open at a particular size does not change the
  preference, since the next size may work. Only an encoder that fails while running moves the
  preference to the next one.
- A stage with no area (for example while the page is hidden or being laid out again) has no
  viewport: nothing is sent, and the host keeps the last constraints.
- The host (`video/size.rs`, a pure module) encodes at the source size fitted into the viewport
  (but never below a short side of 240, as far as the source allows) and into the decode box
  (rotated to match the display), never scaled up, with even dimensions, and then shrunk until it
  fits the H.264 level limit (Table A-1: MaxFS, MaxMBPS, side <= sqrt(8 MaxFS)). The level limit
  is the offer's `profile-level-id` (for the payload type in use) until the viewer has sent a
  decode box; after that it is the encoder's highest settable level (5.2 on Media Foundation and
  VideoToolbox). Browsers offer a fixed level 3.1 (`42e01f`, or `64001f` with
  `level-asymmetry-allowed=1`) regardless of what they can decode, so the decode box that the
  viewer vouched for, by querying MediaCapabilities with the rung's level, replaces it. The
  stream is marked with the lowest level that can hold it (`MF_MT_MPEG2_LEVEL`,
  `kVTProfileLevel_H264_High_*`).
- The encoders accept the exact size: Media Foundation (AMD, and the Microsoft software MFT) and
  VideoToolbox pad to whole macroblocks and crop in the SPS (verified by
  `encoders_pad_and_crop_real_sizes`, which is run manually).
- A new size (from new constraints, or a display being switched or rotated) creates a new
  encoder, which starts with a keyframe. This happens at most once per keyframe interval, and the
  latest constraints win.
- Frame rate: at most 24 frames a second (`video::size::FPS`), and only while the screen is
  changing. A still screen is refined a few times, after which nothing is sent. The browser asks
  for fewer frames in `fps`: its picture setting **Sharper** (the default,
  `web/static/js/picture.js`) asks for 2, and **Smoother** for as many as it decodes smoothly, up
  to 24. The first change after a pause is sent immediately; only continuing changes wait for the
  next frame.
- Bitrate: the ceiling is the Kush gauge for the encoded size at the full 24 frames a second
  (w x h x 24 x 0.07), shared among the frames the viewer accepts, with each frame limited to 0.4
  bits per pixel (`stream_bitrate`), and kept within the level's MaxBR. At 24 frames a second the
  stream gets the full gauge; at 2 frames a second each frame gets about six times as many bits as
  at 24 (up to about 410 KB for a 3840x2160 frame), which keeps text sharp. The agent reports the
  ceiling (`EncodedSize`), and bandwidth estimation (TWCC) probes up to it and lowers the
  encoder's rate from there.
- Real time takes priority over completeness (`session/desk/pace.rs`). A TCP relay never drops
  data, so on a slow or proxied link what the computer sends could queue up for seconds. The
  system send buffer is kept small (`crates/peer/src/turn.rs`) so that the queue is measured in
  the host. Once more than about a quarter of a second of data is waiting to be sent, encoding
  pauses (`network_behind`). When half of it has drained, encoding resumes with the screen as it
  is at that moment. The screens in between are skipped, but nothing already encoded is dropped,
  so no keyframe is needed.
- Recovery after a display goes missing is signalled by `CaptureAvailable(false/true)` from the
  platform. The browser then requests a keyframe and waits for the next presented frame with
  `requestVideoFrameCallback`.

## Input

- Keys are sent as positions (`KeyboardEvent.code`). A single table maps every code to its
  Windows scan code and macOS virtual key (`input/keys.rs`); an unknown code is logged once.
- Sync (`{t:"sync", locks, mods}`) is sent at session start and whenever the page gets focus,
  using `getModifierState` from the first event after that. The host sets the toggle keys and
  modifier state to match (on Windows with `GetKeyState`/`GetAsyncKeyState`; on macOS with the
  HID system's state, setting Caps Lock through `IOHIDSetModifierLockState`). Every macOS event
  carries the flags of the modifier keys that the backend holds down, set on the event itself
  (with Caps Lock read from the HID system). Nothing is inherited from the system's modifier
  state, so a release the system missed cannot turn later keys into shortcuts. When the page
  loses the keyboard, the browser releases any modifiers that a `sync` pressed on the remote
  computer.
- On a Mac controller, browsers do not deliver keyups for keys released while Cmd is held, so the
  browser releases those keys when Cmd goes up.
- Text from an input method on the controller (a phone keyboard, a computer's input method or
  dictation) is sent as text (`{t:"x", v}`); line breaks and deletions are sent as Enter,
  Backspace and Delete. A single binding handles every text box that accepts such input
  (`session/text-box.js`). Once a physical keyboard has been used on the device, focus on the
  screen is placed in such a box (invisible until something is composed in it), so keys taken by
  the input method come back as text while every other key is still sent as its position. On a
  device without a physical keyboard, the screen keeps the focus itself so that no system
  keyboard appears; the on-screen keyboard's **Input method** key has its own box for that
  purpose.
- Characters from an input method combined with modifiers are sent as characters
  (`{t:"kc", ch, mods}`), and the host resolves them with its own keyboard layout.
- The host reports its layout's key labels, and the on-screen keyboard draws them.
- Relative mouse movement (Pointer Lock) is available as a session mode for games and 3D
  programs.
- The pointer position reported by the host carries the sequence number of the last move it
  applied; older reports are ignored.

## Signaling and server

- Messages and their fields are defined once, in `proto/messages.json`, and the Go, Rust and JS
  code is checked against it. Every hello carries `version` and `caps`. The server's hello to
  browsers also carries `build` (a hash of the client files it serves). A page that is told of a
  build different from the one it loaded shows that a new version is available, on its regular
  pages but never during a session; reloading picks up the new version.
- The server keeps sessions alive across socket loss in a single owner, `Sessions`
  (`sessions.go`), which holds the records, the browser each session belongs to, which of that
  browser's sockets currently carries it, and routing, as well as host status and browser
  attach. `Sessions` owns its store behind methods, with no locking from outside.
- `internal/server` is organized by responsibility:

  | File | Responsibility |
  |---|---|
  | `server.go` | connections, routes, request helpers |
  | `auth.go` | cookies, sessions, sign-in and the failure limiter |
  | `account.go` | account names and password changes |
  | `members.go` | members and their links: list, create, issue, revoke, remove |
  | `setup.go`, `links.go` | one-time account link issuance, expiry and redemption |
  | `browsers.go` | approvals, browser keys, the device log, the device-check policy |
  | `aiclients.go` | AI clients' sign-in without the password and the 10-minute window in which an account accepts new ones |
  | `clientprograms.go` | AI client program lookup, cache and HTTP responses |
  | `mcp.go`, `mcp_start_linux.go` | opt-in MCP routes, public origin, Linux client preflight and profile lifecycle |
  | `mcp_authorization.go`, `mcp_clients.go` | OAuth request validation, registration, metadata fetching and browser approval |
  | `mcp_store.go` | registrations and grants bound to approved device records, resource and token validation |
  | `mcp_gateways_linux.go` | private gateway processes for approved profiles, idle and shutdown handling, bounded HTTP forwarding |
  | `release.go` | release stamp, checksum parsing and verified downloads shared by AI client distribution and server upgrades |
  | `selfupgrade.go` | atomic replacement of the server executable from a verified release archive |
  | `computers.go` | enroll, leave, rename, remove, the device list and who holds each computer as each browser sees it; at most `maxComputersPerUser` per account |
  | `previews.go` | preview refresh requests, cached previews and live vitals |
  | `signaling.go` | the sockets, routed through `sessions.go` |
  | `turn.go` | the relay and the ICE servers, whose TURN credentials name the account |
  | `relays.go` | relay enrollment, admin routes, signed reports, and selection of online nodes |
  | `relaynode.go` | the standalone node's local identity, TURN startup, and HTTPS reports |
  | `tlssplit.go` | the shared HTTPS listener that sends decrypted connections to HTTP or TURN |
  | `relaylimit.go` | each account's share of the relay |
  | `store.go`, `records.go` | the state |

  The HTTP and devlog shapes of browser keys and their log (`BrowserKey`, `LogEntry`, `Proof`)
  live in `internal/proto`, together with the signaling types.
- With `-remote-mcp`, the server publishes `/mcp` and OAuth discovery, registration,
  authorization and token endpoints. Valid authorization requests open the approval page at
  the web client's root. The Linux CLI owns its profile under `mcp` beside the state file.
  The store stages the browser's existing signed AI endorsement in memory so the CLI can sign in.
  The first successful code exchange saves the device, signed log entry and grant together.
  Abandoned requests expire after ten minutes and unused codes expire after two. Startup
  removes orphan profile directories and retires grants whose directories are gone.
- The Linux supervisor starts a gateway on demand for each grant. A gateway owns its proxy
  and listens on a private Unix socket. Its idle limit is five minutes. The pool holds up to
  16 processes and answers requests needing another with HTTP 503 and `Retry-After`. The
  server caps request bodies at 2 MiB, forwards MCP headers and the body, and allows up to
  240 seconds for a call. The child holds the profile directory lock until exit and stops when
  the server closes its stdin. The supervisor clears stale sockets before restarting a gateway.
  A gateway has five seconds to bind before the supervisor stops and reaps it, logs up to 4 KiB
  of its stderr tail and waits five seconds before allowing the same profile to start again.
  A gateway that reports loss of its sign-in retires the grant.
- The state file belongs to a single process. The store holds an OS lock on `state.json.lock`
  while it is open and rewrites the whole file from memory. Account links live in
  `state.json.links` with a separate OS lock held for each operation. `deskway-server admin-link`
  writes the fixed admin link record under the links lock. The server resolves that record to
  first setup or admin recovery under the store lock. Link redemption commits the account
  password, browser key, device log and browser approval in one state save.
- Accounts (docs/accounts.md) are the admin and the members it creates. Each uses only its own
  computers, and nothing is shared between them. An account is stored under a random id; what it
  signs in with is its `name`, which is unique and can be changed (`POST /api/me/name`, after
  entering the password). Everything else refers to the id: the owners of computers and browsers,
  account links, sessions, and the `user` that a host is given and reports.
- The host's signaling thread waits for socket readability, a notification from its local
  queues, or the next ping, silence or browser-transfer deadline. It drains buffered WebSocket
  and TLS data before waiting. It sends a ping every 15 seconds and reconnects after 45 seconds
  without a server frame. The matching pong measures host-to-server latency on that socket.
- The signaling process reads the machine's hardware once at startup. It measures load on a
  short-lived worker when the server's preview request includes vitals. The worker takes two
  CPU samples about 250 ms apart and wakes the requesting connection with the result. The
  server owns the 30-second limit on requests per computer, keeps load and latency on the
  live connection, and saves the last host program version with the computer's record.
- Connect signatures use a nonce issued by the host, not the clock.
- Connecting waits only for what it must. The server sends browsers and AI clients the main
  server's ICE entries on every socket and renews them before the TURN credentials expire.
  The host answers with its host candidates and then trickles its STUN
  address (`crates/peer/src/stun.rs`, with probes answered
  through the sockets' own readers) and its relay candidates. Capture and the encoder start when
  the offer is accepted, on the offer's `display` (or the primary display if absent or detached)
  and at the offer's `view`. The first frame sent after DTLS connects is a keyframe.
- With server-terminated TLS and TURN enabled, the HTTPS listener serves both HTTP and TURN
  over TLS. HTTP ALPN goes to the HTTP server. Without ALPN, an ASCII letter goes to HTTP and
  a STUN first byte goes to TURN. The relay shares its account limits and peer rules across
  listeners. ICE advertises the certificate hostname and HTTPS port in the `turns:` URL. The
  host and CLI verify that hostname using system trust. They offer direct candidates first
  and publish each relay as it is ready. ICE candidate priorities prefer UDP, TCP, then TLS.
- `deskway-server relay` runs the same UDP and TCP TURN listeners in a separate process and
  reports to the main server over HTTPS. The main store owns each relay record and its TURN
  secret. Join tokens are single-use and expire in 10 minutes. Tokens and last report times
  live in server memory. A node reports every 30 seconds and is online for 90 seconds from the
  last accepted report. Each computer measures UDP STUN round-trip times to the main relay and
  online nodes when it receives a relay list. The server keeps those measurements on the computer's
  signaling connection and offers the host the nearest measured online relay for each new session,
  using that relay's own secret. With no usable measurement, the server falls back to the main
  relay when configured or the first online node otherwise.

## Power

`prevent_sleep` is a setting on desktop hosts. The panel shows it as **Keep this computer
awake**, directly under **Start with Windows** (on Windows, which sets the services' start type)
or **Open at login** (on macOS, which controls the login item). It only keeps the computer awake;
the operating system still controls locking and verifies the sign-in password. New macOS
enrollments enable sleep prevention and register the per-user login item. On Windows, sleep
prevention is off by default. The panel applies the setting through the network service on
Windows and in the app on macOS.

The always-running part of Deskway (the network service on Windows, the app on macOS) requests
keep-awake while the setting is on. On battery below 15%, Deskway releases the setting's request
and reapplies it on external power or at 15% or higher. The platform reads the power state on
enable and when the OS reports a change. Turning the setting off unregisters the notifications.
A failed read or registration is treated as external power. Deskway logs a registration failure
once per enable. Deskway reissues the request when external power returns because Windows can end
it on battery on Modern Standby devices. The display may still turn off, and the OS handles lid close.

A desk session keeps the system and display awake, and an AI session keeps the system awake.
The battery rule applies only to the setting and is table-tested in `power.rs`. Each process
holds one native request using `PowerCreateRequest` on Windows or IOPM assertions on macOS.

## macOS host

The Mac host is a single process (the app) that contains everything, including `Platform` and
the workspace. It requires macOS 15 or later (`LSMinimumSystemVersion`).
`scripts/build-mac-host.sh` builds it, signs it ad hoc and packages it. Because an ad-hoc
signature identifies a single build, an update can require the privacy permissions to be granted
again. User setup is described in [install-host-macos.md](install-host-macos.md).

Platform code sits behind small interfaces, each implemented in `platform/windows` and
`platform/macos`: `video::Capturer`, `video::Encoder` (a catalog tried in order),
`input::Backend`, `transfer::Host`, `clipboard::Native`, `platform::ui` (the panel, indicator and
border, capture exclusion, the tray icon, the login item, permissions), and a few small functions
(the config folder, the computer's name, the system language, locking). Keep-awake is `power.rs`
over `platform::PowerRequest`.

The Mac code guarantees the following:

| Area | Behaviour |
|---|---|
| Video | H.264 Annex B with start codes (converted from VideoToolbox's AVCC), with SPS and PPS before every IDR frame; High profile, no B-frames, at the level `video::size` chooses. Encoder modes, best first: hardware low-latency, hardware, software. The bitrate can change at run time; keyframes are produced only on request (requests within 500 ms share one keyframe). |
| Displays | Changes are reported by Quartz's reconfiguration callback; nothing polls. One display is captured at a time, and an id that is no longer attached falls back to the primary display. Displays are measured in physical pixels in the global desktop space and identified by their UUID. With no capturable display, the host reports `capture{no-display}`, refuses input and stays online. |
| Pointer | With `lc` on, the pointer is reported instead of drawn into the video: its shape (straight RGBA, at most 64 px on a side, on the reliable channel) and its position (at most every 33 ms, on the lossy channel). Setting `DESKWAY_CAPTURE_CURSOR=0` in the launch environment hides the composited cursor. |
| Input (CGEvent) | Coordinates are mapped to the captured display in points; dragged events are generated while a button is held; click state is tracked for double clicks; wheel events use pixel units; key codes come from the shared table; each committed text event carries one Unicode scalar. The host owns the modifier flags it sets on every event. |
| Capture (ScreenCaptureKit) | NV12 video range, BT.709. The settings window appears in the video, while the indicator and border are excluded. Dirty rects show when nothing has changed. |
| Clipboard | `changeCount` is checked when sharing turns on and twice a second while it stays on. The browser's formats are promised through `NSPasteboardItemDataProvider` and marked `org.nspasteboard.TransientType`. |
| Permissions | The panel shows Screen Recording and Accessibility. Desktop control requires both. Terminal and Files use the app's ordinary user rights and macOS privacy permissions. Programs an AI client starts inherit these permissions (security.md, R5). |
| Lock screen | The host runs in the signed-in user's session, including while it is locked. The remote view shows the lock screen, and macOS verifies the password. Control-Command-Q locks the Mac. |
| Files | `host.json`, `host-key`, `controllers.json`, `pairing.json` and `ai.json` are stored in `~/Library/Application Support/Deskway`, with mode 0700. |

To test on a Mac, run `cargo test --locked --release` (covering AVCC to Annex B conversion, the
key table, coordinate mapping and the macOS-only tests). `DESKWAY_TEST_VT_MODES=1 cargo test
--release video_toolbox_outputs_high_profile_idrs_at_non_macroblock_sizes -- --nocapture` tries
all three VideoToolbox modes. Run `tests/e2e/run.mjs` with Chrome's path, adding
`--areas host-restart` for the restart checks. Check the following by hand: sharp video on a
static screen, switching displays, the pointer's shape, the clipboard in both directions,
clicking, dragging and scrolling, ⌘C and ⌘V, text from an input method, held keys being released
on disconnect, reconnecting after network loss, and a long idle soak.

## Web client (`web/static`)

The web client uses native ES modules, with no build step:

```
js/main.js           startup: pages, signaling messages, page events; loads js/test-api.js only
                     under the test flag
js/env.js            storage (local and per tab), device facts (pointer, touch, platform, an
                     installed app)
js/dom.js            element helpers, icons, the spinner
js/dialogs.js        dialogs, one at a time, and the form in a dialog
js/i18n.js           language and catalogs; times, ages, sizes and file names as text
js/net.js            api (a 401 signs out), signaling socket (reconnects itself), ICE servers
js/state.js          application state, views (every change through setView), the current session
js/proto.js          message types and constants (checked against proto/messages.json)
js/auth-messages.js  the strings that are signed, MACed or hashed; pure, checked against
                     proto/vectors.json
js/auth.js           device key, signing, host-key pinning
js/browser-sign-in.js  device key registration for a waiting or signed-in browser
js/pwa.js            the offline shell's worker and the browser's install prompt
js/windows.js        each computer in a window of its own, when the device chose so (?computer=
                     <id>; the windows find each other over a BroadcastChannel)
js/views/            login, setup (one-time account links), list, detail,
                     devices, menu, account (with installing), members (**Members**), relays
                     (**Relays**), computer, vitals (gauges and latency), approvals, pairing, takeover (opening a browser
                     session and mapping end reasons to messages); common.js holds shared pieces
js/session/          connection.js (the session's lifecycle: peer connection, negotiation,
                     state, deadlines, attaching again, the end), session.js (the session's
                     parts put together), desktop.js (picture and controls), terminal-view.js
                     (emulator and layout), terminal-keys.js (keyboard input for the PTY),
                     terminal-channel.js (byte stream and credit), data-view.js (switch between
                     Terminal and Files), files-view.js (browsing and folder operations) and
                     file-listing.js (name and metadata display), file-transfers.js (contents
                     channel and queue), transfer-list.js (list and collision dialog),
                     sha256.js (incremental content hashing), download-file.js (browser save destinations),
                     video, pointer, touch, mouse-pad, keyboard, onscreen-keyboard,
                     keyboard-layout.js (the on-screen US layout), text-box (every input
                     method's text), clipboard
                     (state) and clipboard-ui, transfer, progress
                     (the progress card), floating (dragging and remembered places), toolbar,
                     displays (also their order and the remembered choice for the detail page),
                     pip (picture in picture)
js/test-api.js       a small, explicit surface for the e2e tests (only with a test flag)
sw.js                the offline shell: network first, the kept copy only when the network
                     fails; never the API or the signaling socket
manifest.json, icons/  installable; the icons are host/src/icon.rs's drawing (its test writes
                     and checks them)
design.css           every design token and shared component (docs/design.md); the host builds
                     the same file into its panel, setup window and in-session bar
style.css            the client's page and session layout, in those tokens only
vendor/              the upstream xterm.js modules and stylesheet, loaded on terminal open
```

## Tests

- Unit tests live next to each core module (in Rust: the session state machine, the clipboard
  state machine, key maps; in Go: routing and authorization). The browser's pure modules (input
  messages, view constraints, the session connection with a fake peer, socket and clock), the
  design tokens, and the i18n catalogs and code scan are checked under Node with node:test:
  `node --test "tests/unit/*.test.mjs"`.
- Contract tests check `proto/messages.json`, `proto/version.json` and `proto/vectors.json` in Rust, Go and JS
  (`node --test "tests/contract/*.test.mjs"`).
- The e2e tests are split by area (`tests/e2e/*.test.mjs`) and run by `tests/e2e/run.mjs` on one
  shared harness (`harness.mjs`: the browser, pages, sign-in, real mouse and touch input, and the
  test doubles). Each area names the areas it depends on, and `manage` runs last. Several runs
  can proceed in parallel, because the ports are parameters. Opt-in areas run only when named:
  `clipboard-host` (which uses the real clipboard), `relay` (which uses a fresh account and a
  relay-only host), `first-use` (which runs its own fresh server and Windows hosts), and
  `previous-protocol` (which opens a session on a previous-version host using a fresh account
  on the current server), and `windows-workspace` (which starts an isolated Windows
  foreground host and exercises Terminal, Files and lock).
- Use `--areas relay --relay-tls` with an HTTPS `--url` and start the host with
  `DESKWAY_TEST_RELAY=tls` to exercise TURN over the HTTPS port. Silently drop the host's UDP
  and TCP traffic to the server's `-turn-port` (3478 by default) so both attempts time out.
  For a local run, start the server with `-tls-cert`, `-tls-key`, `-public-ip 127.0.0.1` and
  `-turn-allow-private-for-tests`. Use a certificate for the URL's hostname trusted by the
  browser and host.
- A rule enforced by the server or the host is tested in Go or Rust; e2e tests check only what
  the page shows.
