# AI clients

Deskway lets AI clients that speak the [Model Context Protocol](https://modelcontextprotocol.io)
(Claude Code, Claude Desktop, Cursor and others) use your computers as tools: look at the
screen, use the mouse and keyboard, run commands, read and write files, and open and quit apps.

The AI client talks to a small program, `deskway`, that runs on the AI's machine. `deskway` signs
in to your Deskway server as one of your account's devices and connects to your computers the
same way a browser does.

## Safeguards

An AI client never uses your password. It signs in with its own device key, and you approve it
from an approved browser after comparing a six-digit code. The server accepts new AI sign-ins
only within 10 minutes after you copy the setup prompt.

Approval alone does not give access to any computer. AI control is off for every computer until
you turn on **Allow AI control** for it on the **My devices** page. While an AI client is in
control, your browser sessions show it. On desktop hosts the person at the computer can end it
with **Stop AI control**. Each computer keeps its own record of what AI clients did there,
which you can read in the browser's audit view or with the `audit_read` tool.

Administrator rights are a separate setting, **Allow administrator rights**. On Windows it
requires the signed-in user to be an administrator. On a Mac, each use has to be confirmed at the
Mac. Linux hosts run commands and file operations as the host user and refuse elevation.
On Linux, `run` and `proc_start` with a command use `$SHELL -lc`. An interactive `proc_start`
uses `$SHELL -l`. The default shell is `/bin/sh` when `SHELL` is unset.

## Requirements

- The AI's machine runs Windows (x64 or ARM64), macOS (Apple silicon or Intel), or Linux
  (x64 or ARM64). On Linux, release binaries require glibc 2.35 or later, OpenSSL 3.0, and CA
  certificates. Debian 12 and Ubuntu 22.04 or later provide these. Run `deskway` as your own user.
- The server can provide the `deskway` program for the AI's machine. Released servers fetch the
  programs in the background at startup. The administrator can also supply them as described in
  [Self-hosting, step 5](self-hosting.md#5-ai-client-programs-automatic-for-releases).
- The computers to be controlled run a current version of Deskway Host. A computer with an older
  version shows "This computer's Deskway is too old for AI control".

## Connect an AI client

1. In the browser, open **My devices** and choose **Add an AI client**.
2. Click **Copy prompt** and paste the prompt into your AI client. Copying starts the 10-minute
   window for new AI sign-ins.
3. The AI client follows the prompt on its own. It downloads the `deskway` program for its
   system from your server, checks its SHA-256 hash and runs `deskway login`, which shows six
   digits. It then adds `deskway mcp` as an MCP server.
4. When the request "AI client “…” wants to join" appears in the browser, compare its digits with
   the ones the AI client gave you. Approve the request only if they match.
5. On **My devices**, turn on **Allow AI control** for each computer it may use.

To set up the client by hand instead, open **Connect by hand, without the prompt** in the same
window. It lists the download for each system and the commands described below.

## Manual setup

Download the file for your system from the server (the **Add an AI client** window lists the
links) and verify its SHA-256. Save it in your user folder as `deskway` (`deskway.exe` on
Windows). On macOS and Linux, make it executable with `chmod +x /full/path/to/deskway`.
Copy the setup prompt to open the 10-minute sign-in window, then sign in using the full path:

```sh
/full/path/to/deskway login --server https://desk.example.net --user me --name "Claude Code"
```

Approve the request in the browser if the digits match. Then register the program with your AI
client, using its full path:

```sh
# Claude Code
claude mcp add deskway -- /full/path/to/deskway mcp
```

For other clients, add this to their MCP configuration:

```json
{ "command": "/full/path/to/deskway", "args": ["mcp"] }
```

To confirm the setup, run `deskway computers`. It lists the account's computers and shows which
of them allow AI control.

## Commands

| Command | What it does |
|---|---|
| `deskway login --server URL --user NAME [--name NAME]` | Signs this machine in; a browser approves it |
| `deskway logout [--forget]` | Signs out. `--forget` also deletes the device key |
| `deskway mcp` | The MCP server an AI client starts (stdio) |
| `deskway gateway --listen IP:PORT` | Serves MCP over HTTP on loopback for remote AI clients |
| `deskway gateway --listen unix:PATH` | Serves MCP over HTTP on a private Unix socket on Linux and macOS |
| `deskway computers` | Lists the account's computers and where AI control is allowed |
| `deskway call COMPUTER TOOL [JSON]` | Makes one tool call, as an AI client would |
| `deskway update` | Replaces this program with the server's version now |

By default, AI clients using the same Deskway profile share one proxy and one link to each
computer in use. A gateway started with `--own-proxy` has its own proxy and its own links.

`deskway` keeps its sign-in in `%LOCALAPPDATA%\Deskway\cli` on Windows and
`~/Library/Application Support/Deskway/cli` on macOS. On Linux it uses
`$XDG_STATE_HOME/deskway/cli` when `XDG_STATE_HOME` is absolute, or `~/.local/state/deskway/cli`
otherwise. Set `DESKWAY_CLI_HOME` to use another folder. On Linux the state folder has mode
`0700` and `device.json` holds the unsealed device key with mode `0600`.

## Serve MCP over HTTP

Run the gateway for a remote MCP client that connects over HTTPS. Sign in and approve
`deskway` first, then start the gateway as the same user with the same profile:

```sh
deskway gateway --listen 127.0.0.1:8765
```

The gateway serves Streamable HTTP at `http://127.0.0.1:8765/mcp`. You can also listen on
`[::1]`. Every local process can reach this listener. Put an authenticating HTTPS server in
front of it before offering remote access and cap request bodies at 2 MiB before forwarding them.
Anyone who can reach the gateway can act as that approved Deskway AI client. For a fronting
server that preserves the public Host header, set `--public-host mcp.example.net` (include a
nondefault HTTPS port if used). If a caller sends an Origin header, its origin must match the
request's local HTTP or configured public HTTPS authority.

On Linux and macOS, a supervisor running as the same user can connect over a Unix socket:

```sh
mkdir -m 700 "$HOME/deskway-gateway"
deskway gateway --listen "unix:$HOME/deskway-gateway/mcp.sock" --own-proxy
```

Use an absolute socket path in an existing directory that belongs to the gateway user and has
no group or other permissions. The gateway creates the socket with mode `0600`. On macOS the
directory must also be free of extended access control lists. Send HTTP requests to `/mcp` over
the socket. Unix permissions control access, and the gateway does not check `Host` or `Origin`
on this listener. Windows does not offer the `unix:` form.

With `--own-proxy`, the gateway runs its own proxy in the same process. A supervisor must
spawn it with an stdin pipe and hold the only write end open for as long as it wants the
gateway to run. Closing the pipe or exiting stops the gateway. On Unix, `SIGTERM` and `SIGINT`
also stop it. The gateway removes its socket on orderly exit. Startup fails if the socket path
already exists. After a forced exit such as `SIGKILL`, the supervisor must confirm that the
gateway process has exited and remove the socket file before restarting it.
Use a separate profile if other Deskway clients need their shared proxy at the same time.

The gateway writes one readiness line to stderr when it begins serving HTTP. The form is
`Gateway listening at http://IP:PORT/mcp` for TCP, with the actual bound address and port, or
`Gateway listening at unix:PATH` for a Unix socket. A startup failure exits nonzero with an
error on stderr. If `deskway logout` clears the profile sign-in or the owned proxy cannot renew
it, the gateway releases its links and exits nonzero with an error on stderr. After a forced
exit, an open AI seat clears when the computer detects the lost link.

The gateway serves MCP `2025-06-18` and `2025-11-25` with the initialization handshake and
`2026-07-28` with per-request metadata and `server/discover`. Requests are independent of
the HTTP connection and responses use JSON. The gateway offers `2025-11-25` to clients that
request the unsupported `2025-03-26` revision during initialization. Clients can use the
offered revision if they support it. The gateway supports one approved identity per process.
Use separate `DESKWAY_CLI_HOME` directories and processes for separate identities.

The computer gives up its AI seat after 10 minutes without a tool call, when
the last client of the shared proxy disconnects, or immediately when you call `release`. The
gateway offers the same tool annotations as `deskway mcp`. Use stdio for `copy_to`, `copy_from`,
and `tunnel`, which use paths or ports on the machine running Deskway.

CLI usage and diagnostics, along with the gateway's JSON-RPC error messages and tool errors,
are in English.

### Remote call limits

- `--call-timeout-seconds` defaults to 240 seconds, including the time to connect to the
  computer. The proxy discards calls still queued at the deadline. A call already sent can
  take effect after the timeout. Check the outcome before trying again. The gateway never
  replays a call when the connection closes.
- `--max-result-chars` defaults to 150,000 characters of serialized tool result JSON. The
  minimum is 512 characters to allow room for a limit error. A larger result is refused in
  full. Ask for less where the tool supports it, or ask the gateway operator to raise the
  limit. The link to the computer still has its own 1 MiB message limit.

An HTTP POST can carry up to 2 MiB of JSON, including MCP request metadata. The gateway
handles up to 32 requests at once and returns HTTP `503` when all slots are in use.

All forwarded calls can spend time establishing or recovering a link. This table covers the
tools' own waits and outputs after that connection is ready.

| Tools | Time and result size |
|---|---|
| `computers`, `computer_info` | Directory and computer information return without a requested wait. The result grows with the number of computers or displays. |
| `release` | The proxy releases the link and returns a small acknowledgment. |
| `audit_read` | Reads the bounded audit log. A page can exceed the result limit. Lower `limit` and use `offset` to read on. |
| `screenshot` | Capture can wait up to 15 seconds. Base64 adds about one third to the JPEG size before its JSON fields. A detailed screenshot can exceed the limit. Ask for a smaller `max_width`. |
| `wait_for` | Can wait up to 60 seconds plus one more poll and capture. Returns a small status and timing result. |
| `click`, `move`, `drag`, `scroll`, `key`, `type` | Send input and return small acknowledgments. |
| `run`, `proc_read`, `proc_start` | `run` can wait up to 600 seconds and `proc_read` up to 30 seconds. Use a shorter `wait_ms`. `proc_start` waits briefly for initial output. Any of these can return large output. Redirect large output to a file and use `fs_read` in pieces. |
| `proc_write`, `proc_kill` | Send input to or stop a program. Writing can wait for the program to take input. Stopping waits up to five seconds for exit. Both return small results. |
| `proc_list`, `fs_list` | Program records and directory entries can exceed the result limit. List a smaller folder where possible. |
| `fs_read` | Reads up to 256 KiB per call. Text or base64 can exceed the result limit. Use a smaller `length` and continue from `next`. |
| `fs_stat`, `fs_write`, `fs_hash`, `fs_move`, `fs_delete` | Return small results in normal use. Filesystem time depends on the file, directory, and storage. Hashing reads the whole file and recursive deletion visits the tree. |
| `clipboard_get`, `clipboard_set` | Reading returns the clipboard text, which can exceed the result limit. Writing returns a small acknowledgment. |
| `windows`, `app_list`, `ui_tree` | Window, application and accessibility lists can exceed the limit. For `ui_tree`, ask for fewer levels or nodes. |
| `window_act`, `app_open`, `app_quit`, `ui_act` | Perform an action and return a small result in normal use. `app_quit` can wait up to five seconds. |
| `copy_to`, `copy_from`, `tunnel` | Available through stdio. |

## Updates

`deskway mcp` keeps itself up to date. When it starts, and every hour after that, it compares itself
with the program the server serves and replaces itself if they differ. The new version takes
effect the next time the AI client starts `deskway mcp`. Released servers fetch their own
release's programs automatically at startup. Replace files directly in `clients` only for
programs you supply by hand. Those files take priority over the release downloads.

## Tools

The tools are defined in [`proto/tools.json`](../proto/tools.json). Each computer reports which
of them it supports. Linux hosts offer Computer, Commands and programs, Files, and Network
from the table below. On Linux, `computer_info` reports `os: "linux"`, an empty display list
and `signed_in: true` for the user running the host. The AI client can also use `copy_to`
and `copy_from` with a Linux host.

| Group | Tools |
|---|---|
| Computer | `computer_info`, `audit_read` |
| Screen | `screenshot`, `wait_for` |
| Mouse and keyboard | `click`, `move`, `drag`, `scroll`, `key`, `type` |
| Commands and programs | `run`, `proc_start`, `proc_write`, `proc_read`, `proc_kill`, `proc_list` |
| Files | `fs_list`, `fs_stat`, `fs_read`, `fs_write`, `fs_hash`, `fs_move`, `fs_delete` |
| Clipboard | `clipboard_get`, `clipboard_set` |
| Windows and apps | `windows`, `window_act`, `app_list`, `app_open`, `app_quit` |
| Accessibility tree | `ui_tree`, `ui_act` |
| Network | `tunnel` (reach a port on the computer's loopback from the AI's machine) |

A `proc_read` offset past the end of the output returns empty text and the current end offset.

## Troubleshooting

| Symptom | Check |
|---|---|
| Login is refused right away | Copy the prompt again in **Add an AI client**: new sign-ins are accepted only for 10 minutes after copying |
| The SHA-256 doesn't match | Don't run the file. Download it again from the server; if it still differs, tell the server's administrator |
| `computers` lists a computer but calls are refused | Turn on **Allow AI control** for it on **My devices**, and check the person at the computer didn't stop AI control |
| A tool is reported as unsupported | That computer does not offer the tool. Call `computers` to see which tools it lacks. |
| A file operation is denied | Check the workspace user's permissions on the path. |
