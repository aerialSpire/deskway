#!/bin/sh
set -eu

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    echo "Build Deskway.app on an Apple silicon Mac." >&2
    exit 1
fi

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
package_arch=arm64
case "$#:${1-}" in
    0:) mode=archive ;;
    1:--dmg) mode=dmg ;;
    1:--universal-dmg)
        mode=dmg
        package_arch=universal
        ;;
    1:--install) mode=install ;;
    *) echo "Usage: $0 [--dmg | --universal-dmg | --install]" >&2; exit 2 ;;
esac
if [ "$mode" = install ] && [ "${DESKWAY_INSTALL_LOCKED:-}" != yes ]; then
    # Keep the lockfile so concurrent updates always contend on the same inode.
    exec /usr/bin/lockf -k /Applications/.deskway-install.lock env DESKWAY_INSTALL_LOCKED=yes "$root/scripts/build-mac-host.sh" --install
fi

cd "$root/host"
export MACOSX_DEPLOYMENT_TARGET=15.0
export CARGO_TARGET_DIR="$root/target"
cargo build --locked --release --target aarch64-apple-darwin --bin deskway-host --example mac-icon --example mac-plist-strings

mkdir -p "$root/bin"
app=/Applications/Deskway.app
legacy="$root/bin/Deskway.app"
if [ "$mode" = install ]; then
    # Stage on the same volume as the installed app so the replacement can be renamed into place.
    staging=$(mktemp -d /Applications/.deskway-build.XXXXXX)
else
    staging=$(mktemp -d "$root/bin/.deskway-build.XXXXXX")
fi
cleanup() {
    # An interrupted rename must leave the previous app available.
    if [ -d "$staging/previous" ] && [ ! -e "$app" ]; then
        mv "$staging/previous" "$app" || return
    fi
    rm -rf "$staging"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
bundle="$staging/Deskway.app"
iconset="$staging/Deskway.iconset"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$iconset"
arm64_release="$CARGO_TARGET_DIR/aarch64-apple-darwin/release"
executable="$bundle/Contents/MacOS/deskway-host"
if [ "$package_arch" = universal ]; then
    cargo build --locked --release --target x86_64-apple-darwin --bin deskway-host
    lipo -create "$arm64_release/deskway-host" \
        "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/deskway-host" -output "$executable"
    lipo "$executable" -verify_arch arm64 x86_64
    chmod 755 "$executable"
else
    install -m 755 "$arm64_release/deskway-host" "$executable"
fi
"$arm64_release/examples/mac-icon" "$iconset"
iconutil -c icns "$iconset" -o "$bundle/Contents/Resources/Deskway.icns"
# The host product name and permission prompts in the Mac's language.
"$arm64_release/examples/mac-plist-strings" "$bundle/Contents"
codesign --force --sign - --timestamp=none "$bundle"
codesign --verify --strict --all-architectures "$bundle"

if [ "$mode" != install ]; then
    # Keep the distribution copy out of Launch Services and the privacy permission pickers.
    if [ "$mode" = dmg ]; then
        package="Deskway-macOS-$package_arch.dmg"
        mkdir "$staging/image"
        mv "$bundle" "$staging/image/"
        ln -s /Applications "$staging/image/Applications"
        hdiutil create -volname 'Deskway Host' -srcfolder "$staging/image" -format UDZO "$staging/$package"
        hdiutil verify "$staging/$package"
    else
        package=Deskway-macOS-arm64.zip
        ditto -c -k --keepParent "$bundle" "$staging/$package"
    fi
    mv "$staging/$package" "$root/bin/$package"
    printf '%s\n' "$root/bin/$package"
    exit 0
fi

app_pids() {
    ps -axo pid=,comm= | awk -v exe="$1/Contents/MacOS/deskway-host" '
        { pid = $1; sub(/^[[:space:]]*[0-9]+[[:space:]]+/, ""); if ($0 == exe) print pid }
    '
}

stop_app() {
    for pid in $(app_pids "$1"); do kill -TERM "$pid" || return; done
    remaining=50
    while [ -n "$(app_pids "$1")" ]; do
        if [ "$remaining" -eq 0 ]; then
            echo "Deskway did not exit at $1." >&2
            return 1
        fi
        remaining=$((remaining - 1))
        sleep 0.1
    done
}

# Only replace Deskway bundles at the two paths used by this script.
for old in "$app" "$legacy"; do
    if [ -e "$old" ] || [ -L "$old" ]; then
        if [ -L "$old" ] || [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$old/Contents/Info.plist" 2>/dev/null)" != com.deskway.host ]; then
            echo "Unexpected application at $old." >&2
            exit 1
        fi
    fi
done

stop_app "$legacy"
# A repeat build of the same bits keeps the installed bundle and any running session in place.
if cmp -s "$bundle/Contents/Info.plist" "$app/Contents/Info.plist" &&
    diff -r "$bundle/Contents/Resources" "$app/Contents/Resources" >/dev/null 2>&1 &&
    cmp -s "$bundle/Contents/MacOS/deskway-host" "$app/Contents/MacOS/deskway-host" &&
    codesign --verify --strict "$app" 2>/dev/null; then
    :
else
    stop_app "$app"
    if [ -d "$app" ]; then mv "$app" "$staging/previous"; fi
    mv "$bundle" "$app"
fi

if [ -d "$legacy" ]; then
    # Rebind the enabled login item while its old bundle still exists.
    if ! "$app/Contents/MacOS/deskway-host" refresh-login-item || ! mv "$legacy" "$staging/legacy"; then
        echo "Could not migrate the previous Deskway bundle at $legacy." >&2
        # Keep the installed bundle usable if the login item needs attention.
        open -a "$app"
        exit 1
    fi
fi

launch_app() {
    open -a "$app" || return
    remaining=100
    until [ -n "$(app_pids "$app")" ]; do
        if [ "$remaining" -eq 0 ]; then
            echo "Deskway did not start at $app." >&2
            return 1
        fi
        remaining=$((remaining - 1))
        sleep 0.1
    done
}

if ! launch_app; then
    if [ -d "$staging/legacy" ]; then mv "$staging/legacy" "$legacy"; fi
    if [ -d "$staging/previous" ]; then
        mv "$app" "$staging/failed"
        mv "$staging/previous" "$app"
        open -a "$app"
    fi
    exit 1
fi
printf '%s\n' "$app"
