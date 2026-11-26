# Changelog

Notable changes to Deskway are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- The Mac host panel offers optional Full Disk Access to skip folder-by-folder prompts during file transfers and AI file operations.
- Connection details show whether a direct session is local or over the internet and which relays a relayed session uses.
- Admins can check whether their browser can pass traffic through each relay over UDP, TCP, and TLS where available.
- Self-hosted servers can add regional relay nodes from **Relays** in the admin menu with a single command.
- Install the Deskway server on Debian or Ubuntu with one command and upgrade released servers with verified GitHub release assets.
- Self-hosted servers that terminate TLS can relay sessions over the HTTPS port when a network blocks the usual TURN port.
- Released servers fetch and verify missing AI client programs in the background at startup.
- First public release: the Deskway server, web client, Deskway Host for Windows and macOS
  (Apple silicon and Intel), and the `deskway` CLI for AI clients.

### Changed

- Computers measure relay latency so the server can offer them the nearest responding relay for their sessions.
- Keep this computer awake pauses its request on battery below 15% and resumes on external power or at 15% or higher.
- Server upgrades keep the previous host protocol working, show which computers need updates, and include an `upgrade-check` command.
- Deskway Host and the web client now share a D mark formed by a screen and a circle.
- Signing out of Deskway Host keeps the computer on the account. Signing in again with the same
  account brings it back approved, with its trusted devices and AI setting; only removing it on
  **My devices** takes it off.

### Fixed

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
