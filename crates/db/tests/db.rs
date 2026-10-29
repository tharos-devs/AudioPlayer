use std::path::Path;

use audioplayer_db::{inspect, legacy, schema, Database, Error, FileKind};
use rusqlite::{params, Connection};

fn sample(db: &Database) {
    db.connection()
        .execute_batch(
            "INSERT INTO libraries (id, name, root_path) VALUES (1, 'OST', '/music/ost'), (2, 'CLASSIC', '/music/classic');
             INSERT INTO artists (id, library_id, name) VALUES (1, 1, 'john Williams'), (2, 1, 'Alan Silvestri'), (3, 2, 'John Williams');
             INSERT INTO albums (id, library_id, artist_id, title, year) VALUES (10, 1, 1, 'Jaws', 1975), (11, 1, 2, 'Back to the Future', 1985), (12, 2, 3, 'Jaws', 1975);
             INSERT INTO tracks (id, album_id, artist_id, title, disc_number, track_number, duration_ms, file_path) VALUES
                 (100, 10, 1, 'Main Title', 1, 1, 135000, '/music/ost/jaws/01.flac'),
                 (101, 10, 1, 'Bonus', 2, 1, 60000, '/music/ost/jaws/201.flac'),
                 (102, 10, 1, 'Chrissie', 1, 2, 90000, '/music/ost/jaws/02.flac'),
                 (103, 11, 2, 'Overture', NULL, NULL, 0, '/music/ost/bttf/01.flac'),
                 (104, 12, 3, 'Main Title', 1, 1, 135000, '/music/classic/jaws/01.flac');
             INSERT INTO artworks (album_id, size, mime, data) VALUES (10, 80, 'image/jpeg', x'01'), (10, 600, 'image/jpeg', x'02');",
        )
        .unwrap();
}

#[test]
fn migrations_are_applied_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    drop(Database::open(&path).unwrap());
    let db = Database::open(&path).unwrap();
    assert_eq!(schema::version(db.connection()).unwrap(), schema::MIGRATIONS.len());
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    Connection::open(&path).unwrap().pragma_update(None, "user_version", 999).unwrap();
    assert!(matches!(Database::open(&path), Err(Error::SchemaTooNew { found: 999, .. })));
}

#[test]
fn album_cannot_borrow_an_artist_from_another_library() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    let result = db
        .connection()
        .execute("INSERT INTO albums (library_id, artist_id, title) VALUES (2, 1, 'Leak')", []);
    assert!(result.is_err());
}

#[test]
fn deleting_a_library_removes_its_whole_catalogue() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    db.connection().execute("DELETE FROM libraries WHERE id = 1", []).unwrap();
    let count = |table: &str| -> i64 {
        db.connection().query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).unwrap()
    };
    assert_eq!((count("artists"), count("albums"), count("tracks"), count("artworks")), (1, 1, 1, 0));
}

#[test]
fn catalogue_reads_are_scoped_and_ordered() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);

    let libraries: Vec<_> = db.libraries().unwrap().into_iter().map(|l| l.name).collect();
    assert_eq!(libraries, ["CLASSIC", "OST"]);

    let artists = db.artists(1).unwrap();
    let names: Vec<_> = artists.iter().map(|a| (a.name.as_str(), a.album_count)).collect();
    assert_eq!(names, [("Alan Silvestri", 1), ("john Williams", 1)]);

    let albums = db.albums(1).unwrap();
    let jaws = albums.iter().find(|a| a.id == 10).unwrap();
    assert_eq!((jaws.track_count, jaws.disc_count, jaws.has_artwork), (3, 2, true));
    let bttf = albums.iter().find(|a| a.id == 11).unwrap();
    assert_eq!((bttf.disc_count, bttf.has_artwork), (1, false));

    let tracks: Vec<_> = db.tracks(1).unwrap().into_iter().map(|t| t.id).collect();
    assert_eq!(tracks, [100, 102, 101, 103]);

    let cutoff = db.datetime_from_now("-14 days").unwrap();
    assert!(db.tracks(1).unwrap().iter().all(|t| t.added_at > cutoff));
}

#[test]
fn artwork_picks_the_smallest_sufficient_size() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    let size = |min| db.artwork(10, min).unwrap().map(|a| a.size);
    assert_eq!(size(40), Some(80));
    assert_eq!(size(160), Some(600));
    assert_eq!(size(1200), Some(600));
    assert_eq!(db.artwork(11, 80).unwrap(), None);

    let all: Vec<_> = db.library_artworks(1, 160).unwrap().into_iter().map(|(album, a)| (album, a.size)).collect();
    assert_eq!(all, [(10, 600)]);
    assert!(db.library_artworks(2, 160).unwrap().is_empty());
}

// Sous-ensemble du schéma de la v0.1, avec ses colonnes ajoutées après coup.
const LEGACY_SCHEMA: &str = "
CREATE TABLE libraries (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP, last_scan DATETIME);
CREATE TABLE library_paths (id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL, path TEXT NOT NULL, is_root BOOLEAN DEFAULT TRUE);
CREATE TABLE artists (id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL, name TEXT NOT NULL, sort_name TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP, favorited_at DATETIME);
CREATE TABLE albums (id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL, artist_id INTEGER NOT NULL, title TEXT NOT NULL,
    sort_title TEXT, year INTEGER, total_discs INTEGER, total_tracks INTEGER, folder_path TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP, favorited_at DATETIME);
CREATE TABLE tracks (id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL, artist_id INTEGER NOT NULL, album_id INTEGER NOT NULL,
    title TEXT NOT NULL, track_number INTEGER, duration INTEGER, file_path TEXT NOT NULL UNIQUE, file_size INTEGER,
    bitrate INTEGER, sample_rate INTEGER, channels INTEGER, bits_per_sample INTEGER, codec TEXT, genre TEXT, composer TEXT,
    disc_number INTEGER, total_discs INTEGER, play_count INTEGER DEFAULT 0, last_played DATETIME, rating INTEGER,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP, last_modified DATETIME DEFAULT CURRENT_TIMESTAMP, bpm INTEGER,
    album_artist TEXT, comment TEXT, grouping TEXT, conductor TEXT, lyricist TEXT, publisher TEXT, sort_album_artist TEXT,
    description TEXT, catalog_number TEXT, disc_subtitle TEXT, barcode TEXT, work TEXT, compilation INTEGER DEFAULT 0,
    favorited_at DATETIME);
CREATE TABLE artworks (id INTEGER PRIMARY KEY, album_id INTEGER NOT NULL, type TEXT NOT NULL, width INTEGER NOT NULL,
    height INTEGER NOT NULL, base64_data TEXT NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE playlists (id INTEGER PRIMARY KEY, name TEXT NOT NULL, description TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE playlist_tracks (id INTEGER PRIMARY KEY, playlist_id INTEGER NOT NULL, track_id INTEGER NOT NULL,
    position INTEGER NOT NULL, added_at DATETIME DEFAULT CURRENT_TIMESTAMP);
";

// « /9j/4A== » : début d'un JPEG (FF D8 FF E0) en base64.
fn legacy_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(LEGACY_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO libraries (id, name, last_scan) VALUES (1, 'OST', '2026-01-02 03:04:05');
         INSERT INTO library_paths (library_id, path, is_root) VALUES
             (1, '/music/ost/A/Alan Silvestri/Play Dirty', 0), (1, '/music/ost/', 1);
         INSERT INTO artists (id, library_id, name, sort_name, favorited_at) VALUES (5, 1, 'Bear McCreary', '', '2026-02-01 10:00:00');
         INSERT INTO albums (id, library_id, artist_id, title, year, total_discs) VALUES (7, 1, 5, 'Agents of S.H.I.E.L.D.', 0, 1);
         INSERT INTO tracks (id, library_id, artist_id, album_id, title, track_number, duration, file_path, codec, genre,
                             composer, disc_number, play_count, rating, compilation) VALUES
             (9, 1, 5, 7, 'Paris Part I', 3, 56, '/music/ost/shield/03.flac', 'Unknown', '', 'Bear McCreary', 1, 4, 0, 0),
             (8, 1, 5, 7, 'Marvel Logo', 1, 33, '/music/ost/shield/01.flac', 'FLAC', 'Soundtrack', NULL, 1, NULL, 3, 1);
         INSERT INTO artworks (album_id, type, width, height, base64_data) VALUES
             (7, 'cover', 80, 80, 'not base64!'), (7, 'header_80', 80, 80, 'data:image/jpeg;base64,/9j/4A==');
         INSERT INTO playlists (id, name) VALUES (2, 'Best of');
         INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (2, 9, 10), (2, 8, 4);",
    )
    .unwrap();
}

#[test]
fn legacy_import_cleans_and_keeps_ids() {
    // Espace et accent dans le chemin : l'URI d'ATTACH doit les encoder.
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("Music Librairies é").join("audioplayer.db");
    std::fs::create_dir(old_path.parent().unwrap()).unwrap();
    legacy_db(&old_path);
    let before = std::fs::read(&old_path).unwrap();

    let mut db = Database::open_in_memory().unwrap();
    let report = legacy::import(&mut db, &old_path).unwrap();
    assert_eq!(
        report,
        legacy::ImportReport {
            libraries: 1,
            artists: 1,
            albums: 1,
            tracks: 2,
            artworks: 1,
            skipped_artworks: 1,
            playlists: 1
        }
    );
    assert_eq!(std::fs::read(&old_path).unwrap(), before);

    let library = &db.libraries().unwrap()[0];
    assert_eq!((library.id, library.root_path.as_str()), (1, "/music/ost"));

    let artist = &db.artists(1).unwrap()[0];
    assert!(artist.is_favorite);
    let album = &db.albums(1).unwrap()[0];
    assert_eq!((album.id, album.year, album.has_artwork), (7, None, true));
    assert_eq!(db.artwork(7, 80).unwrap().unwrap().mime, "image/jpeg");

    let conn = db.connection();
    let compilation: bool = conn.query_row("SELECT is_compilation FROM albums WHERE id = 7", [], |r| r.get(0)).unwrap();
    assert!(compilation);
    let row = |id: i64| -> (i64, Option<String>, Option<String>, i64, Option<i64>) {
        conn.query_row(
            "SELECT duration_ms, codec, genre, play_count, rating FROM tracks WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    };
    assert_eq!(row(9), (56_000, None, None, 4, None));
    assert_eq!(row(8), (33_000, Some("FLAC".into()), Some("Soundtrack".into()), 0, Some(3)));

    let order: Vec<(i64, i64)> = conn
        .prepare("SELECT track_id, position FROM playlist_tracks ORDER BY position")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(order, [(8, 0), (9, 1)]);

    assert!(matches!(legacy::import(&mut db, &old_path), Err(Error::TargetNotEmpty)));
}

#[test]
fn preferences_are_upserted() {
    let db = Database::open_in_memory().unwrap();
    assert_eq!(db.preference("appearance").unwrap(), None);
    db.set_preference("appearance", r#"{"dark":true}"#).unwrap();
    db.set_preference("appearance", r#"{"dark":false}"#).unwrap();
    assert_eq!(db.preference("appearance").unwrap().as_deref(), Some(r#"{"dark":false}"#));
}

#[test]
fn inspect_tells_files_apart_without_touching_them() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.db");
    assert_eq!(inspect(&missing).unwrap(), None);

    let current = dir.path().join("current.db");
    drop(Database::open(&current).unwrap());
    assert_eq!(inspect(&current).unwrap(), Some(FileKind::Current { version: schema::MIGRATIONS.len() }));

    let old = dir.path().join("old.db");
    legacy_db(&old);
    let before = std::fs::read(&old).unwrap();
    assert_eq!(inspect(&old).unwrap(), Some(FileKind::Legacy));
    assert_eq!(std::fs::read(&old).unwrap(), before);

    let text = dir.path().join("notes.db");
    std::fs::write(&text, "not a database").unwrap();
    assert_eq!(inspect(&text).unwrap(), Some(FileKind::Unknown));

    let empty = dir.path().join("empty.db");
    std::fs::write(&empty, "").unwrap();
    assert_eq!(inspect(&empty).unwrap(), Some(FileKind::Current { version: 0 }));
}

#[test]
fn record_play_counts_and_dates() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    db.record_play(100).unwrap();
    db.record_play(100).unwrap();
    let track = db.tracks(1).unwrap().into_iter().find(|t| t.id == 100).unwrap();
    assert_eq!(track.play_count, 2);
    let played: Option<String> = db.connection().query_row("SELECT last_played_at FROM tracks WHERE id = 100", [], |r| r.get(0)).unwrap();
    assert!(played.is_some());
}

#[test]
fn playlists_belong_to_one_library() {
    let mut db = Database::open_in_memory().unwrap();
    sample(&db);
    let mix = db.create_playlist(1, "Mix").unwrap().unwrap();
    assert_eq!(db.create_playlist(1, "Mix").unwrap(), None, "name already used in this library");
    assert!(db.create_playlist(2, "Mix").unwrap().is_some(), "same name in another library is fine");

    assert_eq!(db.add_to_playlist(mix, &[102, 100, 102]).unwrap(), 2);
    assert_eq!(db.add_to_playlist(mix, &[100, 103]).unwrap(), 1);
    assert_eq!(db.playlist_tracks(mix).unwrap(), [102, 100, 103]);
    // Morceau 104 : bibliothèque CLASSIC, refusé par le garde-fou.
    assert!(db.add_to_playlist(mix, &[104]).is_err());
    assert_eq!(db.playlist_tracks(mix).unwrap(), [102, 100, 103]);

    db.remove_from_playlist(mix, &[100]).unwrap();
    assert_eq!(db.playlist_tracks(mix).unwrap(), [102, 103]);
    let positions: Vec<i64> = db
        .connection()
        .prepare("SELECT position FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position")
        .unwrap()
        .query_map([mix], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(positions, [0, 1]);

    let listed: Vec<_> = db.playlists(1).unwrap().into_iter().map(|p| (p.name, p.track_count)).collect();
    assert_eq!(listed, [("Mix".to_string(), 2)]);
    let other = db.create_playlist(1, "Other").unwrap().unwrap();
    assert!(!db.rename_playlist(other, "Mix").unwrap());
    assert!(db.rename_playlist(other, "Road").unwrap());
    db.delete_playlist(mix).unwrap();
    assert_eq!(db.playlists(1).unwrap().len(), 1);
    // Supprimer la bibliothèque emporte ses playlists.
    db.connection().execute("DELETE FROM libraries WHERE id = 1", []).unwrap();
    assert!(db.playlists(1).unwrap().is_empty());
}

#[test]
fn favorite_toggles() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    assert!(db.toggle_track_favorite(100).unwrap());
    assert!(db.tracks(1).unwrap().iter().any(|t| t.id == 100 && t.is_favorite));
    assert!(!db.toggle_track_favorite(100).unwrap());
}

#[test]
fn v2_playlists_are_attached_to_their_tracks_library() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(schema::MIGRATIONS[0]).unwrap();
        tx.execute_batch(schema::MIGRATIONS[1]).unwrap();
        tx.pragma_update(None, "user_version", 2).unwrap();
        tx.commit().unwrap();
        conn.execute_batch(
            "INSERT INTO libraries (id, name, root_path) VALUES (1, 'OST', '/ost'), (2, 'CLASSIC', '/classic');
             INSERT INTO artists (id, library_id, name) VALUES (1, 1, 'A'), (2, 2, 'B');
             INSERT INTO albums (id, library_id, artist_id, title) VALUES (10, 1, 1, 'X'), (20, 2, 2, 'Y');
             INSERT INTO tracks (id, album_id, artist_id, title, file_path) VALUES
                 (1, 20, 2, 'c1', '/classic/1'), (2, 10, 1, 'o1', '/ost/1'), (3, 20, 2, 'c2', '/classic/2');
             INSERT INTO playlists (id, name) VALUES (5, 'Mostly classic'), (6, 'Empty');
             INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (5, 1, 3), (5, 2, 7), (5, 3, 9);",
        )
        .unwrap();
    }
    let db = Database::open(&path).unwrap();
    assert_eq!(schema::version(db.connection()).unwrap(), schema::MIGRATIONS.len());
    let classic: Vec<_> = db.playlists(2).unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(classic, ["Mostly classic"]);
    // Le morceau d'OST est écarté, les positions sont renumérotées.
    assert_eq!(db.playlist_tracks(5).unwrap(), [1, 3]);
    // Playlist vide : première bibliothèque.
    let ost: Vec<_> = db.playlists(1).unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(ost, ["Empty"]);
}

#[test]
fn libraries_rename_and_delete() {
    let db = Database::open_in_memory().unwrap();
    sample(&db);
    assert!(!db.rename_library(1, "CLASSIC").unwrap());
    assert!(db.rename_library(1, "Soundtracks").unwrap());
    db.delete_library(2).unwrap();
    let names: Vec<_> = db.libraries().unwrap().into_iter().map(|l| l.name).collect();
    assert_eq!(names, ["Soundtracks"]);
    assert!(db.artists(2).unwrap().is_empty() && db.tracks(2).unwrap().is_empty());
}
