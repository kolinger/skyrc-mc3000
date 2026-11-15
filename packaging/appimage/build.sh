#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "$0")/../.." && pwd)"
cd "$root"

export PATH="$root:$PATH"
linuxdeploy="${LINUXDEPLOY:-linuxdeploy}"
if ! command -v "$linuxdeploy" >/dev/null 2>&1; then
    printf 'linuxdeploy was not found; set LINUXDEPLOY to its executable path\n' >&2
    exit 1
fi

cargo build --release --package mc5000-gui --bin mc5000-gui

appdir="$(mktemp -d "${TMPDIR:-/tmp}/mc5000-appdir.XXXXXX")"
trap 'rm -rf "$appdir"' EXIT

LDAI_OUTPUT="$root/mc5000-gui-x86_64.AppImage" \
    APPIMAGE_EXTRACT_AND_RUN=1 \
    "$linuxdeploy" \
    --appdir "$appdir" \
    --executable "$root/target/release/mc5000-gui" \
    --desktop-file "$root/packaging/appimage/mc5000.desktop" \
    --icon-file "$root/resources/mc5000.svg" \
    --output appimage
