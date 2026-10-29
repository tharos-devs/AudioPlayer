# Dépendances corrigées localement

## i-slint-core 1.18.1 — inertie du défilement sur macOS

`vendor/i-slint-core` est une copie de `i-slint-core` 1.18.1 (crates.io), branchée via
`[patch.crates-io]` dans le `Cargo.toml` racine. Seul `items/flickable.rs` est modifié :
voir `i-slint-core-scroll-inertia.patch`.

**Problème.** Avec un pavé tactile ou une Magic Mouse, macOS envoie lui-même des
événements d'inertie (*momentum*) après le lever du doigt, puis un `Ended` quand on
repose le doigt pour arrêter. Slint lance en plus sa propre animation d'inertie à chaque
`Ended` : l'inertie est doublée, et reposer le doigt relance le défilement au lieu de
l'arrêter.

**Correctif.** Sur macOS uniquement, pas d'animation de Slint à la fin d'un geste
tactile ; l'inertie du système suffit. Windows et Linux ne fournissent pas d'inertie,
le comportement de Slint y est conservé.

**Mise à jour de Slint.** Recopier la nouvelle version de `i-slint-core` ici, réappliquer
le correctif (`patch -p1 -d vendor/i-slint-core < vendor/i-slint-core-scroll-inertia.patch`)
ou supprimer ce dossier et l'entrée `[patch.crates-io]` si Slint corrige le problème.

## block 0.1.6 — refusé par une future version de Rust

`vendor/block` est une copie de `block` 0.1.6 (crates.io), dépendance de souvlaki (touches
multimédia sur macOS), branchée via `[patch.crates-io]`. La bibliothèque n'est plus
maintenue (remplacée par `block2`).

**Problème.** `static _NSConcreteStackBlock: Class` avec `enum Class {}` : un `static` d'un
type sans valeur possible, que Rust signale comme « future incompatibility » et refusera
(rust-lang/rust#74840).

**Correctif.** `Class` devient un type opaque habité de taille nulle ; seule l'adresse du
symbole est utilisée, le comportement est inchangé. Les déclarations `extern` reçoivent
aussi leur ABI explicite (`"C"`, la valeur implicite), dépréciée sinon
(`block-future-incompat.patch`).

**À terme.** Remplacer souvlaki sur macOS par une intégration `objc2` (MPRemoteCommandCenter
et MPNowPlayingInfoCenter) permettrait de supprimer cette copie.
