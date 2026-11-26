#!/bin/sh
# Builds the AI client program (deskway) for this system into the folder the server hands it out
# from (deskway-server -clients; by default "clients" beside its state file), under the names
# internal/server/clientprograms.go lists. macOS builds both architectures. Linux builds the
# current machine's architecture and uses system OpenSSL (libssl-dev and pkg-config). Windows:
# scripts/build-ai-clients.ps1.
#
#   scripts/build-ai-clients.sh [OUT]      OUT defaults to deskway-data/clients
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${1:-"$root/deskway-data/clients"}
mkdir -p "$out"
cd "$root"

build() { # file name, optional target triple
    file=$1
    shift
    target_dir=${CARGO_TARGET_DIR:-target}
    if [ "$#" -gt 0 ]; then
        rustup target add "$1" >/dev/null
        target_dir="$target_dir/$1"
        set -- --target "$1"
    fi
    cargo build --locked --release -p deskway "$@"
    install -m 755 "$target_dir/release/deskway" "$out/$file"
    if command -v sha256sum >/dev/null; then sha256sum "$out/$file"; else shasum -a 256 "$out/$file"; fi
}

case "$(uname -s)/$(uname -m)" in
    Darwin/*)
        build deskway-macos-arm64 aarch64-apple-darwin
        build deskway-macos-x64 x86_64-apple-darwin
        ;;
    Linux/x86_64) build deskway-linux-x64 ;;
    Linux/aarch64) build deskway-linux-arm64 ;;
    *) echo "No AI client program for $(uname -s)/$(uname -m)." >&2; exit 1 ;;
esac
