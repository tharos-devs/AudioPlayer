//! Emplacement de la base, choisi par l'utilisateur (disque externe, dossier visible…).
//! Seul son chemin est noté hors de la base, dans `AudioPlayer/audioplayer.cfg` du dossier
//! de configuration. La v0.1 range le sien un niveau plus haut (`AudioPlayer.cfg`) : les
//! deux versions n'ouvrent pas la même base.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "audioplayer.cfg";
// Nom des premières versions de la réécriture, repris au premier lancement.
const OLD_FILE_NAME: &str = "database.cfg";

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("AudioPlayer")
}

fn pointer_file() -> PathBuf {
    pointer_file_in(&config_dir())
}

fn pointer_file_in(dir: &Path) -> PathBuf {
    let file = dir.join(FILE_NAME);
    let old = dir.join(OLD_FILE_NAME);
    if !file.exists() && old.exists() {
        if let Err(error) = std::fs::rename(&old, &file) {
            eprintln!("{} -> {}: {error}", old.display(), file.display());
            return old;
        }
    }
    file
}

/// `AUDIOPLAYER_DB` impose une base, sans rien enregistrer (essais).
pub fn saved() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AUDIOPLAYER_DB") {
        return Some(path.into());
    }
    let text = std::fs::read_to_string(pointer_file()).ok()?;
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn remember(path: &Path) -> std::io::Result<()> {
    if std::env::var_os("AUDIOPLAYER_DB").is_some() {
        return Ok(());
    }
    let file = pointer_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, path.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // L'ancien fichier est repris sous le nouveau nom, sans perdre le chemin.
    #[test]
    fn old_pointer_file_is_renamed() {
        let home = std::env::temp_dir().join(format!("audioplayer-location-{}", std::process::id()));
        let dir = home.join("AudioPlayer");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OLD_FILE_NAME), "/Volumes/Disk/library.db").unwrap();
        let file = pointer_file_in(&dir);
        assert_eq!(file, dir.join(FILE_NAME));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "/Volumes/Disk/library.db");
        assert!(!dir.join(OLD_FILE_NAME).exists());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
