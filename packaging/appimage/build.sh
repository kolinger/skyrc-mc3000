#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "$0")/../.." && pwd)"
cd "$root"

PATH="$root:$PATH"
if [[ -n "${HOME:-}" ]]; then
    PATH="$PATH:$HOME/.local/bin:$HOME/.cargo/bin"
fi
export PATH

linuxdeploy="${LINUXDEPLOY:-linuxdeploy}"
if ! command -v "$linuxdeploy" >/dev/null 2>&1; then
    if [[ -n "${LINUXDEPLOY:-}" ]]; then
        printf 'LINUXDEPLOY="%s" was not found or is not executable; correct or unset LINUXDEPLOY to use the default\n' "$linuxdeploy" >&2
    else
        printf 'linuxdeploy was not found; install it or set LINUXDEPLOY to its executable path\n' >&2
    fi
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
