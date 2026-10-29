//! Base SQLite de la bibliothèque musicale : schéma versionné, lectures du catalogue
//! et import depuis la base de la v0.1.

mod catalog;
mod preferences;
pub mod legacy;
pub mod schema;

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

pub use catalog::{Album, Artist, Artwork, Library, Track};

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    SchemaTooNew { found: usize, supported: usize },
    /// L'import exige une base cible vide.
    TargetNotEmpty,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(error) => write!(f, "sqlite: {error}"),
            Error::SchemaTooNew { found, supported } => {
                write!(f, "database schema v{found} is newer than this application (v{supported})")
            }
            Error::TargetNotEmpty => write!(f, "target database already contains libraries"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Error::Sqlite(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Database {
    conn: Connection,
}

/// Nature d'un fichier, établie sans le modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Base de cette application, au schéma `version` (0 = vide, à initialiser).
    Current { version: usize },
    /// Schéma plus récent que cette application.
    TooNew { version: usize },
    /// Base de la v0.1 (Tauri) : à importer, jamais à ouvrir directement.
    Legacy,
    /// Autre base SQLite, ou pas une base du tout.
    Unknown,
}

/// Examine `path` en lecture seule. Un fichier absent n'est pas une erreur : `None`.
pub fn inspect(path: impl AsRef<Path>) -> Result<Option<FileKind>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }
    // Fichier vide : SQLite le traite comme une base neuve.
    if std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(false) {
        return Ok(Some(FileKind::Current { version: 0 }));
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(path, flags) else { return Ok(Some(FileKind::Unknown)) };
    let Ok(version) = schema::version(&conn) else { return Ok(Some(FileKind::Unknown)) };
    let tables: Vec<String> = match conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'") {
        Ok(mut stmt) => stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?,
        Err(_) => return Ok(Some(FileKind::Unknown)),
    };
    let has = |name: &str| tables.iter().any(|t| t == name);
    Ok(Some(match version {
        0 if tables.is_empty() => FileKind::Current { version: 0 },
        0 if has("libraries") && has("library_paths") && has("artworks") => FileKind::Legacy,
        0 => FileKind::Unknown,
        v if v > schema::MIGRATIONS.len() => FileKind::TooNew { version: v },
        v if has("libraries") && has("tracks") => FileKind::Current { version: v },
        _ => FileKind::Unknown,
    }))
}

impl Database {
    /// Ouvre (ou crée) la base et la met à jour vers le dernier schéma.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::setup(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(mut conn: Connection) -> Result<Self> {
        // Les clés étrangères sont désactivées par défaut, connexion par connexion.
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        schema::migrate(&mut conn)?;
        Ok(Database { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}
