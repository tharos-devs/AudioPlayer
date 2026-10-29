//! Réglages de l'interface : paires clé → texte (JSON côté application).

use rusqlite::OptionalExtension;

use crate::{Database, Result};

impl Database {
    pub fn preference(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row("SELECT value FROM preferences WHERE key = ?1", [key], |row| row.get(0))
            .optional()?;
        Ok(value)
    }

    pub fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO preferences (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }
}
