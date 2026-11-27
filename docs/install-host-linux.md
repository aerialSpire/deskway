# Install Deskway Host on Linux

When the Linux computer is online, choose **Terminal** or **Files** below the desktop notice
on its page. The session toolbar switches between the two. Files opens the host user's home
directory without starting a shell. Use the path above the entries to go to an ancestor or the
root. Scroll for more entries and use **Refresh** to see changes. The folder menu creates
folders. Each entry's menu can rename or delete it. A folder must be empty to be deleted.
Opening a symbolic link to a folder shows the resolved path and operates on that folder's
entries. Deleting a symbolic link removes the link itself. Files uses the host user's permissions
and includes dotfiles. The host writes each operation's device, path and outcome to its
output, which systemd sends to the journal.

Use **Upload** in the Files bar or drop files into the folder. To download a regular file,
choose **Download** in its entry's menu. Transfers run one at a time. The bottom strip
opens a list of waiting, interrupted, active, completed and failed transfers in both views.
Transfers resume automatically after the session reconnects while the same page is open.
The list marks files that passed SHA-256 verification. Use **Try again** on a failed or cancelled
transfer while its source is available.

Uploads have no Deskway size limit. If the name belongs to a regular file, choose **Skip** or
**Replace** after comparing the two files' sizes and dates. A choice can apply to the remaining
name conflicts in that batch. When replacing a file, the original stays intact until the upload
is complete and verified. If the original changes, Files asks again. Use **Choose** on the
transfer row to reopen the question. Folders and symbolic links are refused.

Where the browser offers a save picker with writable file access, it streams to the selected
file without a Deskway size limit. Cancel is available until the browser starts closing the
file. A file newly created by the picker may remain empty if the transfer is cancelled.
Other browsers keep up to 64 MiB per download in memory. After verification, select **Save**
on the transfer row to hand the file to the browser. Dismissing the row or leaving the session
releases the download from memory.

The host stages uploads beside the destination, using reserved names
`.deskway-upload-<32 lowercase hex digits>.partial`. Files lists live partials and refuses to
rename or delete them. The host removes them on cancellation or when the owning session can no
longer resume. Partials left by a host that stopped unexpectedly are swept after one hour, at
startup in the host user's home and when any folder is listed.
The journal records the device, direction, path, file size, outcome and any replacement once per transfer.

Choose **Terminal** to start or return to the shell.
A browser this computer trusts can use the host user's login shell, starting in that user's
home directory. Type with a physical keyboard or choose **Keyboard** for Deskway's on-screen
keyboard. Select **Input method** on that keyboard to use your device's input method.
Paste into the terminal with your browser's Paste command or from the input-method box.
When a shell or terminal application enables bracketed paste, it receives a multiline paste
as one paste. On a Mac controller, Option can type characters from the local keyboard layout.
**Disconnect** leaves the terminal and its programs running. On the computer's page, choose
**Return** to rejoin the shell and see recent output. If another device or tab holds the
terminal, Deskway asks before taking over the same shell. If older output was discarded,
the terminal displays a notice in your browser's language. Replay restores private modes
such as bracketed paste and the alternate screen. Colours, cursor position for
non-full-screen programs, scroll regions and character sets can be lost when older output
is discarded. Full-screen programs redraw when notified of the terminal size, and shells
redraw their prompts.

In the terminal's Disconnect dialog, choose **Close the terminal and disconnect** to end the
shell and its jobs. If closing fails, Deskway shows an error in the attached terminal.
You can also type `exit`. Deskway closes a terminal left unattended for
24 hours. Stopping, updating or rebooting the host also ends the terminal and its jobs.
The host writes terminal opens, departures, returns and closures to its output, which systemd
sends to the journal.
The service runs headless.

To use another browser, sign in there and approve its six-digit request from a browser the
Linux computer already trusts. See [Adding a device](security.md#adding-a-device).

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

For a [local server](self-hosting.md#local-mode), pass the whole
`https://name.local:port#ca=<64 hex digits>` address to `--server`. Use the fingerprint
from the hub's startup log. Enrollment saves the verified root in the Host's state directory.

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
