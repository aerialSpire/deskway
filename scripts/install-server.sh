#!/bin/sh
set -eu

case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in
  zh_TW*|zh_HK*|zh_MO*|zh_Hant*|zh-TW*|zh-HK*|zh-MO*|zh-Hant*)
    usage='用法：install-server.sh --domain NAME [--version TAG] [--remote-mcp|--no-remote-mcp]'
    root_help='請在使用 systemd 的 Debian 或 Ubuntu 上以 sudo 執行安裝程式。'
    dns_help='請設定網域的 A 記錄，確認可解析為伺服器的公開 IPv4 位址後重試。'
    release_help='請確認 GitHub 可連線且指定的版本含有伺服器安裝檔，然後重試。'
    download='下載並驗證伺服器安裝檔與 SHA256SUMS'
    verify_help='請確認該版本含有完整的伺服器安裝檔和 SHA256SUMS，然後重試。'
    storage_help='請檢查安裝路徑的權限與可用空間後重試。'
    architecture='架構'
    arch_help='請使用 amd64 或 arm64 架構的 Debian 或 Ubuntu。'
    upgrade_help='請修正升級錯誤後重新執行安裝程式。'
    install_binary='安裝伺服器程式'
    install_user='準備 deskway 系統帳號與資料資料夾'
    link_once='此連結只能使用一次，一小時內有效。若需要新連結，請執行'
    link_help='請檢查伺服器狀態檔案的權限後重試。'
    install_unit='安裝、啟用並啟動 systemd 服務'
    enable_help='請執行 journalctl -u deskway.service 檢查錯誤，修正後執行 systemctl daemon-reload 與 systemctl enable --now deskway.service。'
    legacy_help='請先依照自架指南手動安裝支援 upgrade 的發布版本。'
    upgrade='升級已安裝的伺服器'
    restart='重新啟動 Deskway 服務'
    restart_help='請執行 journalctl -u deskway.service 檢查錯誤，修正後執行 systemctl restart deskway.service。'
    ready='請開啟'
    first_load='首次載入可能需要幾秒鐘，等待憑證簽發。'
    firewall='請開放防火牆連接埠 TCP 443、TCP 和 UDP 3478，以及 UDP 49160–49359。'
    mcp_edit_help='無法自動變更此服務的 ExecStart。請依照自架指南手動編輯服務單元（docs/self-hosting.md#remote-mcp）。'
    mcp_check_help='請修正遠端 MCP 預檢錯誤後重試。'
    mcp_restored='變更遠端 MCP 設定後服務無法啟動。已還原服務單元並重新啟動服務。'
    mcp_keys='伺服器會持有已核准的遠端應用程式金鑰，詳見安全性文件（docs/security.md）。'
    mcp_address='請在網頁用戶端的「新增 AI 用戶端」中查看 MCP 位址。'
    ;;
  zh*)
    usage='用法：install-server.sh --domain NAME [--version TAG] [--remote-mcp|--no-remote-mcp]'
    root_help='请在使用 systemd 的 Debian 或 Ubuntu 上以 sudo 运行安装程序。'
    dns_help='请设置域名的 A 记录，确认可解析为服务器的公网 IPv4 地址后重试。'
    release_help='请确认 GitHub 可访问且指定的版本含有服务器安装包，然后重试。'
    download='下载并验证服务器安装包和 SHA256SUMS'
    verify_help='请确认该版本含有完整的服务器安装包和 SHA256SUMS，然后重试。'
    storage_help='请检查安装路径的权限和可用空间后重试。'
    architecture='架构'
    arch_help='请使用 amd64 或 arm64 架构的 Debian 或 Ubuntu。'
    upgrade_help='请修正升级错误后重新运行安装程序。'
    install_binary='安装服务器程序'
    install_user='准备 deskway 系统账号和数据目录'
    link_once='此链接只能使用一次，一小时内有效。若需要新链接，请运行'
    link_help='请检查服务器状态文件的权限后重试。'
    install_unit='安装、启用并启动 systemd 服务'
    enable_help='请运行 journalctl -u deskway.service 检查错误，修正后运行 systemctl daemon-reload 和 systemctl enable --now deskway.service。'
    legacy_help='请先按自建指南手动安装支持 upgrade 的发布版本。'
    upgrade='升级已安装的服务器'
    restart='重启 Deskway 服务'
    restart_help='请运行 journalctl -u deskway.service 检查错误，修正后运行 systemctl restart deskway.service。'
    ready='请打开'
    first_load='首次加载可能需要几秒钟，等待证书签发。'
    firewall='请开放防火墙端口 TCP 443、TCP 和 UDP 3478，以及 UDP 49160–49359。'
    mcp_edit_help='无法自动更改此服务的 ExecStart。请按自建指南手动编辑服务单元（docs/self-hosting.md#remote-mcp）。'
    mcp_check_help='请修正远程 MCP 预检错误后重试。'
    mcp_restored='更改远程 MCP 设置后服务无法启动。已恢复服务单元并重启服务。'
    mcp_keys='服务器将持有已批准的远程应用程序密钥，详见安全文档（docs/security.md）。'
    mcp_address='请在网页客户端的「添加 AI 客户端」中查看 MCP 地址。'
    ;;
  *)
    usage='Usage: install-server.sh --domain NAME [--version TAG] [--remote-mcp|--no-remote-mcp]'
    root_help='Run the installer with sudo on Debian or Ubuntu booted with systemd.'
    dns_help="Set the domain's A record to the server's public IPv4 address and retry once it resolves."
    release_help='Check that GitHub is reachable and the selected release has server assets, then retry.'
    download='Download and verify the server archive and SHA256SUMS'
    verify_help='Check that the release has an intact server archive and SHA256SUMS, then retry.'
    storage_help='Check permissions and free space for the installation paths, then retry.'
    architecture='Architecture'
    arch_help='Use Debian or Ubuntu on amd64 or arm64.'
    upgrade_help='Fix the upgrade error, then rerun the installer.'
    install_binary='Install the server executable'
    install_user='Prepare the deskway system user and data directory'
    link_once='This link works once and expires in one hour. To get a new link, run'
    link_help='Check the server state file permissions, then retry.'
    install_unit='Install, enable and start the systemd service'
    enable_help='Check journalctl -u deskway.service, fix the error, then run systemctl daemon-reload and systemctl enable --now deskway.service.'
    legacy_help='Use the by-hand route in the self-hosting guide to install a release that supports upgrade.'
    upgrade='Upgrade the installed server'
    restart='Restart the Deskway service'
    restart_help='Check journalctl -u deskway.service, fix the error, then run systemctl restart deskway.service.'
    ready='Open'
    first_load='The first load can take a few seconds while the certificate is issued.'
    firewall='Open firewall ports TCP 443, TCP and UDP 3478, and UDP 49160–49359.'
    mcp_edit_help='Cannot change ExecStart automatically. Edit the unit by hand as described in the self-hosting guide (docs/self-hosting.md#remote-mcp).'
    mcp_check_help='Fix the remote MCP preflight error and retry.'
    mcp_restored='The service failed to start with the new remote MCP setting. The previous unit was restored and the service restarted.'
    mcp_keys='The server will hold keys for approved remote applications (see the security guide, docs/security.md).'
    mcp_address='Find the MCP address under "Add an AI client" in the web client.'
    ;;
esac

die() {
  printf '%s\n' "$*" >&2
  exit 1
}

run() {
  remedy=$1
  shift
  "$@" || die "$remedy"
}

service_failed() {
  journalctl -u deskway.service -n 20 --no-pager >&2 || :
  if [ -n "$previous_unit" ]; then
    run "$storage_help" cp -p "$previous_unit" "$unit"
    run "$restart_help" systemctl daemon-reload
    run "$restart_help" systemctl restart deskway.service
    service_active || die "$restart_help"
    die "$mcp_restored"
  fi
  die "$1"
}

service_active() {
  sleep 3
  systemctl is-active --quiet deskway.service
}

domain=
version=
remote_mcp=keep
while [ "$#" -gt 0 ]; do
  option=$1
  shift
  case "$option" in
    --help) printf '%s\n' "$usage"; exit 0 ;;
    --remote-mcp|--no-remote-mcp)
      [ "$remote_mcp" = keep ] || die "$usage"
      remote_mcp=$option
      ;;
    --domain|--version)
      case "${1:-}" in ''|--*) die "$usage" ;; esac
      case "$option" in
        --domain) domain=$1 ;;
        --version)
          case "$1" in *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-]*) die "$usage" ;; esac
          version=$1
          ;;
      esac
      shift
      ;;
    *) die "$usage" ;;
  esac
done
case "$domain" in ''|-*|*[!a-zA-Z0-9.-]*) die "$usage" ;; esac

[ "$(id -u)" = 0 ] && [ -d /run/systemd/system ] && [ -r /etc/systemd/system ] || die "$root_help"

binary=/usr/local/bin/deskway-server
data=/var/lib/deskway
unit=/etc/systemd/system/deskway.service

# Match the shipped unit's command, allowing the domain and IP chosen at install time.
exec_start='ExecStart=/usr/local/bin/deskway-server -listen :443 -domain @DOMAIN@ -data /var/lib/deskway/state.json -public-ip @IP@'
read_exec_start() {
  awk -v shape="$exec_start" '
    BEGIN {
      gsub(/[.]/, "[.]", shape)
      sub(/@DOMAIN@/, "[a-zA-Z0-9.][a-zA-Z0-9.-]*", shape)
      sub(/@IP@/, "[0-9.]+", shape)
      shape = "^" shape "( -remote-mcp)?$"
    }
    { line = $0; sub(/^\357\273\277/, "", line) }
    line ~ /^[[:space:]]*[#;]/ { next }
    !continued && line ~ /^[[:space:]]*\[/ { service = line ~ /^[[:space:]]*\[Service\][[:space:]]*$/ }
    line ~ /^[[:space:]]*ExecStart[[:space:]]*(=|\\$)/ {
      if (continued || !service || $0 !~ shape || ++starts != 1) exit 1
      print
    }
    { continued = line ~ /(^|[^\\])(\\\\)*\\$/ }
    END { if (starts != 1) exit 1 }
  ' "$@"
}

mcp_flag=
mcp_edit=
previous_unit=
if [ "$remote_mcp" != keep ]; then
  [ "$remote_mcp" != --remote-mcp ] || mcp_flag=' -remote-mcp'
  mcp_edit="/^ExecStart=/ { s/ -remote-mcp\$//; s/\$/$mcp_flag/; }"
fi

check_mcp() {
  run "$mcp_check_help" runuser -u "$1" -- "$binary" remote-mcp-check -data "$data/state.json"
}

announce_mcp() {
  if [ "$remote_mcp" = --remote-mcp ]; then
    printf '%s\n' "$mcp_keys" "$mcp_address"
  fi
}

issue_initial_link() {
  link_domain=$(sed -nE 's/^[[:space:]]*ExecStart[[:space:]]*=.* -domain[ =]([a-zA-Z0-9.-]+)([[:space:]].*)?$/\1/p' "$unit") || die "$link_help"
  link_origin="https://${link_domain:-$domain}"
  link_user=$(systemctl show --property=User --value deskway.service) || die "$link_help"
  link_user=${link_user:-root}
  link=$(runuser -u "$link_user" -- "$binary" admin-link -if-needed -data "$data/state.json" -url "$link_origin") || die "$link_help"
  if [ -n "$link" ]; then
    printf '%s\n' "$link" "$link_once:" "sudo runuser -u $link_user -- $binary admin-link -data $data/state.json -url $link_origin"
  fi
}

if [ -f "$unit" ]; then
  if [ "$remote_mcp" != keep ]; then
    current_start=$(read_exec_start "$unit") || die "$mcp_edit_help"
    # systemctl cat uses the loaded fragment and drop-in paths.
    properties=$(systemctl show --property=FragmentPath,NeedDaemonReload,User deskway.service) || die "$mcp_edit_help"
    printf '%s\n' "$properties" | grep -Fxq "FragmentPath=$unit" || die "$mcp_edit_help"
    printf '%s\n' "$properties" | grep -Fxq 'NeedDaemonReload=no' || die "$mcp_edit_help"
    effective=$(systemctl --no-pager cat deskway.service) || die "$mcp_edit_help"
    effective_start=$(printf '%s\n' "$effective" | read_exec_start) || die "$mcp_edit_help"
    [ "$effective_start" = "$current_start" ] || die "$mcp_edit_help"
    service_user=$(printf '%s\n' "$properties" | sed -n 's/^User=//p')
  fi
  "$binary" -supports-upgrade >/dev/null 2>&1 || die "$legacy_help"
  printf '%s\n' "$upgrade"
  set -- upgrade
  if [ -n "$version" ]; then
    set -- "$@" -to "$version"
  fi
  run "$upgrade_help" "$binary" "$@"
  if [ "$remote_mcp" = --remote-mcp ]; then
    check_mcp "${service_user:-root}"
  fi
  if [ "$remote_mcp" != keep ] && [ "$current_start" != "${current_start% -remote-mcp}$mcp_flag" ]; then
    work=$(mktemp -d) || die "$storage_help"
    trap 'rm -rf -- "$work"' 0
    run "$storage_help" cp -p "$unit" "$work/previous.service"
    sed -e "$mcp_edit" "$work/previous.service" > "$work/deskway.service" || die "$storage_help"
    previous_unit="$work/previous.service"
    install -m 644 "$work/deskway.service" "$unit" || service_failed "$storage_help"
    systemctl daemon-reload || service_failed "$enable_help"
  fi
  systemctl enable --now deskway.service || service_failed "$enable_help"
  # A running service needs a restart to load the replacement binary.
  printf '%s\n' "$restart"
  systemctl restart deskway.service || service_failed "$restart_help"
  service_active || service_failed "$restart_help"
  issue_initial_link
  announce_mcp
  exit 0
fi

public_ip=$(getent -s dns ahostsv4 "${domain%.}." | awk 'NR == 1 {print $1}')
case "$public_ip" in
  '') die "$domain. $dns_help" ;;
  127.*|10.*|172.1[6-9].*|172.2[0-9].*|172.3[01].*|192.168.*)
    die "$domain ($public_ip). $dns_help" ;;
esac
command -v curl >/dev/null 2>&1 || die 'sudo apt-get update && sudo apt-get install -y curl ca-certificates'

arch=$(dpkg --print-architecture) || die "$root_help"
printf '%s\n' "$architecture $arch"
case "$arch" in amd64|arm64) ;; *) die "$arch_help" ;; esac

releases=https://github.com/aerialSpire/deskway/releases
if [ -n "$version" ]; then
  tag=$version
else
  # Resolve latest before downloading so both files come from the same release.
  page=$(curl -q -fsS -o /dev/null -w '%{redirect_url}' "$releases/latest") || die "$release_help"
  tag=${page#"$releases/tag/"}
  [ "$tag" != "$page" ] && [ -n "$tag" ] || die "$release_help"
fi
base="$releases/download/$tag"
package="deskway-server-linux-$arch"
archive="$package.tar.gz"
work=$(mktemp -d) || die "$storage_help"
trap 'rm -rf -- "$work"' 0
trap 'die "$storage_help"' HUP INT TERM
cd "$work" || die "$storage_help"

printf '%s\n' "$download" "$base/$archive" "$base/SHA256SUMS"
run "$release_help" curl -q -fLsS --proto '=https' --proto-redir '=https' -o "$archive" "$base/$archive"
run "$release_help" curl -q -fLsS --proto '=https' --proto-redir '=https' -o SHA256SUMS "$base/SHA256SUMS"
awk -v asset="$archive" '$2 == asset || $2 == "*" asset' SHA256SUMS |
  sha256sum --check --strict - || die "$verify_help"
run "$verify_help" tar -xzf "$archive" "$package/deskway-server" "$package/deskway.service"
sed -e "s/@DOMAIN@/$domain/g" -e "s/@IP@/$public_ip/g" -e "$mcp_edit" "$package/deskway.service" > deskway.service ||
  die "$storage_help"
if [ "$remote_mcp" != keep ]; then
  read_exec_start deskway.service >/dev/null || die "$mcp_edit_help"
fi

printf '%s\n' "$install_user"
if ! getent passwd deskway >/dev/null; then
  run "$storage_help" useradd --system --user-group --home-dir "$data" --shell /usr/sbin/nologin deskway
fi
run "$storage_help" install -d -m 700 -o deskway -g deskway "$data"
printf '%s\n' "$install_binary"
run "$storage_help" install -m 755 "$package/deskway-server" "$binary"
if [ "$remote_mcp" = --remote-mcp ]; then
  check_mcp deskway
  run "$storage_help" install -d -m 755 -o deskway -g deskway "$data/clients"
fi
printf '%s\n' "$install_unit"
run "$storage_help" install -m 644 deskway.service "$unit"
run "$enable_help" systemctl daemon-reload
systemctl enable --now deskway.service || service_failed "$enable_help"
service_active || service_failed "$enable_help"
issue_initial_link
printf '%s\n' "$ready https://$domain — $first_load" "$firewall"
announce_mcp
