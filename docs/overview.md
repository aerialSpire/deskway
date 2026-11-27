# Deskway overview

Deskway is a remote desktop that you host yourself. The server runs on a Linux machine, the
computers you want to reach run Deskway Host and stay connected to it. Control Windows and Mac
desktops from a browser on a phone, tablet or another computer. Approved AI clients can also
run commands, work with files and open tunnels on Linux computers.

## Parts

| Part | Runs on | Role |
|---|---|---|
| Server (`deskway-server`) | A Linux server you run | Accounts, device approval, signaling, the TURN relay and the web client, in one Go binary |
| Web client | Any modern browser | Where you sign in, manage devices and control computers. Can be installed as an app (PWA) |
| Deskway Host | Windows, macOS and Linux computers | Desktop access on Windows and macOS. AI workspace on all three |
| `deskway` | The machine running an AI client | Connects MCP clients such as Claude Code to your computers. Windows, macOS and Linux |

Video and input go directly between the browser and the computer over WebRTC when the network
allows. Otherwise they go through the server's TURN relay, which only forwards encrypted traffic.
When connecting or reconnecting, the browser keeps paths the computer reports while its answer is
being verified, so it can try them as soon as that answer is applied.

## Features

### Remote control

- You see the screen and use the mouse and keyboard from any browser. Phones and tablets get
  touch controls: pointer modes, a touchpad, an on-screen keyboard and the device's own input
  method.
- Select **Direct** or **Relay** in the session toolbar to open **Connection details**. The
  **Path** row shows whether the session is direct on the same network, direct over the internet,
  or relayed. Separate rows show available relay addresses and the browser's relay protocol.
- My computers shows the installed host version, CPU, memory, system disk usage and latency to
  the server. Open a computer to see its CPU model and the capacity of each disk.
- With several displays, the browser shows a preview of each and lets you switch between them.
- There are two picture settings. **Sharper** keeps text crisp, and **Smoother** runs at up to
  24 fps.
- On Windows, the computer can be controlled at the lock screen, at the sign-in screen and in UAC
  prompts.
- A Mac can be controlled without a monitor attached, as long as macOS provides a display.
- The clipboard can be shared (text, HTML, pictures and files), and files and images can be sent
  in both directions.
- One person controls a computer at a time. Another of your devices can take over, and the
  first one can take it back.

### Accounts and devices

- A server can be shared by several people. The admin creates their accounts in **Members**, and
  each person sees and controls only their own computers.
- When you sign in to a new browser with a password, or add a computer or AI client, compare the
  confirmation code and approve it from an approved device.
- Each computer also keeps its own list of devices allowed to control it, added at the computer
  by QR code or by a request confirmed there.
- One-time links set up an account and recover access. See [Accounts](accounts.md).

### AI control

- AI clients connect through MCP without a password. You approve each one in the browser.
- AI control is off for every computer until you turn it on. Desktop hosts show when an AI
  client is in control, and every host keeps a record of what the AI did.
- The tools cover the screen, mouse and keyboard, commands and programs, files, the clipboard,
  windows and apps, the accessibility tree, and port tunnels. See [AI clients](ai-clients.md).

### Operation

- The server is one binary with one state file. It obtains its HTTPS certificate from
  Let's Encrypt and includes a TURN relay that limits each account to a share of the bandwidth.
- The interface is available in English, Simplified Chinese and Traditional Chinese, and follows
  the device's language.

## Security

- Each browser, computer and AI client has its own device key. Computers accept only sessions
  signed by a device they trust, and sign their answers. The signatures cover the WebRTC DTLS
  fingerprints, so even the server can't sit in the middle of a session or add a device to a
  computer by itself.
- Video and input are end-to-end encrypted by WebRTC. The relay can't read them.
- The server delivers the web client's code, so whoever controls the server can change what the
  browser runs. Run the server on a machine you trust.

The full model is in [Security](security.md).

## Supported platforms

| Component | Platforms |
|---|---|
| Server | Linux with systemd (x86-64 or ARM64); builds anywhere Go 1.26+ runs |
| Web client | Current Chrome, Edge and Safari, on desktop and mobile. Other browsers with WebRTC and H.264 may work; the automated tests use Chrome |
| Deskway Host | Windows 10 and 11 (x64), macOS 15 or later (Apple silicon and Intel), Debian 12 or Ubuntu 22.04 and later (x86-64 and ARM64, AI workspace) |
| `deskway` (AI clients) | Windows (x64, ARM64); macOS (Apple silicon, Intel); Linux (x86-64, ARM64) |

## Where to go next

- To run your own server, see [Self-hosting](self-hosting.md).
- To install the host, see [Windows](install-host-windows.md), [macOS](install-host-macos.md)
  or [Linux](install-host-linux.md).
- To connect an AI client, see [AI clients](ai-clients.md).
- [Accounts](accounts.md) explains how accounts and approvals work.
- [Architecture](architecture.md), [Protocol](protocol.md) and
  [Session lifecycle](session-lifecycle.md) describe how Deskway is built.
