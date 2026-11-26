#!/bin/sh
# Build and package the host for the Linux machine running this script.
set -eu

out=$1
package="deskway-host-linux-$(dpkg --print-architecture)"
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT

cargo build --locked --release -p deskway-host
mkdir -p "$out" "$stage/$package"
install -m 755 "${CARGO_TARGET_DIR:-target}/release/deskway-host" "$stage/$package/"
install -m 644 LICENSE deploy/systemd/deskway-host.service "$stage/$package/"
tar -czf "$out/$package.tar.gz" --owner=0 --group=0 --numeric-owner -C "$stage" "$package"
