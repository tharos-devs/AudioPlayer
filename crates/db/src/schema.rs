//! Schéma versionné : la migration d'index `i` amène la base à la version `i + 1`,
//! enregistrée dans `PRAGMA user_version`. Une migration publiée ne se modifie
//! plus : toute évolution passe par une nouvelle entrée en fin de liste.

use rusqlite::Connection;

use crate::{Error, Result};

pub const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5];

// Les dates sont du texte UTC « AAAA-MM-JJ HH:MM:SS », comme `datetime('now')`.
const V1: &str = "
CREATE TABLE libraries (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    -- Une bibliothèque = un dossier physique ; elles restent étanches entre elles.
    root_path    TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    last_scan_at TEXT
) STRICT;

CREATE TABLE artists (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    sort_name    TEXT,
    favorited_at TEXT,
    UNIQUE (library_id, name),
    -- Cible de la clé composite des albums.
    UNIQUE (library_id, id)
) STRICT;

CREATE TABLE albums (
    id             INTEGER PRIMARY KEY,
    library_id     INTEGER NOT NULL,
    artist_id      INTEGER NOT NULL,
    title          TEXT NOT NULL,
    sort_title     TEXT,
    year           INTEGER,
    is_compilation INTEGER NOT NULL DEFAULT 0 CHECK (is_compilation IN (0, 1)),
    folder_path    TEXT,
    favorited_at   TEXT,
    added_at       TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (library_id, artist_id, title),
    -- L'album appartient forcément à la bibliothèque de son artiste.
    FOREIGN KEY (library_id, artist_id) REFERENCES artists(library_id, id) ON DELETE CASCADE
) STRICT;

CREATE TABLE tracks (
    id             INTEGER PRIMARY KEY,
    album_id       INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    -- Interprète du morceau ; l'artiste de l'album est porté par l'album.
    artist_id      INTEGER NOT NULL REFERENCES artists(id),
    title          TEXT NOT NULL,
    disc_number    INTEGER,
    disc_subtitle  TEXT,
    track_number   INTEGER,
    duration_ms    INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    file_path      TEXT NOT NULL UNIQUE,
    file_size      INTEGER,
    file_mtime     INTEGER,
    codec          TEXT,
    bitrate        INTEGER,
    sample_rate    INTEGER,
    bit_depth      INTEGER,
    channels       INTEGER,
    genre          TEXT,
    composer       TEXT,
    conductor      TEXT,
    lyricist       TEXT,
    grouping       TEXT,
    work           TEXT,
    comment        TEXT,
    publisher      TEXT,
    catalog_number TEXT,
    barcode        TEXT,
    bpm            INTEGER,
    play_count     INTEGER NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at TEXT,
    rating         INTEGER CHECK (rating BETWEEN 0 AND 5),
    favorited_at   TEXT,
    added_at       TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

-- Une ligne par taille (côté du carré, en pixels).
CREATE TABLE artworks (
    album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    size     INTEGER NOT NULL CHECK (size > 0),
    mime     TEXT NOT NULL,
    data     BLOB NOT NULL,
    PRIMARY KEY (album_id, size)
) STRICT;

CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

CREATE TABLE playlist_tracks (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at    TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (playlist_id, track_id)
) STRICT;

CREATE INDEX albums_by_artist       ON albums (artist_id, year);
CREATE INDEX tracks_by_album        ON tracks (album_id, disc_number, track_number);
CREATE INDEX tracks_by_artist       ON tracks (artist_id);
CREATE INDEX tracks_by_added        ON tracks (added_at);
CREATE INDEX tracks_by_play_count   ON tracks (play_count) WHERE play_count > 0;
CREATE INDEX artists_favorited      ON artists (favorited_at) WHERE favorited_at IS NOT NULL;
CREATE INDEX albums_favorited       ON albums (favorited_at) WHERE favorited_at IS NOT NULL;
CREATE INDEX tracks_favorited       ON tracks (favorited_at) WHERE favorited_at IS NOT NULL;
CREATE INDEX playlist_tracks_order  ON playlist_tracks (playlist_id, position);
CREATE INDEX playlist_tracks_track  ON playlist_tracks (track_id);
";

// Réglages de l'interface, rangés dans la base comme dans la v0.1 : elle se déplace
// avec eux (disque externe, par exemple). Valeurs JSON, une clé par section.
const V2: &str = "
CREATE TABLE preferences (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
";

// Les playlists appartiennent à une bibliothèque, comme ses artistes et albums.
// Les tables sont reconstruites (SQLite ne sait pas ajouter une colonne NOT NULL
// liée) : la bibliothèque d'une playlist existante est celle de ses morceaux, la
// première sinon. Les tables filles sont supprimées avant les mères pour ne pas
// déclencher les suppressions en cascade.
const V3: &str = "
CREATE TABLE playlists_v3 (
    id         INTEGER PRIMARY KEY,
    library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (library_id, name)
) STRICT;

CREATE TABLE playlist_tracks_v3 (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists_v3(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at    TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (playlist_id, track_id)
) STRICT;

INSERT INTO playlists_v3 (id, library_id, name, created_at, updated_at)
SELECT p.id,
       coalesce((SELECT al.library_id FROM playlist_tracks pt
                 JOIN tracks t ON t.id = pt.track_id JOIN albums al ON al.id = t.album_id
                 WHERE pt.playlist_id = p.id ORDER BY pt.position LIMIT 1),
                (SELECT min(id) FROM libraries)),
       p.name, p.created_at, p.updated_at
FROM playlists p
WHERE EXISTS (SELECT 1 FROM libraries);

-- Seuls les morceaux de la bibliothèque de la playlist sont gardés.
INSERT INTO playlist_tracks_v3 (playlist_id, track_id, position, added_at)
SELECT pt.playlist_id, pt.track_id,
       row_number() OVER (PARTITION BY pt.playlist_id ORDER BY pt.position, pt.id) - 1,
       pt.added_at
FROM playlist_tracks pt
JOIN playlists_v3 p ON p.id = pt.playlist_id
JOIN tracks t ON t.id = pt.track_id
JOIN albums al ON al.id = t.album_id
WHERE al.library_id = p.library_id;

DROP TABLE playlist_tracks;
DROP TABLE playlists;
ALTER TABLE playlists_v3 RENAME TO playlists;
ALTER TABLE playlist_tracks_v3 RENAME TO playlist_tracks;
CREATE INDEX playlist_tracks_order ON playlist_tracks (playlist_id, position);
CREATE INDEX playlist_tracks_track ON playlist_tracks (track_id);

-- Garde-fou : un morceau ne rejoint qu'une playlist de sa propre bibliothèque.
CREATE TRIGGER playlist_tracks_same_library
BEFORE INSERT ON playlist_tracks
WHEN (SELECT al.library_id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = NEW.track_id)
     IS NOT (SELECT library_id FROM playlists WHERE id = NEW.playlist_id)
BEGIN
    SELECT RAISE(ABORT, 'track and playlist belong to different libraries');
END;
";

// Totaux de pistes et de disques (« 3 of 12 » du tiroir des tags), relus au prochain scan.
const V4: &str = "
ALTER TABLE tracks ADD COLUMN track_total INTEGER;
ALTER TABLE tracks ADD COLUMN disc_total INTEGER;
";

// Index jamais utilisés : l'app filtre récents, écoutes et favoris en mémoire. Ils ne
// coûtaient qu'à l'écriture (scans, chaque lecture comptée) et en taille de base.
const V5: &str = "
DROP INDEX tracks_by_added;
DROP INDEX tracks_by_play_count;
DROP INDEX artists_favorited;
DROP INDEX albums_favorited;
DROP INDEX tracks_favorited;
";

pub fn version(conn: &Connection) -> Result<usize> {
    let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version as usize)
}

/// Applique les migrations manquantes, chacune dans sa propre transaction.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let current = version(conn)?;
    if current > MIGRATIONS.len() {
        return Err(Error::SchemaTooNew { found: current, supported: MIGRATIONS.len() });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
