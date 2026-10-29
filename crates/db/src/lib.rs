//! Base SQLite de la bibliothèque musicale : schéma versionné, lectures du catalogue
//! et import depuis la base de la v0.1.

mod catalog;
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
