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
    for v in [16u32] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
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
    tag.insert_text(ItemKey::DiscNumber, "1/2".into());
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
