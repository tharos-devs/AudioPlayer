#!/bin/bash
# Paquet d'installation macOS : dist/AudioPlayer-<version>.pkg, app universelle
# (Apple Silicon + Intel) installée dans /Applications.
#
# Affiche la version en cours et demande la nouvelle (Entrée garde l'actuelle) ; une
# nouvelle version est écrite dans Cargo.toml, d'où l'app la tire pour la barre d'état.
#   --yes             garde la version sans rien demander (CI)
#   --version X.Y.Z   fixe la version sans rien demander
set -euo pipefail
cd "$(dirname "$0")/../.."

current=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
version=$current
case "${1:-}" in
    --yes) ;;
    --version) version=${2:?"--version demande un numéro"} ;;
    "")
        echo "Version actuelle : $current"
        read -r -p "Nouvelle version [$current] : " answer
        version=${answer:-$current}
        ;;
    *) echo "usage : $0 [--yes | --version X.Y.Z]" >&2; exit 2 ;;
esac

# Chiffres seulement : macOS refuse « 1.2.3-beta » dans CFBundleVersion.
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Version invalide : $version (attendu : 1.2.3)" >&2
    exit 1
fi
if [ "$version" != "$current" ]; then
    # La première ligne `version =` est celle de [workspace.package], partagée par tous les crates.
    perl -0pi -e 's/(\[workspace\.package\]\nversion = ")[^"]*"/${1}'"$version"'"/' Cargo.toml
    # La réécriture ne doit pas avoir échoué en silence (Cargo.toml réorganisé) : le paquet
    # porterait la nouvelle version et l'app l'ancienne.
    written=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    if [ "$written" != "$version" ]; then
        echo "Cargo.toml n'a pas pu être mis à jour (version lue : $written)" >&2
        exit 1
    fi
    cargo check --quiet -p audioplayer   # met Cargo.lock à jour (les builds sont en --locked)
    echo "Cargo.toml : $current -> $version"
fi

packaging/macos/bundle.sh --universal

mkdir -p dist
pkg=dist/AudioPlayer-$version.pkg
root=$(mktemp -d)
trap 'rm -rf "$root" "$root.plist"' EXIT
cp -R target/release/bundle/AudioPlayer.app "$root/"
# Sans cela, l'installeur mettrait à jour une autre copie de l'app trouvée ailleurs sur
# le disque (target/…) au lieu de l'installer dans /Applications.
pkgbuild --analyze --root "$root" "$root.plist" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$root.plist"
# Toujours installer, même par-dessus une app de numéro plus élevé (l'ancienne v0.1 en 0.1.5).
plutil -replace 0.BundleIsVersionChecked -bool NO "$root.plist"
# postinstall : retire la quarantaine et vérifie la signature de l'app installée (comme la v0.1).
pkgbuild --root "$root" --component-plist "$root.plist" --install-location /Applications \
    --scripts packaging/macos/scripts \
    --identifier com.audioplayer.player --version "$version" "$pkg"

echo "$pkg"
