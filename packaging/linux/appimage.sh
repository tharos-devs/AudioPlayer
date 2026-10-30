#!/bin/bash
# AppImage Linux x86_64 : dist/AudioPlayer-<version>-x86_64.AppImage, construite avec
# linuxdeploy (qui y copie les bibliothèques nécessaires hors celles du système de base).
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked -p audioplayer

tools=target/appimage-tools
mkdir -p "$tools" dist
# Version figée et vérifiée : l'AppImage publiée ne dépend pas d'une version « continuous »
# qui change sans prévenir.
LINUXDEPLOY_URL=https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
LINUXDEPLOY_SHA256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
if ! echo "$LINUXDEPLOY_SHA256  $tools/linuxdeploy" | sha256sum -c --quiet 2>/dev/null; then
    curl -fsSL -o "$tools/linuxdeploy" "$LINUXDEPLOY_URL"
    echo "$LINUXDEPLOY_SHA256  $tools/linuxdeploy" | sha256sum -c --quiet
    chmod +x "$tools/linuxdeploy"
fi

appdir=target/AppDir
rm -rf "$appdir"
cp crates/app/ui/icons/app.png target/audioplayer.png
# Sans FUSE (conteneurs, CI) : l'outil s'extrait au lieu de se monter.
export APPIMAGE_EXTRACT_AND_RUN=1 LINUXDEPLOY_OUTPUT_VERSION=$version
"$tools/linuxdeploy" --appdir "$appdir" \
    --executable target/release/audioplayer \
    --desktop-file packaging/linux/audioplayer.desktop \
    --icon-file target/audioplayer.png \
    --output appimage
mv AudioPlayer-"$version"-x86_64.AppImage dist/
echo "dist/AudioPlayer-$version-x86_64.AppImage"
