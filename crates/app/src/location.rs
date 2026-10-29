//! Emplacement de la base, choisi par l'utilisateur (disque externe, dossier visible…).
//! Seul son chemin est noté hors de la base, dans un petit fichier de configuration,
//! comme `AudioPlayer.cfg` de la v0.1 — sous un autre nom, pour ne pas que la v0.1
//! ouvre la nouvelle base.

use std::path::{Path, PathBuf};

fn pointer_file() -> PathBuf {
    let dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("AudioPlayer");
    dir.join("database.cfg")
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
