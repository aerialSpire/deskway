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
| `deskway computers` | Lists the account's computers and where AI control is allowed |
| `deskway call COMPUTER TOOL [JSON]` | Makes one tool call, as an AI client would |
| `deskway update` | Replaces this program with the server's version now |

Several AI client windows on one machine share one connection through a local proxy that
`deskway mcp` starts and stops by itself.

`deskway` keeps its sign-in in `%LOCALAPPDATA%\Deskway\cli` on Windows and
`~/Library/Application Support/Deskway/cli` on macOS. On Linux it uses
`$XDG_STATE_HOME/deskway/cli` when `XDG_STATE_HOME` is absolute, or `~/.local/state/deskway/cli`
otherwise. Set `DESKWAY_CLI_HOME` to use another folder. On Linux the state folder has mode
`0700` and `device.json` holds the unsealed device key with mode `0600`.

## Updates

`deskway` keeps itself up to date. When it starts, and every hour after that, it compares itself
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
