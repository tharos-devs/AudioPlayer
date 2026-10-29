#!/bin/sh
# Construit target/release/bundle/AudioPlayer.app, signé localement (ad hoc).
# `--universal` : binaire Apple Silicon + Intel (cibles rustup aarch64/x86_64-apple-darwin).
# Installer : glisser l'app dans Applications, ou `cp -R` de la ligne affichée à la fin.
set -eu
cd "$(dirname "$0")/../.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "${1:-}" = "--universal" ]; then
    # Les cibles viennent de rustup : un Rust Homebrew placé avant dans le PATH n'en a qu'une.
    PATH="$(dirname "$(rustup which cargo)"):$PATH"
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release -p audioplayer --target "$target"
    done
    BINARY=target/universal/audioplayer
    mkdir -p target/universal
    lipo -create -output "$BINARY" \
        target/aarch64-apple-darwin/release/audioplayer \
        target/x86_64-apple-darwin/release/audioplayer
else
    cargo build --release -p audioplayer
    BINARY=target/release/audioplayer
fi

APP=target/release/bundle/AudioPlayer.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/AudioPlayer"
cp packaging/macos/AudioPlayer.icns "$APP/Contents/Resources/AudioPlayer.icns"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
# Signature ad hoc : suffisante pour ce Mac ; une distribution demandera un certificat Apple.
codesign --force --deep --sign - "$APP"

echo "$APP (version $VERSION, $(lipo -archs "$APP/Contents/MacOS/AudioPlayer"))"
echo "Installer : cp -R \"$APP\" /Applications/"
