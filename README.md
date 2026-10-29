# AudioPlayer

Lecteur audio de bureau (macOS, Windows, Linux) écrit entièrement en Rust, interface en [Slint](https://slint.dev).
Refonte complète de la v0.1 (Tauri + Vue), conservée dans `../AudioPlayerOld` comme référence.

> État actuel : **prototype d'interface** branché sur une base SQLite importée de la v0.1.
> La lecture est simulée ; pas encore de scan ni d'audio. Les playlists ne sont pas enregistrées.

## Lancer

La base se trouve dans le dossier de données de l'utilisateur
(`~/Library/Application Support/AudioPlayer/library.db` sur macOS) ; la variable
`AUDIOPLAYER_DB` permet d'en désigner une autre.
Les réglages de l'interface (thème, accent, zoom, égaliseur, volume, bibliothèque ouverte,
taille de la fenêtre…) sont enregistrés à côté, dans `settings.toml`, à chaque changement ;
`AUDIOPLAYER_SETTINGS` permet d'utiliser un autre fichier.

Créer la base à partir de celle de la v0.1 (ouverte en lecture seule, jamais modifiée) :

```bash
cargo run --release -p audioplayer-db --bin import-v01 -- \
    "/Volumes/Transcend/Music Librairies/audioplayer.db" \
    ~/Library/Application\ Support/AudioPlayer/library.db
cargo run --release
```

## Structure

```
crates/db/
  src/schema.rs      schéma versionné (PRAGMA user_version) et migrations
  src/catalog.rs     lectures du catalogue d'une bibliothèque
  src/legacy.rs      import de la base de la v0.1
  src/bin/import-v01.rs
crates/app/
  src/main.rs        état de l'application, branchement des callbacks
  src/library.rs     catalogue de la bibliothèque courante, chargé en mémoire
  src/covers.rs      décodage des pochettes
  ui/app.slint       fenêtre principale
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
