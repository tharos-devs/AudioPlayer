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

    /// Date UTC décalée depuis maintenant, au format des colonnes `*_at` ;
    /// `modifier` suit la syntaxe SQLite, par exemple `"-14 days"`.
    pub fn datetime_from_now(&self, modifier: &str) -> Result<String> {
        Ok(self.conn.query_row("SELECT datetime('now', ?1)", [modifier], |row| row.get(0))?)
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
