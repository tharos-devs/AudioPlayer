//! Recherche des fichiers audio d'un dossier.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Formats lus par les décodeurs du moteur audio (symphonia).
pub const EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "m4b", "aac", "ogg", "oga", "wav", "aif", "aiff"];

#[derive(Debug, Clone, PartialEq)]
pub struct AudioFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|known| known.eq_ignore_ascii_case(e)))
}

// Fichiers et dossiers cachés, dont les « ._ » que macOS laisse sur les disques non Apple.
fn hidden(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
}

/// Parcourt `root` récursivement, sans suivre les liens symboliques (pas de boucle possible).
/// Un dossier illisible est ignoré ; `on_folder` est appelé pour chaque dossier visité.
pub fn find(root: &Path, mut on_folder: impl FnMut(&Path)) -> Vec<AudioFile> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        on_folder(&dir);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if hidden(&path) {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_audio(&path) {
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
                    files.push(AudioFile { path, size: meta.len(), mtime });
                }
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}
