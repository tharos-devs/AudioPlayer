#!/bin/sh
# Construit target/release/bundle/AudioPlayer.app, signé localement (ad hoc).
# Installer : glisser l'app dans Applications, ou `cp -R` de la ligne affichée à la fin.
set -eu
cd "$(dirname "$0")/../.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release -p audioplayer

APP=target/release/bundle/AudioPlayer.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/audioplayer "$APP/Contents/MacOS/AudioPlayer"
cp packaging/macos/AudioPlayer.icns "$APP/Contents/Resources/AudioPlayer.icns"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
# Signature ad hoc : suffisante pour ce Mac ; une distribution demandera un certificat Apple.
codesign --force --deep --sign - "$APP"

echo "$APP (version $VERSION)"
echo "Installer : cp -R \"$APP\" /Applications/"
