# Host resource use

What the host costs on each system, measured on 2026-11-27 at commit `c3c6867` (release
builds). Only the host is measured: the server and the browser ran beside it and are not
counted.

## How it was measured

Each host ran in the foreground with a profile and home directory of its own, enrolled to a
server on the same computer, and a headless Chrome on a Windows computer drove one session
through fixed phases:

1. online and idle, 60 s;
2. the desktop, Sharp picture (2 frames a second asked), 60 s, then Smooth (24), 60 s, in a
   1440×900 window; the screen itself was mostly still;
3. 30 s idle after leaving the desktop;
4. the terminal printing 400,000 lines as fast as the shell can, then 20 s idle;
5. Files: upload a 32 MiB file, then download it (SHA-256 checked on arrival);
6. 60 s idle after leaving.

Once a second a sampler read the host's memory and total CPU time. CPU is in percent of one
core, averaged over the phase. On Windows the figures are the sum of every host process (the
network process, the desktop agent and the file worker); macOS and Linux run one process.

| | Windows | macOS | Linux |
|---|---|---|---|
| Computer | Windows 11, AMD GPU (hardware H.264) | Mac mini, Apple M1, 16 GB, macOS 27 | WSL 2 Ubuntu 24.04 on the Windows computer |
| Memory figure | working set (private bytes) | resident (physical footprint) | resident |

## Results

| Phase | Windows | macOS | Linux (no desktop) |
|---|---|---|---|
| Idle, online | 43 MB (12 MB), 0.8 % | 74 MB (15 MB), 0.4 % | 8 MB, 0.2 % |
| Desktop, Sharp | 172 MB (140 MB), 18 % | 88 MB (22 MB), 6 % | — |
| Desktop, Smooth | 177 MB (145 MB), 23 % | 89 MB (22 MB), 6 % | — |
| After the desktop | 69 MB (24 MB), 0.3 % | 91 MB (17 MB), 1.6 % | — |
| Terminal, heavy output | 51 MB (13 MB), 17 % | 92 MB (17 MB), 7 % | 15 MB, 27 % (peaks above 100 %) |
| Upload 32 MiB | 58 MB (11 MB), 22 % | 92 MB (17 MB), 42 % | 14 MB, 48 % |
| Download 32 MiB | 46 MB (10 MB), 20 % | 92 MB (17 MB), 23 % | 14 MB, 40 % |
| Idle after | 55 MB (15 MB), 0.9 % | 92 MB (17 MB), 0.4 % | 14 MB, 0.2 % |

Memory returned to its idle level after each desktop session; nothing grew across phases.
The terminal phase measures how fast each shell prints: PowerShell took 109 s for the
400,000 lines, zsh 23 s and bash 30 s.

## Known

- Transfer times are left out: they follow the network path between the browser and the
  computer, not the host.
- The desktop figures are for a still screen; a changing picture costs more to encode.
- On macOS a second, installed host ran on the same Mac and is not included.
