# Self-hosting the Deskway server

## Local mode

Run the server on a computer on your home or office network. Local mode uses direct
connections on that network, with no domain, public server, STUN or TURN.

```sh
deskway-server -local -listen :18499 -guide-listen :18498 -local-name "Office computer"
```

`-listen` sets the HTTPS address. `-guide-listen` is required and sets the plain HTTP
address for the certificate setup page. Use a LAN address or leave the address before the port
empty to listen on all LAN interfaces. `-local-name` defaults to the computer's hostname and
accepts up to 40 UTF-8 characters. Omit `-domain`, `-tls-cert`, `-tls-key`, `-public-ip`
and `-stun` in local mode.

On a phone, open `http://HOSTNAME.local:18498/` or use the server's LAN IP with the same port.
Compare the CA fingerprint shown by the guide with the one printed by the server. Follow the
guide to install the certificate, then choose **Continue** to open Deskway over HTTPS. Keep
`local-ca/` beside the state file when backing up the server so devices keep trusting it.

Run `deskway-server admin-link -data FILE -url https://HOSTNAME.local:18499` to set up the
admin. Once an admin exists, add `-login` to issue a one-time link that signs in the browser
without changing the password. Open it within two minutes.

## Quick install

On Debian or Ubuntu with systemd, point your domain's DNS A record to the server's public
IPv4 address and [open the firewall ports](#open-the-firewall). With `curl` and
`ca-certificates` installed, run this on the server with your own domain.

```sh
installer=$(curl -fsSL https://github.com/aerialSpire/deskway/releases/latest/download/install-server.sh) &&
  printf '%s\n' "$installer" | sudo sh -s -- --domain desk.example.net
```

The installer looks up the domain's public IPv4 address in DNS for TURN, verifies the release
archive, and checks that the installed service is running. If setup is still pending, it prints a
one-time admin link. Open it within one hour and follow [First sign-in](#7-first-sign-in-and-adding-people). Add
`--version TAG` to select a release by tag. Use the [by-hand route](#install-by-hand) to build
from source or customize the install. To let applications connect directly to your server's
MCP address, see [Remote MCP](#remote-mcp).

The examples use the following values. Replace them with your own:

| Example | Meaning |
|---|---|
| `desk.example.net` | Your domain |
| `203.0.113.10` | The server's public IPv4 address |
| `me` | The first (admin) account's sign-in name |

## 1. What you need

- A Linux server with systemd and a public IPv4 address. The commands below are for Debian 12
  or Ubuntu 22.04 and later, on x86-64 (`amd64`) or ARM (`arm64`). 1 vCPU and 512 MB of RAM are
  enough for a few people. The bandwidth available to the TURN relay matters more than CPU.
- A domain name with a DNS `A` record pointing to that address.
- TCP port 443 reachable directly on the server (not through a load balancer or another
  proxy), so the server can get its Let's Encrypt certificate by itself. If you already run a web
  server on port 443, see [Behind a reverse proxy](#behind-a-reverse-proxy).
- `sudo` access.

### Open the firewall

Allow these inbound ports in your provider's firewall (security group) and in the server's
own firewall, if one is enabled:

| Ports | Used for |
|---|---|
| TCP 443 | HTTPS, signaling, certificate issuance |
| TCP and UDP 3478 | STUN and TURN |
| UDP 49160–49359 | TURN relay traffic |

With `ufw`:

```sh
sudo ufw allow 443/tcp
sudo ufw allow 3478
sudo ufw allow 49160:49359/udp
```

After signing in as admin, open **Relays** and choose **Check**. For the main relay and each
online node, the check reports whether each listener is reachable and which port to open if it
isn't. If at least one listener answers, the check also reports whether this browser passed
traffic through the relay. If traffic did not pass, open the relay's UDP listener port and UDP
relay port range. A network that blocks UDP for this browser gives the same result.

`getent -s dns ahostsv4 desk.example.net.` should show the server's public IPv4 address.

## Install by hand

### 2. Download the server

Download the archive for your architecture and verify it before extracting it.

```sh
sudo apt-get update && sudo apt-get install -y curl ca-certificates

tag=v0.1.0   # the release to run; see https://github.com/aerialSpire/deskway/releases
arch=$(dpkg --print-architecture)   # amd64 or arm64
base="https://github.com/aerialSpire/deskway/releases/download/$tag"
curl -fLO "$base/deskway-server-linux-$arch.tar.gz"
curl -fLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS &&
  tar -xzf "deskway-server-linux-$arch.tar.gz" &&
  cd "deskway-server-linux-$arch"
```

Check for `OK` beside the archive name before extracting it. Continue with step 4.

To run the server in a container instead, see [Running in Docker](#running-in-docker).

### 3. Or build the server from source

If you already downloaded a release, continue with step 4. To build from source, install Git
and [Go 1.26 or later](https://go.dev/dl/).

`go version` should print `go1.26` or later. Then build the server and gather the same files a
release contains:

```sh
git clone https://github.com/aerialSpire/deskway.git deskway
cd deskway
git checkout v0.1.0   # the release you want to run; omit to build the latest main
go build -o bin/deskway-server ./cmd/deskway-server

mkdir -p ~/deskway-server
cp bin/deskway-server deploy/systemd/deskway.service LICENSE ~/deskway-server/
cd ~/deskway-server
```

The web client is embedded in the binary, so there is nothing else to deploy. You can also build
on another computer and copy the binary over. Go cross-compiles without extra tools, so this
works from Windows or macOS too:

```sh
GOOS=linux GOARCH=amd64 go build -o bin/deskway-server ./cmd/deskway-server
```

### 4. Install the server and prepare its state directory

From the folder with `deskway-server`, install the binary and create the system user and state directory.

```sh
sudo install -m 755 deskway-server /usr/local/bin/deskway-server
sudo useradd --system --user-group --home-dir /var/lib/deskway --shell /usr/sbin/nologin deskway
sudo install -d -m 700 -o deskway -g deskway /var/lib/deskway
```

After starting the service, create the admin in [step 7](#7-first-sign-in-and-adding-people).

### 5. AI client programs (automatic for releases)

A released server downloads missing `deskway` programs for Windows x64, macOS and Linux
from its own GitHub release in the background at startup. It verifies each
program's SHA-256 before saving it as `/var/lib/deskway/clients/.release/<file name>`. At startup,
the server replaces cached files whose hashes differ from the release. This needs outbound HTTPS
to GitHub and its release asset host, and write access to the clients folder (or its parent so
the server can create it). Set `-fetch-clients=false` to turn off outbound fetching. The server
can still use cached programs whose hashes match its release.

For a source build, an offline server, or a custom build of a client program, you can place
files directly in `/var/lib/deskway/clients`, the `clients` folder beside the state file.
The `-clients` flag changes this location. Files with these exact names take priority:

| File name | For |
|---|---|
| `deskway-windows-x64.exe` | Windows on x64 |
| `deskway-windows-arm64.exe` | Windows on ARM (build it yourself, see below) |
| `deskway-macos-arm64` | macOS on Apple silicon |
| `deskway-macos-x64` | macOS on Intel |
| `deskway-linux-x64` | Linux on x64 |
| `deskway-linux-arm64` | Linux on ARM64 |

Use the matching release assets for an offline release install. To build the programs yourself,
run `scripts\build-ai-clients.ps1 <folder>` on Windows. It
builds the x64 program, and also the ARM64 one if the `aarch64-pc-windows-msvc` Rust target is
installed. `scripts/build-ai-clients.sh <folder>` builds both macOS programs on a Mac and
builds for the machine's architecture on Linux. On Debian or Ubuntu, install `libssl-dev` and
`pkg-config` to build on Linux. Copy the results into `/var/lib/deskway/clients` with the names
above.

The server reads the folder on each request, so it does not need a restart. When you replace the
files with a newer version, AI clients that are already set up update themselves within an hour
(see [AI clients](ai-clients.md)).

After you sign in (step 7), **Add an AI client** lists the available programs and shows any
download, verification or storage errors. The server logs fetch failures and retries them
in the background after a one-minute delay. Reopen the page to see the latest status.

### 6. Start the service

The folder from step 2 or 3 holds a hardened systemd unit template, `deskway.service`. From that
folder, fill in your domain and IP address and install it:

```sh
deskway_domain=desk.example.net
deskway_public_ip=203.0.113.10

sed -e "s/@DOMAIN@/$deskway_domain/g" -e "s/@IP@/$deskway_public_ip/g" deskway.service \
  | sudo tee /etc/systemd/system/deskway.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now deskway
```

The unit runs the server as the `deskway` user on port 443. `-domain` obtains a Let's Encrypt
certificate automatically and caches it in `/var/lib/deskway/certs`, and `-public-ip` turns on the
built-in TURN relay.
With both enabled, TCP 443 also carries TURN over TLS for networks that block the usual TURN
port. Keep the same firewall ports open.

`sudo systemctl status deskway` should show `active (running)`, and
`sudo journalctl -u deskway -n 20 --no-pager` should include `TURN relay on port 3478` and
`listening on :443`. `https://desk.example.net` should now show the sign-in page. The first load
can take a few seconds while the certificate is issued.

## 7. First sign-in and adding people

Open the one-time link from the installer. If you installed by hand or need a new link, run
this command as the service user while Deskway is running. This gives the service access to
the link and lock files the command creates.

```sh
sudo -u deskway /usr/local/bin/deskway-server admin-link -data /var/lib/deskway/state.json -url https://desk.example.net
```

Pass the public HTTPS origin in `-url`, including any non-default port. Use the same `-data`
path as the service. The link works once and expires after one hour. When there is no admin,
the page asks you to choose the admin’s sign-in name (for example, `me`) and password. When
there is an admin, it resets that account’s password. In either case, the browser you used is
approved and signed in.

To give someone an account, open the menu under your name, choose **Members**, then **Create
user**. Copy the message with the invitation link and account name and send it privately. The
member has seven days to choose a password and sign in through the link. Password sign-in on
a new browser requires approval from an approved device. See [Accounts](accounts.md).

## 8. Add your first computer

Install Deskway Host on the computer you want to control and sign it in to
`https://desk.example.net` with the account that owns it:

- Windows: [Install Deskway Host on Windows](install-host-windows.md)
- macOS: [Install Deskway Host on macOS](install-host-macos.md)

An account can have at most 10 computers, including those still waiting for approval.

## 9. Verify from another network

On a phone, turn off Wi-Fi and open `https://desk.example.net` over mobile data. Sign in, approve
the phone from your first browser when asked, then open the computer and check that you can see
and control it. The session toolbar shows whether the connection is direct or goes through the
relay. If the session works over mobile data, the TURN relay is working.

## Relay nodes

Run relay nodes in regions near your computers to shorten the path when sessions need TURN.
Accounts, signaling, and the website stay on the main server. A relay node forwards
encrypted session traffic and needs a reachable public IPv4 address. Only the main server
needs a domain and HTTPS certificate. The fallback on TCP 443 stays on the main server.

Open the following inbound ports on each node and allow outbound HTTPS to the main server.

| Ports | Used for |
|---|---|
| TCP and UDP 3478 | STUN and TURN |
| UDP 49160–49359 | TURN relay traffic |

Put the same `deskway-server` binary on the node. Sign in as the admin over HTTPS, open
**Relays** from your account menu, and choose **Add a relay**. Run the generated command on the
node within 10 minutes. Each token works once.

```sh
deskway-server relay -join https://desk.example.net -token TOKEN
```

The main server can infer the relay's address when it sees a public IPv4 address on the join
request. If the main server is behind a reverse proxy or the relay uses a different address for
inbound traffic, pass `-public-ip` with the relay's public IPv4 address. Use `-turn-port`,
`-turn-min-port`, and `-turn-max-port` to change the ports, and open the matching ports on the node.

The node keeps its identity, secret, and the per-account limit it received at enrollment in a
private `relay.json` in `-data` (default `deskway-relay`). Restart it with the same directory and
port settings. You can omit `-token` after the first run. To change the advertised address or
TURN port, enroll again with a new token and data directory and remove the old entry.

For systemd, use the main unit template on the node. Enroll once as the `deskway` user with
`-data /var/lib/deskway`, stop the foreground process, then replace the unit's `ExecStart` line.

```ini
ExecStart=/usr/local/bin/deskway-server relay -join https://desk.example.net -data /var/lib/deskway
```

Keep any custom TURN port and range flags in the unit. On **Relays**, use **Refresh** to see
the latest online or last-seen status. The server offers each computer the relay with the
shortest usable round-trip measurement, falling back to the main relay if configured or the
first online node otherwise. The connection decides whether to use a relay and prefers a
direct path. Removing a node stops the main server from offering it and renewing its credentials.
Stop the process on the node to end any remaining traffic.

## Running in Docker

The server is also published as a container image, `ghcr.io/aerialspire/deskway-server`, for
`linux/amd64` and `linux/arm64`. It holds only the server binary, runs as an unprivileged user
(uid 65532) and keeps its data in `/data`. Use it instead of steps 2 to 6; the firewall (step 1)
and everything from step 7 on stay the same.

Use the host's network. The TURN relay needs UDP 3478 and the whole range 49160–49359, and it
must see the server's real public address; mapping two hundred ports through Docker's proxy is
slow and fragile.

```sh
tag=0.1.0   # the release version, without the leading "v"
image=ghcr.io/aerialspire/deskway-server:$tag

sudo install -d -m 700 -o 65532 -g 65532 /var/lib/deskway
sudo docker run -d --name deskway --restart unless-stopped --network host \
  -v /var/lib/deskway:/data "$image" \
  -data /data/state.json -listen :443 -domain desk.example.net -public-ip 203.0.113.10
```

The container can bind port 443 as an unprivileged user. `sudo docker logs deskway` shows the
server log. Run the link command inside the running container, then open the link as in step 7.

```sh
sudo docker exec deskway /usr/local/bin/deskway-server admin-link -data /data/state.json -url https://desk.example.net
```

The image fetches AI client programs at startup and caches them in `/data/clients/.release`
on the mounted volume. For offline deployments, append `-fetch-clients=false` to the server
arguments above. You can put programs in `/var/lib/deskway/clients` on the host as described
in step 5. They must be readable by uid 65532.

To upgrade, pull the new version, then stop the container and back up its data:

```sh
sudo docker stop deskway && sudo docker rm deskway
sudo cp -a /var/lib/deskway /var/lib/deskway.bak-$(date +%F)
```

To switch to automatic fetching, move any manually placed programs out of the host's `clients`
folder before starting the new container. Programs left there take priority. Start the new
container with the same `docker run` command and the new tag.

## Server options

Set these flags on the `ExecStart` line of the unit.

| Flag | Default | Meaning |
|---|---|---|
| `-listen` | `127.0.0.1:8080` | Address to listen on. Use `:443` with `-domain` or `-tls-cert` |
| `-data` | `deskway-data/state.json` | State file. The `certs`, `local-ca` and `clients` folders sit beside it when used |
| `-local` | off | Run [local mode](#local-mode) with a local CA and mDNS discovery |
| `-guide-listen` | | Plain HTTP address for the certificate guide in local mode |
| `-local-name` | hostname | Name advertised in local mode, up to 40 UTF-8 characters |
| `-domain` | | Get a Let's Encrypt certificate for this domain automatically, through port 443 (port 80 can stay closed) |
| `-tls-cert`, `-tls-key` | | Use your own certificate and key files instead of `-domain` |
| `-public-host` | | Public hostname for TURN over TLS with your own certificate, when it has no single concrete DNS name |
| `-public-ip` | | The main server's public IPv4 address. Enables its built-in TURN relay |
| `-turn-port` | `3478` | TURN port, UDP and TCP |
| `-turn-min-port`, `-turn-max-port` | `49160`, `49359` | Relay port range. Open the same range in the firewall |
| `-relay-account-mbps` | `30` | Each account's share of bandwidth per relay, both directions together. New nodes receive this value when they join |
| `-stun` | | Comma-separated STUN URLs to hand out when TURN is off |
| `-clients` | `clients` beside the state file | Folder of AI client programs |
| `-fetch-clients` | `true` | Fetch missing AI client programs from the server's GitHub release. Use `-fetch-clients=false` to disable outbound fetching |
| `-remote-mcp` | off | Let approved remote applications connect over HTTPS at `/mcp` |
| `-mcp-public-url` | inferred from HTTPS configuration where possible | Public HTTPS origin for remote MCP, for example `https://desk.example.net` |

### Remote MCP

Remote MCP is off by default. It requires a Linux x64 or ARM64 server and a public HTTPS
address. The published server container cannot run the required AI client program because
it lacks the program's shared libraries. Use a native Linux install.

Install the matching current Deskway AI client from the same release as your server,
using the [file names and download directory in step 5](#5-ai-client-programs-automatic-for-releases).
Verify the file against the release's `SHA256SUMS` and install it with mode `755`. The program
needs glibc 2.35 or later, OpenSSL 3 (`libssl3` on Debian 12 and Ubuntu 22.04), and the system
CA certificates. Keep this executable current when you upgrade the server.

- **With the installer**, place the executable directly in `/var/lib/deskway/clients` before
  running the [quick install command](#quick-install) with `--remote-mcp`. For example, on x64,
  run `sudo install -D -m 755 deskway-linux-x64 /var/lib/deskway/clients/deskway-linux-x64`.
  Use the same option and domain to enable it on an existing installer-managed server, or
  use `--no-remote-mcp` to turn it off. Rerunning with neither option and running
  `deskway-server upgrade` both keep the setting. The installer can toggle only the
  shipped `ExecStart` command with its original spacing and optional `-remote-mcp` flag.
  Use the by-hand steps below for a customized command or an `ExecStart` drop-in.
  The new server executable checks the AI client program before the installer enables MCP.
  If the service fails to start after a toggle, the installer shows the recent journal,
  restores the previous service unit, and restarts it.
- **By hand**, add `-remote-mcp` to the unit's `ExecStart` line, then run
  `sudo systemctl daemon-reload` and `sudo systemctl restart deskway`. Remove the flag and repeat
  those two commands to turn it off. To check the AI client program beforehand, run
  `deskway-server remote-mcp-check -data /var/lib/deskway/state.json` as the service user.
  Pass the service's `-clients` directory if it sets one. The check uses programs already on
  disk and can run while the service is active.

If a reverse proxy terminates HTTPS, or the server cannot infer its public origin from its
domain or certificate, set `-mcp-public-url https://desk.example.net` on the server command.
Forward `/mcp`, `/.well-known/oauth-*`, `/oauth/` and the usual web client routes, including
`/api/mcp/approval`, to Deskway. Preserve the `Authorization` and MCP headers and set
`X-Forwarded-Proto: https`. The server's AI client must be able to reach and verify that public
HTTPS origin for sign-in and signaling.

The server holds keys for approved remote applications in the `mcp/` folder beside the state
file. Turning remote MCP off takes its routes offline and keeps the approved applications'
records and profiles. Remove an application from **My devices** to end all its sign-ins. See
[Security](security.md#remote-applications-through-the-deskway-server).

Copy the server's MCP address from **Add an AI client** in the web client. From a Deskway
checkout, run this on the same machine as the browser, using Node.js 20 or later and
substituting the copied address.

```sh
node scripts/check-mcp.mjs https://desk.example.net/mcp
```

Open the authorization URL it prints in a trusted browser and approve **Deskway MCP check**.
The check reports discovery, authorization, initialization, the tools and the computers
visible to the application. Add `--refresh` to check rotation and refusal of the old refresh
token. Reusing the old token invalidates that sign-in. When finished, remove **Deskway MCP check**
in **My devices**. Give the same HTTPS address to your application as described in
[AI clients](ai-clients.md#connect-a-remote-application).

The server allows 16 pending authorizations per address and 256 in total, 32 unused client
registrations per address and 1,024 registrations in total, and 32 grants per account.
A registration with no live grant expires after 24 hours without use. The MCP endpoint allows
240 requests per minute per authenticated grant and per address for unauthenticated callers.

### Your own certificate

Replace `-domain @DOMAIN@` in the unit with `-tls-cert /etc/deskway/fullchain.pem -tls-key
/etc/deskway/privkey.pem`. Both files must be readable by the `deskway` user (for example owner
`root`, group `deskway`, mode `640`). `ProtectSystem=strict` keeps `/etc` readable. When
`-public-ip` enables the relay, Deskway infers the TURN hostname when the certificate lists
one DNS name without a wildcard. For a certificate with several DNS names or a wildcard, set
`-public-host` to a concrete public hostname that the certificate covers, such as
`desk.example.net` for `*.example.net`. The server reads the certificate at startup, so run
`sudo systemctl restart deskway` after renewing it.

### Behind a reverse proxy

Running Deskway directly on port 443 is recommended. If another web server already uses port 443,
Deskway can run behind it, with the following limitations:

- The proxy must pass WebSocket upgrades and set `X-Forwarded-Proto: https`.
- Deskway sees every request as coming from the proxy. Approval prompts show the proxy's address
  instead of the device's, and the limit on failed password attempts applies to everyone
  together, so repeated wrong passwords from one person can lock others out for a while.
- MCP limits based on caller addresses use the proxy's socket address because Deskway does
  not use forwarded client address headers.
- With the plain HTTP listener behind the proxy, the relay still uses ports 3478 and 49160–49359
  directly. TURN over TLS on the HTTPS port requires Deskway to terminate TLS.

Caddy meets both proxy requirements by default. A minimal Caddyfile looks like this:

```
desk.example.net {
    reverse_proxy 127.0.0.1:8080
}
```

In the Deskway unit, replace `-listen :443 -domain @DOMAIN@` with `-listen 127.0.0.1:8080`, and
remove the `AmbientCapabilities=` and `CapabilityBoundingSet=` lines. They exist only to let the
server bind port 443.

## Upgrading

Read the release notes and [back up the server's data](#backup-and-restore) first. If the
currently installed binary has `upgrade-check`, run it to check protocol readiness. It reads
the last saved state while the service is running.

```sh
sudo -u deskway /usr/local/bin/deskway-server upgrade-check -data /var/lib/deskway/state.json
```

Exit status 0 means all approved computers report the current protocol or newer. Exit status 1
flags `previous`, `older` and `unknown` computers. Bring computers with an unknown version
online, update computers flagged by the check, and run it again before moving to the next
protocol version. Computers marked `pending` do not affect readiness. Exit status 2 means the
check failed.

### Automated upgrade

For systemd installations, automated upgrades are available starting with the first released
binary that includes `upgrade`. If you are on an earlier release or a source build,
[upgrade by hand](#upgrade-by-hand) once to move to a supported release.

Rerun the installer to verify and replace the executable, enable the service, and restart it.
Add `--version TAG` to pick a release.

```sh
installer=$(curl -fsSL https://github.com/aerialSpire/deskway/releases/latest/download/install-server.sh) &&
  printf '%s\n' "$installer" | sudo sh -s -- --domain desk.example.net
```

You can also replace the executable and restart the service separately.

```sh
sudo /usr/local/bin/deskway-server upgrade && sudo systemctl restart deskway
```

Use `deskway-server upgrade -to TAG` to select a release.

### Upgrade by hand

Use step 2 to get the new release or step 3 to continue building from source. Stop the
service, install the binary as in step 4, then start the service again.

### After upgrading

Update Deskway Host on each computer. On Windows, run the new
`DeskwayHost-Setup-windows-x64.exe` and choose **Update**. On macOS, quit Deskway Host and
replace `Deskway.app` in Applications. See the [compatibility window](protocol.md#versions-and-capabilities).

The released server fetches the new release's AI client programs at startup. Update any
manually supplied programs you want to keep serving. AI clients check for updates within an
hour. Browsers pick up the new web client and show “A new version of Deskway is ready”.

## Backup and restore

Everything the service keeps is in `/var/lib/deskway`: `state.json` (accounts, devices, approvals),
`state.json.links` (account links), `certs/`, `clients/` and, with remote MCP, `mcp/`
(approved applications' AI profiles and device keys). For a consistent copy, stop the service and issue links only after it is running again.

```sh
sudo systemctl stop deskway
sudo tar -C /var/lib -czf deskway-backup.tar.gz deskway
sudo systemctl start deskway
```

To restore, stop the service, put the folder back with owner `deskway` and mode `700`, and start
the service again.

## Troubleshooting

Read the log with `sudo journalctl -u deskway -n 100 --no-pager`.

| Symptom | Check |
|---|---|
| The HTTPS page doesn't load | The DNS record points to this server; TCP 443 is open in both firewalls; nothing else listens on 443 (`sudo ss -ltnp 'sport = :443'`); the log shows no certificate errors |
| Certificate errors in the log | Port 443 must reach this server directly. Let's Encrypt rate-limits repeated failures; fix the cause and wait before retrying |
| The admin link expired or was used | Issue a new link as described in step 7 |
| Sessions work on the same network but not from mobile data | `-public-ip` is the server's public address; TCP/UDP 3478 and UDP 49160–49359 are open in both firewalls |
| Can sign in but cannot connect on a network that only allows TCP 443 | Enable `-public-ip` and terminate TLS in Deskway on port 443. Check that the log shows `TURN over TLS at` with the public hostname and that the browser and host trust its certificate |
| A computer stays "Waiting for approval on a signed-in device" | Approve it from an approved browser, on **My devices** |
| **Add an AI client** says the server has no programs | Check the reason on the page and the server log. For manually supplied programs, check the exact names in step 5 |
| The browser shows "This computer needs an update" | Update Deskway Host on that computer to the server's version |
