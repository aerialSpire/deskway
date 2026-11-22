# Self-hosting the Deskway server

These steps set up Deskway on a fresh Linux server. When you finish, your browsers, your
computers (Deskway Host) and your AI clients all use one HTTPS address. Work through the steps in
order. Most of them end with a way to confirm that the step worked.

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

`dig +short desk.example.net` should print the server's public IP address.

## 2. Install Git and Go

Building the server requires Go 1.26 or later, and the latest stable release is recommended. The
Debian and Ubuntu packages are usually older, so install Go from [go.dev/dl](https://go.dev/dl/):

```sh
sudo apt-get update && sudo apt-get install -y git curl ca-certificates

go_version=1.27.4   # any release from 1.26 on; the newest is listed at https://go.dev/dl/
arch=$(dpkg --print-architecture)   # amd64 or arm64
curl -fsSLO "https://go.dev/dl/go${go_version}.linux-${arch}.tar.gz"
sudo rm -rf /usr/local/go
sudo tar -C /usr/local -xzf "go${go_version}.linux-${arch}.tar.gz"
rm "go${go_version}.linux-${arch}.tar.gz"
echo 'export PATH=$PATH:/usr/local/go/bin' >> ~/.profile
export PATH=$PATH:/usr/local/go/bin
```

`go version` should print `go1.26` or later.

## 3. Build the server

```sh
git clone https://github.com/aerialSpire/deskway.git deskway
cd deskway
git checkout v0.1.0   # the release you want to run; omit to build the latest main
go build -o bin/deskway-server ./cmd/deskway-server
```

The web client is embedded in the binary, so there is nothing else to deploy.

You can also build on another computer and copy the binary over. Go cross-compiles without extra
tools, so this works from Windows or macOS too:

```sh
GOOS=linux GOARCH=amd64 go build -o bin/deskway-server ./cmd/deskway-server
```

`bin/deskway-server -h` should list the server's flags.

## 4. Install it and create the admin account

```sh
sudo install -m 755 bin/deskway-server /usr/local/bin/deskway-server

sudo useradd --system --user-group --home-dir /var/lib/deskway --shell /usr/sbin/nologin deskway
sudo install -d -m 700 -o deskway -g deskway /var/lib/deskway

sudo -u deskway /usr/local/bin/deskway-server admin me -data /var/lib/deskway/state.json
```

`admin` creates the server's first account and prints its initial password. The password is
shown only once, so save it. The command works only while no account exists and the server is
stopped, because it needs exclusive access to the state file. Accounts for other people are
created later on the website.

`sudo ls /var/lib/deskway` should now show `state.json`.

## 5. Put the AI client programs in place (optional)

You need this step only if you will connect AI clients such as Claude Code, and you can do it at
any time.

The server hands out the `deskway` command-line program to AI clients from
`/var/lib/deskway/clients`, the `clients` folder beside the state file. The `-clients` flag
changes this location. The server lists the files in that folder that have one of these exact
names, each with its SHA-256 hash:

| File name | For |
|---|---|
| `deskway-windows-x64.exe` | Windows on x64 |
| `deskway-windows-arm64.exe` | Windows on ARM (build it yourself, see below) |
| `deskway-macos-arm64` | macOS on Apple silicon |
| `deskway-macos-x64` | macOS on Intel |

Each GitHub release publishes the Windows x64 and macOS files under these names, so you can
download them into the folder unchanged. Use the release tag of the server you built:

```sh
tag=v0.1.0   # the release you are running
sudo install -d -m 755 -o deskway -g deskway /var/lib/deskway/clients
for f in deskway-windows-x64.exe deskway-macos-arm64 deskway-macos-x64; do
  sudo -u deskway curl -fL -o "/var/lib/deskway/clients/$f" \
    "https://github.com/aerialSpire/deskway/releases/download/$tag/$f"
done
```

To build them from source instead, run `scripts\build-ai-clients.ps1 <folder>` on Windows. It
builds the x64 program, and also the ARM64 one if the `aarch64-pc-windows-msvc` Rust target is
installed. On a Mac, `scripts/build-ai-clients.sh <folder>` builds both macOS programs. Copy the
results into `/var/lib/deskway/clients` with the names above.

The server reads the folder on each request, so it does not need a restart. When you replace the
files with a newer version, AI clients that are already set up update themselves within an hour
(see [AI clients](ai-clients.md)).

After you sign in (step 7), the **Add an AI client** window on the website should list a download
for each file. If it shows "The server has no AI client programs yet", the folder is empty or the
file names don't match.

## 6. Start the service

The repository includes a hardened systemd unit template, `deploy/systemd/deskway.service`. Fill
in your domain and IP address and install it:

```sh
deskway_domain=desk.example.net
deskway_public_ip=203.0.113.10

sed -e "s/@DOMAIN@/$deskway_domain/g" -e "s/@IP@/$deskway_public_ip/g" deploy/systemd/deskway.service \
  | sudo tee /etc/systemd/system/deskway.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now deskway
```

The unit runs the server as the `deskway` user on port 443. `-domain` obtains a Let's Encrypt
certificate automatically and caches it in `/var/lib/deskway/certs`, and `-public-ip` turns on the
built-in TURN relay.

`sudo systemctl status deskway` should show `active (running)`, and
`sudo journalctl -u deskway -n 20 --no-pager` should include `TURN relay on port 3478` and
`listening on :443`. `https://desk.example.net` should now show the sign-in page. The first load
can take a few seconds while the certificate is issued.

## 7. First sign-in and adding people

1. Open `https://desk.example.net` and sign in as `me` with the initial password. This first
   browser does not need approval. You are then asked to choose your own **Password** and a
   **Super password**. The super password signs you in when no approved device is at hand, for
   example after you lose all your devices. Keep it somewhere safe.
2. To give someone else an account, open the menu under your name, choose **Members**, then
   **Create user**. Send the person the link, the account name and the initial password, which is
   shown only once. Each person sees and controls only their own computers. See
   [Accounts](accounts.md).

From then on, every new browser and computer has to be approved on the **My devices** page of a
browser that is already approved. A password alone is not enough to get in.

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

## Server options

Set these flags on the `ExecStart` line of the unit.

| Flag | Default | Meaning |
|---|---|---|
| `-listen` | `127.0.0.1:8080` | Address to listen on. Use `:443` with `-domain` or `-tls-cert` |
| `-data` | `deskway-data/state.json` | State file. The `certs` and `clients` folders sit beside it |
| `-domain` | | Get a Let's Encrypt certificate for this domain automatically, through port 443 (port 80 can stay closed) |
| `-tls-cert`, `-tls-key` | | Use your own certificate and key files instead of `-domain` |
| `-public-ip` | | The public IPv4 address. Turns on the built-in TURN relay. Without it, computers behind strict NATs can't be reached |
| `-turn-port` | `3478` | TURN port, UDP and TCP |
| `-turn-min-port`, `-turn-max-port` | `49160`, `49359` | Relay port range. Open the same range in the firewall |
| `-relay-account-mbps` | `30` | Each account's share of the relay bandwidth, both directions together |
| `-stun` | | Comma-separated STUN URLs to hand out when TURN is off |
| `-clients` | `clients` beside the state file | Folder of AI client programs |

### Your own certificate

Replace `-domain @DOMAIN@` in the unit with `-tls-cert /etc/deskway/fullchain.pem -tls-key
/etc/deskway/privkey.pem`. Both files must be readable by the `deskway` user (for example owner
`root`, group `deskway`, mode `640`). `ProtectSystem=strict` keeps `/etc` readable, so no other
unit change is needed. The server reads the certificate at startup, so run
`sudo systemctl restart deskway` after renewing it.

### Behind a reverse proxy

Running Deskway directly on port 443 is recommended. If another web server already uses port 443,
Deskway can run behind it, with the following limitations:

- The proxy must pass WebSocket upgrades and set `X-Forwarded-Proto: https`.
- Deskway sees every request as coming from the proxy. Approval prompts show the proxy's address
  instead of the device's, and the limit on failed password attempts applies to everyone
  together, so repeated wrong passwords from one person can lock others out for a while.
- The TURN ports (3478 and 49160–49359) still go straight to the server, not through the proxy.

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

1. Read the release notes for the version you are moving to.
2. Build the new server (step 3, after `git pull` or a fresh checkout of the new tag).
3. Stop the service, back up its data, replace the binary and start the service again:

   ```sh
   sudo systemctl stop deskway
   sudo cp -a /var/lib/deskway /var/lib/deskway.bak-$(date +%F)
   sudo install -m 755 bin/deskway-server /usr/local/bin/deskway-server
   sudo systemctl start deskway
   ```

4. Replace the files in `/var/lib/deskway/clients` with the ones from the new release (step 5).
   AI clients update themselves from there.
5. Update Deskway Host on each computer: on Windows run the new `Deskway-Setup.exe` and choose
   **Update**; on macOS quit Deskway Host and replace `Deskway.app` in Applications. If a computer no longer speaks
   the same protocol as the server, the browser shows "This computer needs an update" for that
   computer.

Browsers pick up the new web client on their own and show "A new version of Deskway is ready".

## Backup and restore

Everything the server keeps is in `/var/lib/deskway`: `state.json` (accounts, devices, approvals),
`state.json.super` (super passwords), `certs/` and `clients/`. For a consistent copy, stop the
service first:

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
| `admin` says the state is locked | The server is running. Stop it first |
| Sessions work on the same network but not from mobile data | `-public-ip` is the server's public address; TCP/UDP 3478 and UDP 49160–49359 are open in both firewalls |
| A computer stays "Waiting for approval on a signed-in device" | Approve it from an approved browser, on **My devices** |
| **Add an AI client** says the server has no programs | Step 5: the files are in the `clients` folder with the exact names |
| The browser shows "This computer needs an update" | Update Deskway Host on that computer to the server's version |
