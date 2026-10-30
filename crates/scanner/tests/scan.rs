use std::path::Path;
use std::sync::atomic::AtomicBool;

use audioplayer_db::Database;
use audioplayer_scanner::{scan, Error, Mode, Report};
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::tag::{Tag, TagType};

// WAV PCM 16 bits mono silencieux de 0,1 s.
fn wav(path: &Path) {
    let (rate, samples) = (8_000u32, 800u32);
    let data = samples * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    for v in [2u16, 16] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.resize(bytes.len() + data as usize, 0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn jpeg(size: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(size, size, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Jpeg).unwrap();
    bytes.into_inner()
}

fn tagged(path: &Path, title: &str, artist: &str, album_artist: Option<&str>, album: &str, track: u32, picture: Option<Vec<u8>>) {
    wav(path);
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.into());
    tag.set_artist(artist.into());
    tag.set_album(album.into());
    tag.set_track(track);
    tag.set_disk(1);
    tag.set_disk_total(2);
    tag.insert_text(ItemKey::Genre, "  ".into());
    if let Some(album_artist) = album_artist {
        tag.insert_text(ItemKey::AlbumArtist, album_artist.into());
    }
    if let Some(data) = picture {
        tag.push_picture(Picture::unchecked(data).pic_type(PictureType::CoverFront).mime_type(MimeType::Jpeg).build());
    }
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

fn run(db: &Path, library: i64, root: &Path, mode: Mode) -> Result<Report, Error> {
    scan(db, library, root, mode, &AtomicBool::new(false), &|_| {})
}

#[test]
fn scans_tags_artwork_changes_and_removals() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let cover = jpeg(150);
    tagged(&root.join("Williams/Jaws/01.wav"), "Main Title", "John Williams", None, "Jaws", 1, Some(cover.clone()));
    tagged(&root.join("Williams/Jaws/02.wav"), "Shark Cage", "LSO", Some("John Williams"), "Jaws", 2, None);
    tagged(&root.join("Zimmer/Dune/01.wav"), "Dream", "Hans Zimmer", None, "Dune", 1, None);
    std::fs::write(root.join("Zimmer/Dune/cover.jpg"), jpeg(400)).unwrap();
    wav(&root.join("Untagged/Live Set/track one.wav"));
    // Fichiers cachés et non audio : ignorés.
    std::fs::write(root.join("Williams/Jaws/._01.wav"), b"AppleDouble").unwrap();
    std::fs::write(root.join("Williams/Jaws/notes.txt"), b"x").unwrap();

    let library = Database::open(&db_path).unwrap().create_library("OST", &root.to_string_lossy()).unwrap().unwrap();
    let report = run(&db_path, library, &root, Mode::Changes).unwrap();
    assert_eq!((report.added, report.updated, report.removed, report.artworks), (4, 0, 0, 2), "{report:?}");

    let db = Database::open(&db_path).unwrap();
    let albums: Vec<_> = db.albums(library).unwrap().into_iter().map(|a| (a.title, a.track_count, a.disc_count, a.has_artwork)).collect();
    assert_eq!(
        albums,
        [
            ("Dune".to_string(), 1, 1, true),
            ("Jaws".to_string(), 2, 1, true),
            // Sans tags : titre du fichier, album du dossier.
            ("Live Set".to_string(), 1, 1, false),
        ]
    );
    // Album groupé par artiste d'album ; l'interprète (LSO) reste sur la piste mais sans album.
    let artists: Vec<_> = db.artists(library).unwrap().into_iter().map(|a| (a.name, a.album_count)).collect();
    assert_eq!(
        artists,
        [
            ("Hans Zimmer".to_string(), 1),
            ("John Williams".to_string(), 1),
            ("LSO".to_string(), 0),
            ("Unknown Artist".to_string(), 1),
        ]
    );
    let jaws = db.albums(library).unwrap().into_iter().find(|a| a.title == "Jaws").unwrap();
    // Pochette intégrée de 150px : gardée octet pour octet ; celle du dossier (400px) réduite.
    assert_eq!(db.artwork(jaws.id, 150).unwrap().unwrap().data, cover);
    let dune = db.albums(library).unwrap().into_iter().find(|a| a.title == "Dune").unwrap();
    assert_eq!(db.artwork(dune.id, 150).unwrap().unwrap().size, 150);
    let genre: Option<String> = db.connection().query_row("SELECT genre FROM tracks LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(genre, None, "blank tag stored as missing");

    // Rien n'a changé : rien n'est relu.
    let again = run(&db_path, library, &root, Mode::Changes).unwrap();
    assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 4));

    // Un fichier modifié est relu en place ; un fichier supprimé est retiré avec son album vide.
    let jaws_id = db.library_files(library).unwrap()[&root.join("Williams/Jaws/01.wav").to_string_lossy().into_owned()].track_id;
    db.record_play(jaws_id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    tagged(&root.join("Williams/Jaws/01.wav"), "Main Title (Remastered)", "John Williams", None, "Jaws", 1, None);
    std::fs::remove_file(root.join("Zimmer/Dune/01.wav")).unwrap();
    let changes = run(&db_path, library, &root, Mode::Changes).unwrap();
    assert_eq!((changes.updated, changes.removed, changes.unchanged), (1, 1, 2), "{changes:?}");
    let track = db.tracks(library).unwrap().into_iter().find(|t| t.id == jaws_id).unwrap();
    assert_eq!((track.title.as_str(), track.play_count), ("Main Title (Remastered)", 1));
    let titles: Vec<_> = db.albums(library).unwrap().into_iter().map(|a| a.title).collect();
    assert_eq!(titles, ["Jaws", "Live Set"]);
    assert!(!db.artists(library).unwrap().iter().any(|a| a.name == "Hans Zimmer"));

    // Disque débranché ou dossier vidé : erreur, rien n'est retiré.
    assert!(matches!(run(&db_path, library, &dir.path().join("Absent"), Mode::Changes), Err(Error::RootMissing(_))));
    let empty = dir.path().join("Empty");
    std::fs::create_dir(&empty).unwrap();
    assert!(matches!(run(&db_path, library, &empty, Mode::Changes), Err(Error::RootEmpty(_))));
    assert_eq!(db.tracks(library).unwrap().len(), 3);
}

#[test]
fn rescan_files_rereads_even_unchanged_and_drops_missing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let (one, two) = (root.join("A/One/01.wav"), root.join("B/Two/01.wav"));
    tagged(&one, "First", "A", None, "One", 1, None);
    tagged(&two, "Second", "B", None, "Two", 1, None);
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    // Année modifiée dans le fichier : la relecture la reprend (le scan normal garde la première connue).
    audioplayer_scanner::write::write_tags(&one, &[(audioplayer_scanner::write::Field::Year, Some("1990".into()))], None).unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();
    audioplayer_scanner::write::write_tags(&one, &[(audioplayer_scanner::write::Field::Year, Some("2001".into()))], None).unwrap();
    std::fs::remove_file(&two).unwrap();
    let report = audioplayer_scanner::rescan_files(&db_path, library, &[one, two], &AtomicBool::new(false), &|_| {}).unwrap();
    assert_eq!((report.updated, report.removed), (1, 1));
    let albums: Vec<_> = Database::open(&db_path).unwrap().albums(library).unwrap().into_iter().map(|a| (a.title, a.year)).collect();
    assert_eq!(albums, [("One".to_string(), Some(2001))]);
}

#[test]
fn writes_only_the_edited_tags_and_the_cover() {
    use audioplayer_scanner::files::AudioFile;
    use audioplayer_scanner::tags;
    use audioplayer_scanner::write::{write_tags, Field};
    use lofty::prelude::{Accessor, TaggedFileExt};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a/01.wav");
    tagged(&path, "Old Title", "Someone", Some("Composer"), "Album", 3, None);
    let file = |p: &Path| AudioFile { path: p.to_owned(), size: 0, mtime: 0 };

    let cover = jpeg(150);
    write_tags(
        &path,
        &[
            (Field::Title, Some("New Title".into())),
            (Field::Year, Some("1999".into())),
            (Field::Composer, Some("John Williams".into())),
            (Field::AlbumArtist, None),
            (Field::TrackNumber, Some("7".into())),
            (Field::TrackTotal, Some("12".into())),
            (Field::Compilation, Some("1".into())),
        ],
        Some(("image/jpeg", &cover)),
    )
    .unwrap();

    let read = tags::read(&file(&path)).unwrap();
    let r = &read.record;
    assert_eq!((r.title.as_str(), r.year, r.track_number), ("New Title", Some(1999), Some(7)));
    assert_eq!((r.composer.as_deref(), r.album_artist.as_deref()), (Some("John Williams"), None));
    // Champs non modifiés : intacts (album, artiste, disque 1/2 avec son total).
    assert_eq!((r.album.as_str(), r.artist.as_str(), r.disc_number), ("Album", "Someone", Some(1)));
    assert_eq!((r.track_total, r.disc_total, r.compilation), (Some(12), Some(2), true));
    write_tags(&path, &[(Field::Compilation, Some("0".into()))], None).unwrap();
    assert!(!tags::read(&file(&path)).unwrap().record.compilation);
    let tagged = lofty::read_from_path(&path).unwrap();
    let tag = tagged.primary_tag().unwrap();
    assert_eq!((tag.disk(), tag.disk_total()), (Some(1), Some(2)), "disc total kept");
    assert_eq!(read.picture.as_deref(), Some(cover.as_slice()));

    // Un nombre invalide est refusé sans rien écrire.
    assert!(write_tags(&path, &[(Field::Title, Some("X".into())), (Field::Year, Some("19x9".into()))], None).is_err());
    assert_eq!(tags::read(&file(&path)).unwrap().record.title, "New Title");
}

// Disque qui flanche : un fichier connu devenu illisible garde ce que la base en sait, et
// un dossier illisible empêche tout retrait (la liste est incomplète).
#[cfg(unix)]
#[test]
fn unreadable_files_and_folders_never_lose_data() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let (one, two) = (root.join("A/One/01.wav"), root.join("B/Two/01.wav"));
    tagged(&one, "First", "Artist A", None, "One", 1, None);
    tagged(&two, "Second", "Artist B", None, "Two", 1, None);
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    std::fs::write(&one, b"not audio any more").unwrap();
    let locked = root.join("B");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = run(&db_path, library, &root, Mode::Full);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    assert_eq!((report.unreadable, report.removed), (1, 0), "{report:?}");
    assert!(report.inaccessible > 0, "{report:?}");

    let db = Database::open(&db_path).unwrap();
    let mut titles: Vec<_> = db.tracks(library).unwrap().into_iter().map(|t| t.title).collect();
    titles.sort();
    assert_eq!(titles, ["First", "Second"]);
}

// Un scan complet reprend l'année des fichiers, même si l'album en avait déjà une.
#[test]
fn full_scan_refreshes_the_album_year() {
    use audioplayer_scanner::write::{write_tags, Field};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let path = root.join("A/One/01.wav");
    tagged(&path, "First", "Artist", None, "One", 1, None);
    write_tags(&path, &[(Field::Year, Some("1990".into()))], None).unwrap();
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();
    write_tags(&path, &[(Field::Year, Some("2004".into()))], None).unwrap();
    run(&db_path, library, &root, Mode::Full).unwrap();
    let years: Vec<_> = Database::open(&db_path).unwrap().albums(library).unwrap().into_iter().map(|a| a.year).collect();
    assert_eq!(years, [Some(2004)]);
}

// Fichier aux seuls tags RIFF (pas de tag principal ID3v2) : modifier un champ garde les autres.
#[test]
fn editing_a_file_without_primary_tag_keeps_its_other_tags() {
    use audioplayer_scanner::files::AudioFile;
    use audioplayer_scanner::tags;
    use audioplayer_scanner::write::{write_tags, Field};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a/01.wav");
    wav(&path);
    let mut riff = Tag::new(TagType::RiffInfo);
    riff.set_title("Riff Title".into());
    riff.set_artist("Riff Artist".into());
    riff.save_to_path(&path, WriteOptions::default()).unwrap();

    write_tags(&path, &[(Field::Genre, Some("Soundtrack".into()))], None).unwrap();
    let read = tags::read(&AudioFile { path: path.clone(), size: 0, mtime: 0 }).unwrap();
    let r = &read.record;
    assert_eq!((r.title.as_str(), r.artist.as_str(), r.genre.as_deref()), ("Riff Title", "Riff Artist", Some("Soundtrack")));
}

// Relecture ciblée avec le dossier d'un album introuvable (disque débranché) : rien n'est retiré.
#[test]
fn rescan_with_an_unreachable_folder_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let one = root.join("A/One/01.wav");
    tagged(&one, "First", "A", None, "One", 1, None);
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    std::fs::rename(root.join("A"), dir.path().join("elsewhere")).unwrap();
    let report = audioplayer_scanner::rescan_files(&db_path, library, &[one], &AtomicBool::new(false), &|_| {}).unwrap();
    assert_eq!((report.removed, report.inaccessible), (0, 1), "{report:?}");
    assert_eq!(Database::open(&db_path).unwrap().tracks(library).unwrap().len(), 1);
}

// Relecture ciblée d'une piste sans année : l'album garde l'année portée par une autre piste.
#[test]
fn targeted_rescan_rereads_the_whole_album() {
    use audioplayer_scanner::write::{write_tags, Field};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let (one, two) = (root.join("A/One/01.wav"), root.join("A/One/02.wav"));
    tagged(&one, "First", "Artist", None, "One", 1, None);
    tagged(&two, "Second", "Artist", None, "One", 2, None);
    write_tags(&one, &[(Field::Year, Some("1990".into()))], None).unwrap();
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    write_tags(&two, &[(Field::Title, Some("Second (edited)".into()))], None).unwrap();
    audioplayer_scanner::rescan_files(&db_path, library, &[two], &AtomicBool::new(false), &|_| {}).unwrap();
    let years: Vec<_> = Database::open(&db_path).unwrap().albums(library).unwrap().into_iter().map(|a| a.year).collect();
    assert_eq!(years, [Some(1990)]);
}

// Album renommé par un autre logiciel, dans un dossier qui avait déjà sa pochette : le nouvel
// album la retrouve (image du dossier).
#[test]
fn retagged_album_keeps_its_artwork() {
    use audioplayer_scanner::write::{write_tags, Field};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let path = root.join("A/One/01.wav");
    tagged(&path, "First", "Artist", None, "One", 1, None);
    std::fs::write(root.join("A/One/cover.jpg"), jpeg(300)).unwrap();
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    write_tags(&path, &[(Field::Album, Some("One (Remastered)".into()))], None).unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();
    let albums: Vec<_> = Database::open(&db_path).unwrap().albums(library).unwrap().into_iter().map(|a| (a.title, a.has_artwork)).collect();
    assert_eq!(albums, [("One (Remastered)".to_string(), true)]);
}

// Un dossier illisible ne bloque que les retraits sous lui : un fichier supprimé ailleurs part.
#[cfg(unix)]
#[test]
fn unreadable_folder_only_blocks_removals_below_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Music");
    let db_path = dir.path().join("library.db");
    let (gone, locked_file) = (root.join("A/One/01.wav"), root.join("B/Two/01.wav"));
    tagged(&gone, "Gone", "A", None, "One", 1, None);
    tagged(&root.join("A/One/02.wav"), "Stays", "A", None, "One", 2, None);
    tagged(&locked_file, "Kept", "B", None, "Two", 1, None);
    let library = Database::open(&db_path).unwrap().create_library("L", &root.to_string_lossy()).unwrap().unwrap();
    run(&db_path, library, &root, Mode::Changes).unwrap();

    std::fs::remove_file(&gone).unwrap();
    let locked = root.join("B");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = run(&db_path, library, &root, Mode::Changes);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.unwrap().removed, 1);
    let mut titles: Vec<_> = Database::open(&db_path).unwrap().tracks(library).unwrap().into_iter().map(|t| t.title).collect();
    titles.sort();
    assert_eq!(titles, ["Kept", "Stays"]);
}
