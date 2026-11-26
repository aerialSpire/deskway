# Changelog

Notable changes to Deskway are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Enable remote MCP with the server installer, verify it with a standalone check, and connect applications by HTTPS with browser approval and removal in My devices.

- Serve a Deskway AI profile over MCP Streamable HTTP with `deskway gateway`, behind an authenticating HTTPS server.
  Gateway error messages are in English.
- Open a Linux, macOS or Windows computer’s terminal from the browser, with the same on-screen keyboard and takeover controls as a desktop session.
- Browse a Linux, macOS or Windows computer’s files, create folders, rename entries and delete files or empty folders from the browser.
- Upload, replace and download files from the Files view on Linux, macOS and Windows, resume interrupted transfers, and track verified results from Files or Terminal.
- Ask again when an upload’s replacement target changes, offer Save for verified in-memory downloads, and restore an open terminal when the page reconnects from Files.
- Run a supervised MCP gateway with a private Unix socket on Linux and macOS and a proxy that stops with the gateway.
- Open Terminal and Files on macOS before granting Screen Recording or Accessibility.
- The Mac host panel offers optional Full Disk Access to skip folder-by-folder prompts during file transfers and AI file operations.
- Install Deskway Host as a Linux service on x86-64 or ARM64 for AI commands, files and tunnels, and stop AI control locally with `deskway-host stop-ai`.
- AI applications on Linux x64 and ARM64 can connect to Deskway computers with `deskway`.
- Connection details show whether a direct session is local or over the internet and which relays a relayed session uses.
- Admins can check whether their browser can pass traffic through each relay over UDP, TCP, and TLS where available.
- Self-hosted servers can add regional relay nodes from **Relays** in the admin menu with a single command.
- Install the Deskway server on Debian or Ubuntu with one command and upgrade released servers with verified GitHub release assets.
- Self-hosted servers that terminate TLS can relay sessions over the HTTPS port when a network blocks the usual TURN port.
- Released servers fetch and verify missing AI client programs in the background at startup.
- First public release: the Deskway server, web client, Deskway Host for Windows and macOS
  (Apple silicon and Intel), and the `deskway` CLI for AI clients.

### Changed

- Browser terminals on Linux, macOS and Windows keep running after disconnect, with recent output available on return.
- Locking a macOS or Windows computer ends browser access to Terminal and Files. Kept terminals continue running, and interrupted transfers can resume after unlock.
- Computers measure relay latency so the server can offer them the nearest responding relay for their sessions.
- Keep this computer awake pauses its request on battery below 15% and resumes on external power or at 15% or higher.
- Server upgrades keep the previous host protocol working, show which computers need updates, and include an `upgrade-check` command.
- Deskway Host and the web client now share a D mark formed by a screen and a circle.
- Signing out of Deskway Host keeps the computer on the account. Signing in again with the same
  account brings it back approved, with its trusted devices and AI setting; only removing it on
  **My devices** takes it off.

### Fixed

- Files keeps operating on the folder it showed when a symbolic link is retargeted, and preserves spaces in file names.
- The MCP gateway rejects oversized bodies and excess concurrent requests, and the profile proxy discards queued calls after their gateway deadline.
- Paste large or multiline text into a browser terminal, with bracketed paste where the application and PTY support it.
- When a Linux computer refuses a terminal connection from an untrusted browser, Deskway explains how to get approval from a trusted device.
- Reopen the browser terminal from the computer page immediately after disconnecting.
- The computer page groups **Terminal** and **Files** in one action row, and terminal typing preserves spaces and capital letters.
- Linux and macOS terminals can close while other commands are still running, and reading past command output returns an empty result.
- Linux hosts hide typed enrollment passwords, show approval codes on restart while waiting, and prevent two launches from sharing a state directory. The web client shows when a computer has no desktop, and AI clients explain when a tool is unavailable.
- Browser sessions keep the computer's first connection paths while verifying its answer.
- Computers fall back to the first online relay node when no latency measurement is usable and the main relay is disabled.
- Relay setup explains when to pass `-public-ip`, supports copying the join command by hand, and keeps relay authentication separate from sign-in limits.
- The server installer uses DNS for the public TURN address, enables the service on reruns, and shows recent journal entries if startup fails.
- Sessions can start over the HTTPS relay while attempts to reach the usual TURN port are still pending.
- Server upgrades replace outdated cached AI client programs in place.
- Speed up AI client program listings and downloads by reusing SHA-256 hashes for unchanged files.
- Connect to the first computer you add without needing to request control, and turn on AI for it from the same browser.
- Keep a complete picture during scrolling and window changes on slow connections.
- Reduce the delay before clicks and keystrokes become visible when video backs up.

- Restore the **Request control** and **Take over** prompts after a full-screen connection attempt ends.
- Deskway Host reconnects by itself when its connection to the server dies without notice (for
  example a router dropping the mapping), instead of staying offline until it is signed in again.
- A computer waiting for approval shows a six-digit code, and the approval shows the same code, so
  a computer signed in by someone else under the same name can be told apart.
- **My devices** says why a computer did not take the AI setting (it does not trust the device
  that signed it yet), and the computer takes it by itself once it does.
