# Deskway overview

Deskway is a remote desktop that you host yourself. The server runs on a Linux machine, the
Windows and Mac computers you want to reach run Deskway Host and stay connected to it, and you
control them from a browser on a phone, tablet or another computer. AI clients can use the same
computers as tools once you approve them.

## Parts

| Part | Runs on | Role |
|---|---|---|
| Server (`deskway-server`) | A Linux server you run | Accounts, device approval, signaling, the TURN relay and the web client, in one Go binary |
| Web client | Any modern browser | Where you sign in, manage devices and control computers. Can be installed as an app (PWA) |
| Deskway Host | The computers being controlled | Captures the screen, takes input, transfers files. Windows and macOS |
| `deskway` | The machine running an AI client | Connects MCP clients such as Claude Code to your computers. Windows and macOS |

Video and input go directly between the browser and the computer over WebRTC when the network
allows. Otherwise they go through the server's TURN relay, which only forwards encrypted traffic.

## Features

### Remote control

- You see the screen and use the mouse and keyboard from any browser. Phones and tablets get
  touch controls: pointer modes, a touchpad, an on-screen keyboard and the device's own input
  method.
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
- Every new browser, phone, computer and AI client must be approved from a device that is already
  approved, after you compare a confirmation code. Knowing the password is not enough to sign in.
- Each computer also keeps its own list of devices allowed to control it, added at the computer
  by QR code or by a request confirmed there.
- A super password signs you in when no approved device is at hand.

### AI control

- AI clients connect through MCP without a password. You approve each one in the browser.
- AI control is off for every computer until you turn it on. While an AI client is in control,
  the computer shows it, and the computer keeps a record of what the AI did.
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
| Deskway Host | Windows 10 and 11 (x64); macOS 15 or later (Apple silicon and Intel) |
| `deskway` (AI clients) | Windows (x64, ARM64); macOS (Apple silicon, Intel) |

## Where to go next

- To run your own server, see [Self-hosting](self-hosting.md).
- To install the host, see [Windows](install-host-windows.md) or [macOS](install-host-macos.md).
- To connect an AI client, see [AI clients](ai-clients.md).
- [Accounts](accounts.md) explains how accounts and approvals work.
- [Architecture](architecture.md), [Protocol](protocol.md) and
  [Session lifecycle](session-lifecycle.md) describe how Deskway is built.
