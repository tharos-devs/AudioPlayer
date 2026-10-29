//! Import de la base de la v0.1 (Tauri) dans une base neuve.
//!
//! L'ancienne base est attachée en lecture seule et n'est jamais modifiée. Les
//! identifiants sont conservés, ce qui garde les relations telles quelles.

use std::path::Path;

use base64::Engine;
use rusqlite::params;

use crate::{Database, Error, Result};

#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    pub libraries: usize,
    pub artists: usize,
    pub albums: usize,
    pub tracks: usize,
    pub artworks: usize,
    pub skipped_artworks: usize,
    pub playlists: usize,
}

// Texte vide ou blanc → NULL.
fn text(column: &str) -> String {
    format!("NULLIF(trim(t.{column}), '')")
}

pub fn import(db: &mut Database, legacy: &Path) -> Result<ImportReport> {
    let has_data: bool = db.conn.query_row("SELECT EXISTS (SELECT 1 FROM libraries)", [], |row| row.get(0))?;
    if has_data {
        return Err(Error::TargetNotEmpty);
    }

    // ATTACH est interdit dans une transaction : on attache d'abord.
    db.conn.execute("ATTACH DATABASE ?1 AS old", [read_only_uri(legacy)])?;
    let report = copy(db);
    db.conn.execute("DETACH DATABASE old", [])?;
    report
}

// Le rapport se remplit au fil des étapes.
#[allow(clippy::field_reassign_with_default)]
fn copy(db: &mut Database) -> Result<ImportReport> {
    let tx = db.conn.transaction()?;
    let mut report = ImportReport::default();

    // Les dossiers ajoutés après coup sont des sous-dossiers de la racine : seule la racine compte.
    report.libraries = tx.execute(
        "INSERT INTO libraries (id, name, root_path, created_at, last_scan_at)
         SELECT l.id, l.name,
                (SELECT rtrim(p.path, '/') FROM old.library_paths p WHERE p.library_id = l.id
                 ORDER BY p.is_root DESC, length(p.path) LIMIT 1),
                coalesce(l.created_at, datetime('now')), l.last_scan
         FROM old.libraries l",
        [],
    )?;

    report.artists = tx.execute(
        "INSERT INTO artists (id, library_id, name, sort_name, favorited_at)
         SELECT id, library_id, name, NULLIF(NULLIF(trim(sort_name), ''), name), favorited_at
         FROM old.artists",
        [],
    )?;

    report.albums = tx.execute(
        "INSERT INTO albums (id, library_id, artist_id, title, sort_title, year, is_compilation,
                             folder_path, favorited_at, added_at)
         SELECT a.id, a.library_id, a.artist_id, a.title, NULLIF(NULLIF(trim(a.sort_title), ''), a.title),
                NULLIF(a.year, 0),
                coalesce((SELECT max(t.compilation) > 0 FROM old.tracks t WHERE t.album_id = a.id), 0),
                a.folder_path, a.favorited_at, coalesce(a.created_at, datetime('now'))
         FROM old.albums a",
        [],
    )?;

    let tags = ["genre", "composer", "conductor", "lyricist", "grouping", "work", "comment", "publisher",
        "catalog_number", "barcode", "disc_subtitle"];
    report.tracks = tx.execute(
        &format!(
            "INSERT INTO tracks (id, album_id, artist_id, title, disc_number, track_number, duration_ms,
                                 file_path, file_size, codec, bitrate, sample_rate, bit_depth, channels,
                                 bpm, play_count, last_played_at, rating, favorited_at, added_at,
                                 {tag_columns})
             SELECT t.id, t.album_id, t.artist_id, t.title, NULLIF(t.disc_number, 0), NULLIF(t.track_number, 0),
                    coalesce(t.duration, 0) * 1000, t.file_path, t.file_size,
                    NULLIF(NULLIF(trim(t.codec), ''), 'Unknown'), t.bitrate, t.sample_rate, t.bits_per_sample,
                    t.channels, NULLIF(t.bpm, 0), coalesce(t.play_count, 0), t.last_played, NULLIF(t.rating, 0),
                    t.favorited_at, coalesce(t.created_at, datetime('now')),
                    {tag_values}
             FROM old.tracks t",
            tag_columns = tags.join(", "),
            tag_values = tags.map(text).join(", "),
        ),
        [],
    )?;

    // L'ancienne app stockait du base64 ; `header_80` est la vignette de référence.
    {
        let mut select = tx.prepare(
            "SELECT album_id, max(width, height), base64_data FROM old.artworks
             ORDER BY album_id, type = 'header_80' DESC",
        )?;
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO artworks (album_id, size, mime, data) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            let (album_id, size, encoded): (i64, i64, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
            let decoded = decode_artwork(&encoded);
            match decoded.and_then(|data| image_mime(&data).map(|mime| (mime, data))) {
                Some((mime, data)) if size > 0 => {
                    report.artworks += insert.execute(params![album_id, size, mime, data])?;
                }
                _ => report.skipped_artworks += 1,
            }
        }
    }

    report.playlists = tx.execute(
        "INSERT INTO playlists (id, name, created_at, updated_at)
         SELECT id, name, coalesce(created_at, datetime('now')), coalesce(updated_at, created_at, datetime('now'))
         FROM old.playlists",
        [],
    )?;
    // Positions renumérotées sans trou.
    tx.execute(
        "INSERT INTO playlist_tracks (playlist_id, track_id, position, added_at)
         SELECT playlist_id, track_id,
                row_number() OVER (PARTITION BY playlist_id ORDER BY position, id) - 1,
                coalesce(added_at, datetime('now'))
         FROM old.playlist_tracks",
        [],
    )?;

    tx.commit()?;
    Ok(report)
}

fn decode_artwork(encoded: &str) -> Option<Vec<u8>> {
    // Tolère un éventuel préfixe « data:image/jpeg;base64, ».
    let payload = encoded.split_once("base64,").map_or(encoded, |(_, data)| data);
    base64::engine::general_purpose::STANDARD.decode(payload.trim()).ok()
}

fn image_mime(data: &[u8]) -> Option<&'static str> {
    match data {
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

fn read_only_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'%' | b'?' | b'#' | b' ' | 0x80.. => uri.push_str(&format!("%{byte:02X}")),
            _ => uri.push(byte as char),
        }
    }
    uri.push_str("?mode=ro");
    uri
}
