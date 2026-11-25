# Changelog

Notable changes to Deskway are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Released servers fetch and verify missing AI client programs in the background at startup.
- First public release: the Deskway server, web client, Deskway Host for Windows and macOS
  (Apple silicon and Intel), and the `deskway` CLI for AI clients.

### Changed

- Deskway Host and the web client now share a D mark formed by a screen and a circle.
- Signing out of Deskway Host keeps the computer on the account. Signing in again with the same
  account brings it back approved, with its trusted devices and AI setting; only removing it on
  **My devices** takes it off.

### Fixed

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
