# AudioPlayer

Lecteur audio de bureau (macOS, Windows, Linux) écrit entièrement en Rust, interface en [Slint](https://slint.dev).
Refonte complète de la v0.1 (Tauri + Vue), conservée dans `../AudioPlayerOld` comme référence.

> État actuel : **prototype d'interface** branché sur une base SQLite importée de la v0.1.
> La lecture est simulée ; pas encore de scan ni d'audio. Les playlists ne sont pas enregistrées.

## Lancer

```bash
cargo run --release
```

Au premier lancement, AudioPlayer demande où créer la base (n'importe quel dossier, un disque
externe par exemple), ou en ouvre une existante, ou en crée une à partir de la base de la v0.1
(lue en lecture seule, jamais modifiée). Les réglages de l'interface sont enregistrés dans la base,
table `preferences`. Seul le chemin de la base est noté hors d'elle, dans
`~/Library/Application Support/AudioPlayer/database.cfg` (macOS) ; si la base est introuvable
(disque débranché), l'écran de démarrage propose de réessayer. *Fichier → Nouvelle base / Ouvrir
une base* change de base en cours de route.

`AUDIOPLAYER_DB=/chemin/base.db` impose une base pour un essai, sans rien enregistrer.

En ligne de commande, l'import de la v0.1 existe aussi :

```bash
cargo run --release -p audioplayer-db --bin import-v01 -- ancienne.db nouvelle.db
```

## Structure

```
crates/db/
  src/schema.rs      schéma versionné (PRAGMA user_version) et migrations
  src/catalog.rs     lectures du catalogue d'une bibliothèque
  src/legacy.rs      import de la base de la v0.1
  src/preferences.rs réglages (table preferences)
  src/bin/import-v01.rs
crates/app/
  src/main.rs        état de l'application, démarrage, branchement des callbacks
  src/location.rs    emplacement de la base choisi par l'utilisateur
  src/settings.rs    réglages de l'interface, rangés dans la base
  src/library.rs     catalogue de la bibliothèque courante, chargé en mémoire
  src/covers.rs      décodage des pochettes
  ui/app.slint       fenêtre principale
  ui/setup.slint     écran de démarrage (créer / ouvrir / importer une base)
  ui/theme.slint     thème clair/sombre + couleurs d'accent
  ui/widgets.slint   composants réutilisables
  ui/tracks.slint    table des morceaux (tri, glisser-déposer, menu contextuel)
  lang/<langue>/LC_MESSAGES/audioplayer.po   traductions
vendor/i-slint-core  Slint corrigé localement (inertie du défilement sur macOS), voir vendor/README.md
```

## Traductions

L'interface est en anglais. Chaque texte visible s'écrit `@tr("...")` dans les fichiers `.slint`,
y compris les messages formatés et les pluriels (`@tr("{n} track" | "{n} tracks" % count)`).
Les traductions gettext sont embarquées dans le binaire à la compilation.

Ajouter une langue :

```bash
cargo install slint-tr-extractor
find crates/app/ui -name '*.slint' | xargs slint-tr-extractor --no-default-translation-context -o audioplayer.pot
mkdir -p crates/app/lang/de/LC_MESSAGES
msginit -i audioplayer.pot -o crates/app/lang/de/LC_MESSAGES/audioplayer.po -l de
```

puis l'ajouter au menu *View → Language* dans `ui/app.slint`.

## Licence de Slint

Slint est utilisé sous licence *Royalty-free 2.0* : gratuit pour une application de bureau,
avec une mention « Made with Slint » à prévoir (dans la fenêtre *About* par exemple).
