#!/bin/sh
# Exercise the installer with a local release archive and isolated system commands.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
real_install=$(command -v install)
export real_install

mkdir -p "$work/bin" "$work/release/deskway-server-linux-amd64"
package="$work/release/deskway-server-linux-amd64"
cp "$root/deploy/systemd/deskway.service" "$package/deskway.service"
sed -e 's|^binary=.*|binary="$fixture/bin/deskway-server"|' \
    -e 's|^data=.*|data="$fixture/data"|' \
    -e 's|^unit=.*|unit="$fixture/etc/deskway.service"|' \
    -e 's|/run/systemd/system|"$fixture/run"|g' \
    -e 's|/etc/systemd/system|"$fixture/etc"|g' "$root/scripts/install-server.sh" > "$work/install.sh"

cat > "$package/deskway-server" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >> "$fixture/server.log"
case "$1" in
  admin-link)
    grep -Fxq 'is-active --quiet deskway.service' "$fixture/systemctl.log" || exit 1
    [ "$2" = -if-needed ] || exit 1
    if [ "$needs_admin" = yes ]; then
      printf '%s\n' "$6/setup#c=test-link"
    fi
    ;;
  upgrade) echo '# upgraded' >> "$0" ;;
  remote-mcp-check)
    [ "$preflight" != fail ] || { echo 'remote MCP: preflight failed' >&2; exit 1; }
    ;;
esac
SH
cat > "$work/bin/curl" <<'SH'
#!/bin/sh
while [ "$1" != -o ]; do shift; done
cp "$release/${3##*/}" "$2"
SH
cat > "$work/bin/getent" <<'SH'
#!/bin/sh
case "$*" in
  '-s dns ahostsv4 desk.example.net.') printf '%s\n' '192.0.2.1 STREAM desk.example.net' ;;
  'passwd deskway') exit 0 ;;
  *) exit 1 ;;
esac
SH
cat > "$work/bin/runuser" <<'SH'
#!/bin/sh
[ "$1 $2 $3" = '-u deskway --' ] || exit 1
shift 3
exec "$@"
SH
cat > "$work/bin/install" <<'SH'
#!/bin/sh
# These fixtures run unprivileged, so omit ownership changes on the data directory.
if [ "$1" = -d ]; then
  for path do :; done
  mkdir -p "$path"
else
  exec "$real_install" "$@"
fi
SH
cat > "$work/bin/dpkg" <<'SH'
#!/bin/sh
printf '%s\n' "$trial_arch"
SH
cat > "$work/bin/id" <<'SH'
#!/bin/sh
echo 0
SH
cat > "$work/bin/sleep" <<'SH'
#!/bin/sh
exit 0
SH
cat > "$work/bin/systemctl" <<'SH'
#!/bin/sh
case "$1" in
  show)
    case "$2" in
      --property=User) printf '%s\n' 'deskway' ;;
      *) printf '%s\n' "FragmentPath=$fixture/etc/deskway.service" 'NeedDaemonReload=no' 'User=deskway' ;;
    esac
    exit 0
    ;;
  --no-pager)
    cat "$fixture/etc/deskway.service"
    if [ -f "$fixture/drop-in.conf" ]; then cat "$fixture/drop-in.conf"; fi
    exit 0
    ;;
esac
printf '%s\n' "$*" >> "$fixture/systemctl.log"
[ "$1" != "$fail_at" ] || cmp -s "$fixture/before" "$fixture/etc/deskway.service"
SH
cat > "$work/bin/journalctl" <<'SH'
#!/bin/sh
printf '%s\n' "journalctl $*" >> "$fixture/systemctl.log"
echo 'journal: test startup failure'
SH
chmod +x "$work/bin/"* "$package/deskway-server"
release="$work/release"
export release
(
  cd "$release"
  tar -czf deskway-server-linux-amd64.tar.gz deskway-server-linux-amd64
  sha256sum deskway-server-linux-amd64.tar.gz > SHA256SUMS
)
export PATH="$work/bin:$PATH"

while read -r case_name before option arch preflight locale fail_at expected; do
  fixture="$work/$case_name"
  trial_arch=$arch
  needs_admin=no
  case "$case_name" in new-*|rerun-unfinished-setup|rerun-proxy-setup) needs_admin=yes ;; esac
  export fixture trial_arch preflight fail_at needs_admin
  mkdir -p "$fixture/run" "$fixture/etc" "$fixture/bin" "$fixture/data"
  sed -e 's/@DOMAIN@/desk.example.net/g' -e 's/@IP@/192.0.2.1/g' "$package/deskway.service" > "$fixture/base"
  if [ "$before" != none ]; then
    cat "$package/deskway-server" > "$fixture/bin/deskway-server"
    printf '%s\n' '# installed version' >> "$fixture/bin/deskway-server"
    chmod +x "$fixture/bin/deskway-server"
    cp "$fixture/bin/deskway-server" "$fixture/binary-before"
    case "$before" in
      off|drop-in|environment) cp "$fixture/base" "$fixture/before" ;;
      proxy) sed '/^ExecStart=/s/-listen :443 -domain desk\.example\.net/-listen 127.0.0.1:8080/' "$fixture/base" > "$fixture/before" ;;
      on) sed '/^ExecStart=/s/$/ -remote-mcp/' "$fixture/base" > "$fixture/before" ;;
      value) sed '/^ExecStart=/s/$/ -remote-mcp=true/' "$fixture/base" > "$fixture/before" ;;
      quoted) sed '/^ExecStart=/s/$/ "-remote-mcp"/' "$fixture/base" > "$fixture/before" ;;
      space) sed 's/^ExecStart=/ExecStart =/' "$fixture/base" > "$fixture/before" ;;
      continued) sed '/^ExecStart=/s/$/ \\\n  -remote-mcp/' "$fixture/base" > "$fixture/before" ;;
    esac
    case "$before" in
      drop-in)
        printf '%s\n' '[Service]' 'ExecStart=' 'ExecStart=/usr/local/bin/other-server' > "$fixture/drop-in.conf"
        ;;
      environment)
        printf '%s\n' '[Service]' 'Environment=LANG=C' > "$fixture/drop-in.conf"
        ;;
    esac
    cp "$fixture/before" "$fixture/etc/deskway.service"
    printf '%s\n' '{}' > "$fixture/data/state.json"
  fi
  domain=desk.example.net
  [ "$case_name" != rerun-unfinished-setup ] || domain=different.example.net
  set -- --domain "$domain" --version local
  [ "$option" = keep ] || set -- "$@" "$option"
  status=0
  LC_ALL="$locale" sh "$work/install.sh" "$@" > "$fixture/output" 2>&1 || status=$?
  case "$expected" in
    refused)
      [ "$status" -eq 1 ]
      cmp "$fixture/before" "$fixture/etc/deskway.service"
      cmp "$fixture/binary-before" "$fixture/bin/deskway-server"
      [ ! -e "$fixture/server.log" ]
      [ ! -e "$fixture/systemctl.log" ]
      case "$locale" in
        C) message='Edit the unit by hand' ;;
        zh_CN) message='手动编辑服务单元' ;;
        zh_TW) message='手動編輯服務單元' ;;
      esac
      grep -F "$message" "$fixture/output" >/dev/null
      grep -F 'docs/self-hosting.md#remote-mcp' "$fixture/output" >/dev/null
      ;;
    preflight)
      [ "$status" -eq 1 ]
      if [ "$before" = none ]; then
        [ ! -e "$fixture/etc/deskway.service" ]
      else
        cmp "$fixture/before" "$fixture/etc/deskway.service"
      fi
      [ ! -e "$fixture/systemctl.log" ]
      grep -Fx 'remote MCP: preflight failed' "$fixture/output" >/dev/null
      ;;
    rollback)
      [ "$status" -eq 1 ]
      cmp "$fixture/before" "$fixture/etc/deskway.service"
      grep -Fx '# upgraded' "$fixture/bin/deskway-server" >/dev/null
      {
        printf '%s\n' 'daemon-reload' 'enable --now deskway.service' 'restart deskway.service'
        if [ "$fail_at" = is-active ]; then printf '%s\n' 'is-active --quiet deskway.service'; fi
        printf '%s\n' 'journalctl -u deskway.service -n 20 --no-pager' \
          'daemon-reload' 'restart deskway.service' 'is-active --quiet deskway.service'
      } > "$fixture/expected-calls"
      cmp "$fixture/expected-calls" "$fixture/systemctl.log"
      grep -F 'journal: test startup failure' "$fixture/output" >/dev/null
      grep -F 'The previous unit was restored and the service restarted.' "$fixture/output" >/dev/null
      ;;
    *)
      if [ "$status" -ne 0 ]; then cat "$fixture/output" >&2; exit "$status"; fi
      case "$expected" in
        on)
          sed '/^ExecStart=/s/$/ -remote-mcp/' "$fixture/base" > "$fixture/expected"
          case "$locale" in
            C) address='Find the MCP address under "Add an AI client" in the web client.' ;;
            zh_CN) address='请在网页客户端的「添加 AI 客户端」中查看 MCP 地址。' ;;
            zh_TW) address='請在網頁用戶端的「新增 AI 用戶端」中查看 MCP 位址。' ;;
          esac
          grep -Fx "$address" "$fixture/output" >/dev/null
          if grep -Fq 'https://desk.example.net/mcp' "$fixture/output"; then exit 1; fi
          grep -F 'docs/security.md' "$fixture/output" >/dev/null
          ;;
        off) cp "$fixture/base" "$fixture/expected" ;;
        keep) cp "$fixture/before" "$fixture/expected" ;;
      esac
      cmp "$fixture/expected" "$fixture/etc/deskway.service"
      if [ "$before" != none ]; then
        grep -Fx 'upgrade -to local' "$fixture/server.log" >/dev/null
        grep -Fx 'restart deskway.service' "$fixture/systemctl.log" >/dev/null
      fi
      grep -Fx "admin-link -if-needed -data $fixture/data/state.json -url https://desk.example.net" "$fixture/server.log" >/dev/null
      if grep -q '^journalctl ' "$fixture/systemctl.log"; then exit 1; fi
      if [ "$needs_admin" = yes ]; then
        grep -Fx 'https://desk.example.net/setup#c=test-link' "$fixture/output" >/dev/null
        grep -F 'works once and expires in one hour' "$fixture/output" >/dev/null
        grep -Fx "sudo runuser -u deskway -- $fixture/bin/deskway-server admin-link -data $fixture/data/state.json -url https://desk.example.net" "$fixture/output" >/dev/null
      elif grep -Fq '/setup#c=' "$fixture/output" || grep -Fq 'sudo runuser -u deskway --' "$fixture/output"; then
        exit 1
      fi
      ;;
  esac
  if [ "$expected" != refused ] && [ "$option" = --remote-mcp ]; then
    grep -Fx "remote-mcp-check -data $fixture/data/state.json" "$fixture/server.log" >/dev/null
  fi
  printf 'PASS %s\n' "$case_name"
done <<'CASES'
new-off none keep amd64 ok C no off
new-on none --remote-mcp amd64 ok C no on
rerun-enable off --remote-mcp arm64 ok zh_CN no on
rerun-enable-on on --remote-mcp amd64 ok zh_TW no on
rerun-disable on --no-remote-mcp amd64 ok C no off
rerun-keep-on on keep amd64 ok C no keep
rerun-keep-off off keep amd64 ok C no keep
rerun-unfinished-setup off keep amd64 ok C no keep
rerun-proxy-setup proxy keep amd64 ok C no keep
new-preflight-failure none --remote-mcp amd64 fail C no preflight
rerun-preflight-failure off --remote-mcp amd64 fail C no preflight
refuse-boolean value --remote-mcp amd64 ok C no refused
refuse-quoted quoted --no-remote-mcp amd64 ok zh_CN no refused
refuse-spaced space --remote-mcp amd64 ok zh_TW no refused
refuse-continued continued --no-remote-mcp amd64 ok C no refused
refuse-drop-in drop-in --remote-mcp amd64 ok C no refused
allow-environment-drop-in environment --remote-mcp amd64 ok C no on
rollback-failed-restart off --remote-mcp amd64 ok C restart rollback
rollback-inactive-service on --no-remote-mcp amd64 ok C is-active rollback
CASES
