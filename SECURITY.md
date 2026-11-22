# Security policy

Deskway controls computers remotely, so a vulnerability in it can give someone else control of
yours. Please report security problems privately, not in a public issue, pull request or
discussion.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting. Open the repository's **Security** tab and choose
**Report a vulnerability**. Only the maintainers can see the report.

Please include:

- the component (server, web client, Deskway Host on Windows or macOS, or the `deskway` CLI) and
  the version or commit;
- what an attacker needs (network position, an account, an approved device, local access);
- steps to reproduce, or a proof of concept;
- the impact as you understand it.

## What to expect

- We aim to acknowledge a report within 7 days and to agree on a disclosure date with you.
- We fix confirmed issues in the latest release and on the `main` branch. Older releases do not
  receive fixes, so self-hosters should upgrade.
- We credit reporters in the release notes, unless you prefer not to be named.

## Scope

These are in scope:

- anything that lets someone control, watch or read data from a computer without the owner's
  approval;
- bypasses of device approval, pairing or the AI control switches;
- ways for the server or the relay to read or alter sessions beyond what
  [the security model](docs/security.md) already states;
- privilege escalation through Deskway Host.

These are out of scope:

- problems that require an already compromised server, browser or computer, except where the
  security model claims protection against them;
- volumetric denial of service;
- missing hardening headers without a demonstrated impact.
