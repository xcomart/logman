#!/bin/sh
# Installs logman for the current user: binary, desktop entry and icons.
# Run from the unpacked release directory. No root required.
set -eu

prefix="${XDG_DATA_HOME:-$HOME/.local/share}"
bindir="$HOME/.local/bin"

here="$(cd "$(dirname "$0")" && pwd)"

install -Dm755 "$here/logman" "$bindir/logman"
install -Dm644 "$here/logman.desktop" "$prefix/applications/logman.desktop"
install -Dm644 "$here/icons/logman-128.png" "$prefix/icons/hicolor/128x128/apps/logman.png"
install -Dm644 "$here/icons/logman-256.png" "$prefix/icons/hicolor/256x256/apps/logman.png"
install -Dm644 "$here/icons/logman.svg" "$prefix/icons/hicolor/scalable/apps/logman.svg"

# Refresh caches when the tools are around; harmless to skip.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/applications" || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache -q "$prefix/icons/hicolor" || true

echo "installed logman to $bindir (make sure it is on your PATH)"
