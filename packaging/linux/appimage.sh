#!/bin/bash
# AppImage Linux x86_64 : dist/AudioPlayer-<version>-x86_64.AppImage, construite avec
# linuxdeploy (qui y copie les bibliothèques nécessaires hors celles du système de base).
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release -p audioplayer

tools=target/appimage-tools
mkdir -p "$tools" dist
if [ ! -x "$tools/linuxdeploy" ]; then
    curl -fsSL -o "$tools/linuxdeploy" \
        https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
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
