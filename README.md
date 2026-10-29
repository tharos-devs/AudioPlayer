# AudioPlayer

Lecteur audio de bureau (macOS, Windows, Linux) écrit entièrement en Rust, interface en [Slint](https://slint.dev).
Refonte complète de la v0.1 (Tauri + Vue), conservée dans `../AudioPlayerOld` comme référence.

> État actuel : **prototype d'interface**. La bibliothèque est générée (≈13 000 morceaux),
> la lecture est simulée. Pas encore de base SQLite, de scan ni d'audio.

## Lancer

```bash
cargo run --release
```

## Structure

```
crates/app/
  src/main.rs        état de l'application, branchement des callbacks
  src/library.rs     bibliothèque factice (temporaire)
  ui/app.slint       fenêtre principale
  ui/theme.slint     thème clair/sombre + couleurs d'accent
  ui/widgets.slint   composants réutilisables
  ui/tracks.slint    table des morceaux (tri, glisser-déposer, menu contextuel)
  lang/<langue>/LC_MESSAGES/audioplayer.po   traductions
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
