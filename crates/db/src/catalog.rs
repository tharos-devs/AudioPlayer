//! Lectures du catalogue d'une bibliothèque, dans l'ordre d'affichage.

use rusqlite::{params, OptionalExtension};

use crate::{Database, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub root_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artist {
    pub id: i64,
    pub name: String,
    pub album_count: u32,
    pub is_favorite: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub id: i64,
    pub artist_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub track_count: u32,
    pub disc_count: u32,
    pub has_artwork: bool,
    pub is_favorite: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: i64,
    pub album_id: i64,
    pub artist_id: i64,
    pub title: String,
    pub disc_number: Option<u32>,
    pub track_number: Option<u32>,
    pub duration_ms: u64,
    pub file_path: String,
    pub play_count: u32,
    pub is_favorite: bool,
    pub added_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artwork {
    pub size: u32,
    pub mime: String,
    pub data: Vec<u8>,
}

impl Database {
    pub fn libraries(&self) -> Result<Vec<Library>> {
        let mut stmt = self.conn.prepare("SELECT id, name, root_path FROM libraries ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |row| Ok(Library { id: row.get(0)?, name: row.get(1)?, root_path: row.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn artists(&self, library_id: i64) -> Result<Vec<Artist>> {
        let mut stmt = self.conn.prepare(
            "SELECT ar.id, ar.name, ar.favorited_at IS NOT NULL,
                    (SELECT count(*) FROM albums al WHERE al.artist_id = ar.id)
             FROM artists ar
             WHERE ar.library_id = ?1
             ORDER BY coalesce(ar.sort_name, ar.name) COLLATE NOCASE, ar.id",
        )?;
        let rows = stmt.query_map([library_id], |row| {
            Ok(Artist { id: row.get(0)?, name: row.get(1)?, is_favorite: row.get(2)?, album_count: row.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn albums(&self, library_id: i64) -> Result<Vec<Album>> {
        let mut stmt = self.conn.prepare(
            "SELECT al.id, al.artist_id, al.title, al.year, al.favorited_at IS NOT NULL,
                    count(t.id), max(coalesce(t.disc_number, 1)),
                    EXISTS (SELECT 1 FROM artworks w WHERE w.album_id = al.id)
             FROM albums al
             LEFT JOIN tracks t ON t.album_id = al.id
             WHERE al.library_id = ?1
             GROUP BY al.id
             ORDER BY coalesce(al.sort_title, al.title) COLLATE NOCASE, al.id",
        )?;
        let rows = stmt.query_map([library_id], |row| {
            Ok(Album {
                id: row.get(0)?,
                artist_id: row.get(1)?,
                title: row.get(2)?,
                year: row.get(3)?,
                is_favorite: row.get(4)?,
                track_count: row.get(5)?,
                disc_count: row.get::<_, Option<u32>>(6)?.unwrap_or(1),
                has_artwork: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Morceaux de la bibliothèque, triés par album puis disque et numéro.
    pub fn tracks(&self, library_id: i64) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.album_id, t.artist_id, t.title, t.disc_number, t.track_number,
                    t.duration_ms, t.file_path, t.play_count, t.favorited_at IS NOT NULL, t.added_at
             FROM tracks t
             JOIN albums al ON al.id = t.album_id
             WHERE al.library_id = ?1
             ORDER BY t.album_id, coalesce(t.disc_number, 1), t.track_number IS NULL, t.track_number,
                      t.title COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([library_id], |row| {
            Ok(Track {
                id: row.get(0)?,
                album_id: row.get(1)?,
                artist_id: row.get(2)?,
                title: row.get(3)?,
                disc_number: row.get(4)?,
                track_number: row.get(5)?,
                duration_ms: row.get::<_, i64>(6)?.max(0) as u64,
                file_path: row.get(7)?,
                play_count: row.get(8)?,
                is_favorite: row.get(9)?,
                added_at: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Une lecture de plus, comptée au lancement du morceau comme dans la v0.1.
    pub fn record_play(&self, track_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played_at = datetime('now') WHERE id = ?1",
            [track_id],
        )?;
        Ok(())
    }

    /// Date UTC décalée depuis maintenant, au format des colonnes `*_at` ;
    /// `modifier` suit la syntaxe SQLite, par exemple `"-14 days"`.
    pub fn datetime_from_now(&self, modifier: &str) -> Result<String> {
        Ok(self.conn.query_row("SELECT datetime('now', ?1)", [modifier], |row| row.get(0))?)
    }

    /// Pochettes de tous les albums d'une bibliothèque, une par album, choisies
    /// comme [`Database::artwork`].
    pub fn library_artworks(&self, library_id: i64, min_size: u32) -> Result<Vec<(i64, Artwork)>> {
        let mut stmt = self.conn.prepare(
            "SELECT album_id, size, mime, data FROM (
                 SELECT w.album_id, w.size, w.mime, w.data,
                        row_number() OVER (PARTITION BY w.album_id
                                           ORDER BY w.size < ?2, CASE WHEN w.size >= ?2 THEN w.size ELSE -w.size END) AS rank
                 FROM artworks w JOIN albums al ON al.id = w.album_id
                 WHERE al.library_id = ?1)
             WHERE rank = 1",
        )?;
        let rows = stmt.query_map(params![library_id, min_size], |row| {
            Ok((row.get(0)?, Artwork { size: row.get(1)?, mime: row.get(2)?, data: row.get(3)? }))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// La plus petite pochette d'au moins `min_size` pixels, sinon la plus grande disponible.
    pub fn artwork(&self, album_id: i64, min_size: u32) -> Result<Option<Artwork>> {
        let artwork = self
            .conn
            .query_row(
                "SELECT size, mime, data FROM artworks WHERE album_id = ?1
                 ORDER BY size < ?2, CASE WHEN size >= ?2 THEN size ELSE -size END
                 LIMIT 1",
                params![album_id, min_size],
                |row| Ok(Artwork { size: row.get(0)?, mime: row.get(1)?, data: row.get(2)? }),
            )
            .optional()?;
        Ok(artwork)
    }
}

/// Tous les champs d'un morceau, pour le tiroir d'informations.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackDetails {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub year: Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_total: Option<u32>,
    pub disc_subtitle: Option<String>,
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
    pub compilation: bool,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub duration_ms: u64,
    pub file_size: Option<u64>,
}

impl Database {
    /// Genres déjà présents dans la bibliothèque, pour les suggestions de saisie.
    pub fn genres(&self, library_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT t.genre FROM tracks t JOIN albums al ON al.id = t.album_id
             WHERE al.library_id = ?1 AND t.genre IS NOT NULL AND t.genre <> '' ORDER BY t.genre",
        )?;
        let rows = stmt.query_map([library_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Détails de morceaux, dans l'ordre des ids demandés (ids inconnus ignorés).
    pub fn track_details(&self, ids: &[i64]) -> Result<Vec<TrackDetails>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.id, t.file_path, t.title, ar.name, aa.name, al.title, al.year, t.track_number, t.disc_number,
                    t.disc_subtitle, t.genre, t.composer, t.conductor, t.lyricist, t.grouping, t.work, t.comment,
                    t.publisher, t.catalog_number, t.barcode, al.is_compilation, t.codec, t.bitrate, t.sample_rate,
                    t.bit_depth, t.channels, t.duration_ms, t.file_size, t.track_total, t.disc_total
             FROM tracks t
             JOIN artists ar ON ar.id = t.artist_id
             JOIN albums al ON al.id = t.album_id
             JOIN artists aa ON aa.id = al.artist_id
             WHERE t.id = ?1",
        )?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let row = stmt
                .query_row([id], |r| {
                    Ok(TrackDetails {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        title: r.get(2)?,
                        artist: r.get(3)?,
                        album_artist: r.get(4)?,
                        album: r.get(5)?,
                        year: r.get(6)?,
                        track_number: r.get(7)?,
                        disc_number: r.get(8)?,
                        disc_subtitle: r.get(9)?,
                        genre: r.get(10)?,
                        composer: r.get(11)?,
                        conductor: r.get(12)?,
                        lyricist: r.get(13)?,
                        grouping: r.get(14)?,
                        work: r.get(15)?,
                        comment: r.get(16)?,
                        publisher: r.get(17)?,
                        catalog_number: r.get(18)?,
                        barcode: r.get(19)?,
                        compilation: r.get(20)?,
                        codec: r.get(21)?,
                        bitrate: r.get(22)?,
                        sample_rate: r.get(23)?,
                        bit_depth: r.get(24)?,
                        channels: r.get(25)?,
                        duration_ms: r.get::<_, i64>(26)?.max(0) as u64,
                        file_size: r.get::<_, Option<i64>>(27)?.map(|s| s.max(0) as u64),
                        track_total: r.get(28)?,
                        disc_total: r.get(29)?,
                    })
                })
                .optional()?;
            out.extend(row);
        }
        Ok(out)
    }
}
