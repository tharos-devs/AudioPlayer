//! Écriture des résultats d'un scan : morceaux mis à jour en place (id, écoutes, favoris
//! et playlists conservés), artistes et albums créés à la demande, orphelins retirés.

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::{Database, Result};

/// Un fichier lu par le scanner. Les textes vides sont déjà ramenés à `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackRecord {
    pub path: String,
    pub size: u64,
    /// Secondes depuis l'époque Unix.
    pub mtime: i64,
    pub title: String,
    /// Interprète du morceau.
    pub artist: String,
    /// Artiste qui regroupe l'album ; l'interprète s'il est absent.
    pub album_artist: Option<String>,
    pub album: String,
    pub year: Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_subtitle: Option<String>,
    pub duration_ms: u64,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub conductor: Option<String>,
    pub lyricist: Option<String>,
    pub grouping: Option<String>,
    pub work: Option<String>,
    pub comment: Option<String>,
    pub publisher: Option<String>,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
    pub bpm: Option<u32>,
    pub compilation: bool,
    pub folder: Option<String>,
}

/// Ce que la base sait déjà d'un fichier, pour décider s'il faut le relire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnownFile {
    pub track_id: i64,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
}

impl Database {
    /// `None` si le nom ou le dossier est déjà celui d'une autre bibliothèque.
    pub fn create_library(&self, name: &str, root: &str) -> Result<Option<i64>> {
        let inserted = self
            .conn
            .execute("INSERT OR IGNORE INTO libraries (name, root_path) VALUES (?1, ?2)", params![name, root])?;
        Ok((inserted == 1).then(|| self.conn.last_insert_rowid()))
    }

    pub fn library_files(&self, library_id: i64) -> Result<HashMap<String, KnownFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.file_path, t.id, t.file_size, t.file_mtime
             FROM tracks t JOIN albums al ON al.id = t.album_id WHERE al.library_id = ?1",
        )?;
        let rows = stmt.query_map([library_id], |row| {
            let size: Option<i64> = row.get(2)?;
            Ok((row.get(0)?, KnownFile { track_id: row.get(1)?, size: size.map(|s| s.max(0) as u64), mtime: row.get(3)? }))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Albums de la bibliothèque qui ont déjà une pochette d'au moins `size` pixels.
    pub fn albums_with_artwork(&self, library_id: i64, size: u32) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT w.album_id FROM artworks w JOIN albums al ON al.id = w.album_id
             WHERE al.library_id = ?1 AND w.size >= ?2",
        )?;
        let rows = stmt.query_map(params![library_id, size], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Dossiers des albums qui ont déjà une pochette d'au moins `size` pixels : le scan
    /// n'y cherche plus d'image.
    pub fn folders_with_artwork(&self, library_id: i64, size: u32) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT al.folder_path FROM albums al JOIN artworks w ON w.album_id = al.id
             WHERE al.library_id = ?1 AND w.size >= ?2 AND al.folder_path IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![library_id, size], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Écrit un lot dans une transaction ; renvoie l'album de chaque morceau, dans l'ordre.
    pub fn write_tracks(&mut self, library_id: i64, records: &[TrackRecord]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let mut albums = Vec::with_capacity(records.len());
        for record in records {
            albums.push(write_track(&tx, library_id, record)?);
        }
        tx.commit()?;
        Ok(albums)
    }

    /// Remplace toutes les pochettes de l'album (dont les 80px importés de la v0.1).
    /// `images` : `(côté, type MIME, octets)`.
    pub fn set_artwork(&mut self, album_id: i64, images: &[(u32, String, Vec<u8>)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM artworks WHERE album_id = ?1", [album_id])?;
        for (size, mime, data) in images {
            tx.execute(
                "INSERT INTO artworks (album_id, size, mime, data) VALUES (?1, ?2, ?3, ?4)",
                params![album_id, size, mime, data],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Retire des morceaux (fichiers disparus) puis les albums et artistes devenus vides.
    pub fn remove_tracks(&mut self, library_id: i64, track_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut delete = tx.prepare("DELETE FROM tracks WHERE id = ?1")?;
            for id in track_ids {
                delete.execute([id])?;
            }
        }
        remove_orphans(&tx, library_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Albums sans morceau, puis artistes sans album ni morceau (correctif 0.1.2 de la v0.1).
    pub fn remove_orphans(&mut self, library_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        remove_orphans(&tx, library_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Avant une relecture ciblée : l'année des albums de ces morceaux sera reprise des
    /// fichiers relus, au lieu de garder la première connue (année modifiée à la main).
    pub fn reset_album_years(&mut self, track_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut reset = tx.prepare("UPDATE albums SET year = NULL WHERE id = (SELECT album_id FROM tracks WHERE id = ?1)")?;
            for id in track_ids {
                reset.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_scanned(&self, library_id: i64) -> Result<()> {
        self.conn.execute("UPDATE libraries SET last_scan_at = datetime('now') WHERE id = ?1", [library_id])?;
        Ok(())
    }
}

fn remove_orphans(tx: &Transaction, library_id: i64) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM albums WHERE library_id = ?1 AND NOT EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = albums.id)",
        [library_id],
    )?;
    tx.execute(
        "DELETE FROM artists WHERE library_id = ?1
           AND NOT EXISTS (SELECT 1 FROM albums al WHERE al.artist_id = artists.id)
           AND NOT EXISTS (SELECT 1 FROM tracks t WHERE t.artist_id = artists.id)",
        [library_id],
    )?;
    Ok(())
}

fn artist_id(tx: &Transaction, library_id: i64, name: &str) -> rusqlite::Result<i64> {
    tx.execute("INSERT OR IGNORE INTO artists (library_id, name) VALUES (?1, ?2)", params![library_id, name])?;
    tx.query_row("SELECT id FROM artists WHERE library_id = ?1 AND name = ?2", params![library_id, name], |row| row.get(0))
}

fn write_track(tx: &Transaction, library_id: i64, r: &TrackRecord) -> rusqlite::Result<i64> {
    let album_artist = artist_id(tx, library_id, r.album_artist.as_deref().unwrap_or(&r.artist))?;
    let performer = artist_id(tx, library_id, &r.artist)?;

    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM albums WHERE library_id = ?1 AND artist_id = ?2 AND title = ?3",
            params![library_id, album_artist, r.album],
            |row| row.get(0),
        )
        .optional()?;
    // L'année manquante se complète ; une année déjà connue n'est pas écrasée par une autre plage.
    let album_id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE albums SET year = coalesce(year, ?2), is_compilation = max(is_compilation, ?3),
                                   folder_path = coalesce(?4, folder_path)
                 WHERE id = ?1",
                params![id, r.year, r.compilation, r.folder],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO albums (library_id, artist_id, title, year, is_compilation, folder_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![library_id, album_artist, r.album, r.year, r.compilation, r.folder],
            )?;
            tx.last_insert_rowid()
        }
    };

    // Mise à jour en place : l'id, les écoutes, les favoris et les playlists restent.
    // Chaque champ prend la valeur du fichier, y compris l'absence (correctif 0.0.10).
    tx.execute(
        "INSERT INTO tracks (album_id, artist_id, title, disc_number, disc_subtitle, track_number, duration_ms,
                             file_path, file_size, file_mtime, codec, bitrate, sample_rate, bit_depth, channels,
                             genre, composer, conductor, lyricist, grouping, work, comment, publisher,
                             catalog_number, barcode, bpm)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                 ?21, ?22, ?23, ?24, ?25, ?26)
         ON CONFLICT (file_path) DO UPDATE SET
             album_id = excluded.album_id, artist_id = excluded.artist_id, title = excluded.title,
             disc_number = excluded.disc_number, disc_subtitle = excluded.disc_subtitle,
             track_number = excluded.track_number, duration_ms = excluded.duration_ms,
             file_size = excluded.file_size, file_mtime = excluded.file_mtime, codec = excluded.codec,
             bitrate = excluded.bitrate, sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth,
             channels = excluded.channels, genre = excluded.genre, composer = excluded.composer,
             conductor = excluded.conductor, lyricist = excluded.lyricist, grouping = excluded.grouping,
             work = excluded.work, comment = excluded.comment, publisher = excluded.publisher,
             catalog_number = excluded.catalog_number, barcode = excluded.barcode, bpm = excluded.bpm",
        params![
            album_id,
            performer,
            r.title,
            r.disc_number,
            r.disc_subtitle,
            r.track_number,
            r.duration_ms as i64,
            r.path,
            r.size as i64,
            r.mtime,
            r.codec,
            r.bitrate,
            r.sample_rate,
            r.bit_depth,
            r.channels,
            r.genre,
            r.composer,
            r.conductor,
            r.lyricist,
            r.grouping,
            r.work,
            r.comment,
            r.publisher,
            r.catalog_number,
            r.barcode,
            r.bpm,
        ],
    )?;
    Ok(album_id)
}
