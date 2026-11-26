# Install Deskway Host on Linux

When the Linux computer is online, choose **Terminal** below the desktop notice on its page.
A browser this computer trusts can use the host user's login shell, starting in that user's
home directory. Type with a physical keyboard or choose **Keyboard** for Deskway's on-screen
keyboard. Select **Input method** on that keyboard to use your device's input method.
Paste into the terminal with your browser's Paste command or from the input-method box.
When a shell or terminal application enables bracketed paste, it receives a multiline paste
as one paste. On a Mac controller, Option can type characters from the local keyboard layout.
Disconnecting or losing the session ends the shell and its jobs. Choose **Terminal** again
to start a fresh shell. If another tab still holds the terminal, Deskway asks before taking over.
The host records session start, end and PTY errors in its output, which systemd sends to the journal.
The service runs headless.

To use another browser, sign in there and approve its six-digit request from a browser the
Linux computer already trusts. If you used the super password, choose **Sign out** for
**This device** in **My devices**, then sign in again with the account password to request
approval. See [Adding a device](security.md#adding-a-device).

Approved AI clients can also run commands, work with files and open tunnels. You approve
the computer and manage its AI access in the browser.

## What you need

- Debian 12 or Ubuntu 22.04 or later, on x86-64 (`amd64`) or ARM64 (`arm64`), with systemd.
  The release builds use glibc 2.35 and dynamically link OpenSSL 3.0
  (`libssl.so.3` and `libcrypto.so.3`). Install your distribution's OpenSSL runtime and CA
  certificates.
- A Deskway server address, the account's name and password, and a browser already signed in
  and approved for that account. See [Self-hosting](self-hosting.md) to run your own server.
- Administrator access to install the service and a dedicated ordinary user with a home
  directory to run it. An approved browser or AI client can do what this user can do. Use
  an unprivileged account and grant it sudo rights only if you intend to allow that access.

## 1. Download and verify

Use the tag of a release that contains the Linux host archives on the
[Releases page](https://github.com/aerialSpire/deskway/releases). Run these commands in a
fresh download directory.

```sh
tag=RELEASE_TAG
arch=$(dpkg --print-architecture)
package="deskway-host-linux-$arch"
base="https://github.com/aerialSpire/deskway/releases/download/$tag"
curl -fLO "$base/$package.tar.gz" &&
  curl -fLO "$base/SHA256SUMS" &&
  awk -v file="$package.tar.gz" '$2 == file' SHA256SUMS | sha256sum --check &&
  tar -xzf "$package.tar.gz" &&
  cd "$package"
```

The checksum check must print `OK` for the archive before it is extracted.

## 2. Install and enroll

This example uses the Linux user `deskway-host`. Replace the server address and Deskway account
name with your own. Enter the account password at the prompt.

```sh
sudo useradd --create-home --shell /bin/sh deskway-host
sudo install -m 755 deskway-host /usr/local/bin/deskway-host
sudo -H -u deskway-host /usr/local/bin/deskway-host enroll \
  --server https://desk.example.net --user me --name "Build server"
```

Enrollment saves the computer's sign-in and exits. The host and enrollment use the same state
directory, `$XDG_STATE_HOME/deskway` or `~/.local/state/deskway` under the host user. The commands
here use the home-directory default. If you set `XDG_STATE_HOME`, use the same absolute value
for enrollment, the service and local commands.

## 3. Start and approve

```sh
sed 's/@USER@/deskway-host/' deskway-host.service | sudo tee /etc/systemd/system/deskway-host.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now deskway-host.service
sudo journalctl -u deskway-host.service -f
```

While waiting for approval, the host prints a six-digit code in the journal. Open **My devices**
in your approved browser and approve the computer only if the codes match. Press Ctrl+C to leave
the journal. To run in the foreground instead, stop the service and run `deskway-host` with
no arguments as the host user. The approval code then appears in the terminal.

To let an approved [AI client](ai-clients.md) use the computer, turn on **Allow AI control** for
it in **My devices**.

## Stop AI control locally

Run this as the host user, who can read the state directory and signal the running host.

```sh
sudo -H -u deskway-host /usr/local/bin/deskway-host stop-ai
```

The command ends an active AI session and keeps AI control stopped until an approved browser
turns it on again in **My devices**. It also works while the service is down, using the
computer's saved AI setting. It reports whether AI control stopped or was already inactive.
A failure to confirm the result gives a nonzero exit status.

An AI client that may run commands works as the same user as the host, so this command is a
convenience, not a barrier against a client that is already misbehaving. To cut access off for
certain, stop the service, which also ends everything AI clients started:
`sudo systemctl stop deskway-host.service`.

## Updating

Download and verify the new archive as above. From the extracted directory, run:

```sh
sudo systemctl stop deskway-host.service
sudo install -m 755 deskway-host /usr/local/bin/deskway-host
sed 's/@USER@/deskway-host/' deskway-host.service | sudo tee /etc/systemd/system/deskway-host.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl start deskway-host.service
```

The service starts with the existing sign-in, approvals and AI setting. Stopping the service
ends the commands and programs AI clients started on this computer.

## Uninstalling

Remove the Linux computer in **My devices**, then disable and remove the service.

```sh
sudo systemctl disable --now deskway-host.service
sudo rm /etc/systemd/system/deskway-host.service /usr/local/bin/deskway-host
sudo systemctl daemon-reload
```

To erase the local sign-in and settings too, delete the host user's Deskway state directory.
