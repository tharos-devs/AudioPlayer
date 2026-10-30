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

/// Résultat d'un parcours.
#[derive(Debug, Default)]
pub struct Listing {
    pub files: Vec<AudioFile>,
    /// Dossiers ou entrées illisibles (disque débranché en cours de route, droits…) : la
    /// liste est alors incomplète et ne doit pas servir à retirer des morceaux.
    pub errors: usize,
}

/// Parcourt `root` récursivement, sans suivre les liens symboliques (pas de boucle possible).
/// Un dossier illisible est sauté et compté ; `on_folder` est appelé pour chaque dossier
/// visité et arrête le parcours en renvoyant `false`.
pub fn find(root: &Path, mut on_folder: impl FnMut(&Path) -> bool) -> Listing {
    let mut listing = Listing::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if !on_folder(&dir) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            listing.errors += 1;
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                listing.errors += 1;
                continue;
            };
            let path = entry.path();
            if hidden(&path) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                listing.errors += 1;
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_audio(&path) {
                match entry.metadata() {
                    Ok(meta) => {
                        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
                        listing.files.push(AudioFile { path, size: meta.len(), mtime });
                    }
                    Err(_) => listing.errors += 1,
                }
            }
        }
    }
    listing.files.sort_by(|a, b| a.path.cmp(&b.path));
    listing
}
