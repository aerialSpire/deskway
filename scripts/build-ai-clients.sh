#!/bin/sh
# Builds the AI client program (deskway) for this system into the folder the server hands it out
# from (deskway-server -clients; by default "clients" beside its state file), under the names
# internal/server/aiclients.go lists, on a Mac: Apple silicon and Intel. Windows:
# scripts/build-ai-clients.ps1.
#
#   scripts/build-ai-clients.sh [OUT]      OUT defaults to deskway-data/clients
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${1:-"$root/deskway-data/clients"}
mkdir -p "$out"
cd "$root"

build() { # target triple, file name
    rustup target add "$1" >/dev/null
    cargo build --locked --release -p deskway --target "$1"
    install -m 755 "target/$1/release/deskway" "$out/$2"
    if command -v sha256sum >/dev/null; then sha256sum "$out/$2"; else shasum -a 256 "$out/$2"; fi
}

case "$(uname -s)/$(uname -m)" in
    Darwin/*)
        build aarch64-apple-darwin deskway-macos-arm64
        build x86_64-apple-darwin deskway-macos-x64
        ;;
    *) echo "No AI client program for $(uname -s)/$(uname -m)." >&2; exit 1 ;;
esac
