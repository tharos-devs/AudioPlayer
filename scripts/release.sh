#!/usr/bin/env bash
# Publie une nouvelle version : scripts/release.sh [X.Y.Z]
# Sans argument, affiche la version en cours et demande la nouvelle. Écrit la version dans
# Cargo.toml (l'app l'affiche en bas à droite), commite, crée le tag vX.Y.Z et pousse : la CI
# (release.yml) construit alors les 3 installeurs, les dépose dans la Release GitHub, et les
# apps déjà installées proposent la mise à jour.
set -euo pipefail
cd "$(dirname "$0")/.."

current=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
version=${1:-}
if [ -z "$version" ]; then
    echo "Version actuelle : $current"
    read -r -p "Nouvelle version : " version
fi
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Version invalide : $version (attendu X.Y.Z)" >&2; exit 1; }
[ "$version" != "$current" ] || { echo "La version $version est déjà celle en cours" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && { echo "Le tag v$version existe déjà" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "Des modifications ne sont pas commitées" >&2; exit 1; }
branch=$(git branch --show-current)

# La première ligne `version =` est celle de [workspace.package], partagée par tous les crates.
perl -0pi -e 's/(\[workspace\.package\]\nversion = ")[^"]*"/${1}'"$version"'"/' Cargo.toml
written=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$written" = "$version" ] || { echo "Cargo.toml n'a pas pu être mis à jour (version lue : $written)" >&2; exit 1; }
cargo check --quiet -p audioplayer   # met Cargo.lock à jour
git commit -qam "Release v$version"
git tag -a "v$version" -m "AudioPlayer $version"

read -r -p "Pousser $branch et v$version vers origin (lance la publication) ? [o/N] " answer
if [[ $answer =~ ^[oOyY] ]]; then
    # Atomique : si la branche est refusée, le tag ne part pas (il publierait une Release
    # depuis un commit absent de la branche distante).
    git push --atomic origin "$branch" "v$version"
    echo "v$version poussée : suis la compilation dans l'onglet Actions du dépôt."
else
    echo "Rien n'est poussé. Plus tard : git push origin $branch v$version"
fi
