//! Playlists d'une bibliothèque (positions denses à partir de 0) et favoris.

use rusqlite::{params, OptionalExtension};

use crate::{Database, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub track_count: u32,
}

impl Database {
    pub fn playlists(&self, library_id: i64) -> Result<Vec<Playlist>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, (SELECT count(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id)
             FROM playlists p WHERE p.library_id = ?1
             ORDER BY p.name COLLATE NOCASE, p.id",
        )?;
        let rows = stmt.query_map([library_id], |row| Ok(Playlist { id: row.get(0)?, name: row.get(1)?, track_count: row.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `None` si la bibliothèque a déjà une playlist de ce nom.
    pub fn create_playlist(&self, library_id: i64, name: &str) -> Result<Option<i64>> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO playlists (library_id, name) VALUES (?1, ?2)",
            params![library_id, name],
        )?;
        Ok((inserted == 1).then(|| self.conn.last_insert_rowid()))
    }

    /// `false` si le nom est déjà pris dans la bibliothèque.
    pub fn rename_playlist(&self, playlist_id: i64, name: &str) -> Result<bool> {
        let taken: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM playlists o JOIN playlists p ON p.library_id = o.library_id
                            WHERE p.id = ?1 AND o.id <> ?1 AND o.name = ?2)",
            params![playlist_id, name],
            |row| row.get(0),
        )?;
        if taken {
            return Ok(false);
        }
        self.conn.execute(
            "UPDATE playlists SET name = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![playlist_id, name],
        )?;
        Ok(true)
    }

    pub fn delete_playlist(&self, playlist_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", [playlist_id])?;
        Ok(())
    }

    pub fn playlist_tracks(&self, playlist_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT track_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position")?;
        let rows = stmt.query_map([playlist_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Ajoute en fin de playlist les morceaux absents ; renvoie combien ont été ajoutés.
    pub fn add_to_playlist(&mut self, playlist_id: i64, track_ids: &[i64]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut next: i64 = tx.query_row(
            "SELECT coalesce(max(position) + 1, 0) FROM playlist_tracks WHERE playlist_id = ?1",
            [playlist_id],
            |row| row.get(0),
        )?;
        let mut added = 0;
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO playlist_tracks (playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
            )?;
            for &track in track_ids {
                if insert.execute(params![playlist_id, track, next])? == 1 {
                    added += 1;
                    next += 1;
                }
            }
        }
        tx.execute("UPDATE playlists SET updated_at = datetime('now') WHERE id = ?1", [playlist_id])?;
        tx.commit()?;
        Ok(added)
    }

    pub fn remove_from_playlist(&mut self, playlist_id: i64, track_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut delete = tx.prepare("DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2")?;
            for &track in track_ids {
                delete.execute(params![playlist_id, track])?;
            }
        }
        renumber(&tx, playlist_id)?;
        tx.commit()?;
        Ok(())
    }

    /// `false` si une autre bibliothèque porte déjà ce nom.
    pub fn rename_library(&self, library_id: i64, name: &str) -> Result<bool> {
        let taken: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM libraries WHERE name = ?2 AND id <> ?1)",
            params![library_id, name],
            |row| row.get(0),
        )?;
        if !taken {
            self.conn.execute("UPDATE libraries SET name = ?2 WHERE id = ?1", params![library_id, name])?;
        }
        Ok(!taken)
    }

    /// Retire la bibliothèque et tout son catalogue de la base ; les fichiers ne sont pas touchés.
    pub fn delete_library(&self, library_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM libraries WHERE id = ?1", [library_id])?;
        Ok(())
    }

    /// Date de mise en favori, ou rien ; renvoie le nouvel état.
    pub fn toggle_track_favorite(&self, track_id: i64) -> Result<bool> {
        self.toggle_favorite("tracks", track_id)
    }

    pub fn toggle_album_favorite(&self, album_id: i64) -> Result<bool> {
        self.toggle_favorite("albums", album_id)
    }

    pub fn toggle_artist_favorite(&self, artist_id: i64) -> Result<bool> {
        self.toggle_favorite("artists", artist_id)
    }

    // `table` est une constante interne (tracks, albums, artists), jamais une saisie.
    fn toggle_favorite(&self, table: &str, id: i64) -> Result<bool> {
        self.conn.execute(
            &format!("UPDATE {table} SET favorited_at = CASE WHEN favorited_at IS NULL THEN datetime('now') END WHERE id = ?1"),
            [id],
        )?;
        let favorite: Option<bool> = self
            .conn
            .query_row(&format!("SELECT favorited_at IS NOT NULL FROM {table} WHERE id = ?1"), [id], |row| row.get(0))
            .optional()?;
        Ok(favorite.unwrap_or(false))
    }

    /// « Reset Play Count » de la v0.1 (vue Most Played).
    pub fn reset_play_counts(&mut self, track_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut reset = tx.prepare("UPDATE tracks SET play_count = 0, last_played_at = NULL WHERE id = ?1")?;
            for id in track_ids {
                reset.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

// Positions denses 0..n après une suppression.
fn renumber(conn: &rusqlite::Connection, playlist_id: i64) -> rusqlite::Result<()> {
    // En deux temps : les positions négatives évitent tout conflit transitoire.
    conn.execute(
        "UPDATE playlist_tracks SET position = -1 - (
             SELECT count(*) FROM playlist_tracks o
             WHERE o.playlist_id = playlist_tracks.playlist_id AND o.position < playlist_tracks.position)
         WHERE playlist_id = ?1",
        [playlist_id],
    )?;
    conn.execute("UPDATE playlist_tracks SET position = -1 - position WHERE playlist_id = ?1", [playlist_id])?;
    Ok(())
}
