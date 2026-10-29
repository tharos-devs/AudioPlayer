//! Réglages de l'interface, gardés d'un lancement à l'autre dans un fichier TOML
//! lisible à côté de la base. Les données musicales restent dans la base.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// Tout champ absent du fichier prend sa valeur par défaut : on peut en ajouter sans migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub accent: String,
    pub zoom: f32,
    pub language: String,
    /// Nom de la bibliothèque ouverte au démarrage (les ids changent si la base est recréée).
    pub library: Option<String>,
    pub window: WindowSettings,
    pub panels: PanelSettings,
    pub player: PlayerSettings,
    pub equalizer: EqualizerSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Taille logique, hors zoom.
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelSettings {
    pub artists_width: f32,
    pub albums_width: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerSettings {
    pub volume: f32,
    pub muted: bool,
    pub random: bool,
    pub repeat: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerSettings {
    pub visible: bool,
    pub enabled: bool,
    pub preset: String,
    pub gains: Vec<f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dark: true,
            accent: "amber".into(),
            zoom: 0.9,
            language: "en".into(),
            library: None,
            window: WindowSettings::default(),
            panels: PanelSettings::default(),
            player: PlayerSettings::default(),
            equalizer: EqualizerSettings::default(),
        }
    }
}

impl Default for WindowSettings {
    fn default() -> Self {
        WindowSettings { width: 1360.0, height: 820.0, maximized: false }
    }
}

impl Default for PanelSettings {
    fn default() -> Self {
        PanelSettings { artists_width: 255.0, albums_width: 300.0 }
    }
}

impl Default for PlayerSettings {
    fn default() -> Self {
        PlayerSettings { volume: 70.0, muted: false, random: false, repeat: false }
    }
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        EqualizerSettings { visible: true, enabled: false, preset: "Custom".into(), gains: vec![0.0; 7] }
    }
}

impl Settings {
    /// Un fichier absent ou illisible donne les réglages par défaut, sans bloquer le démarrage.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                eprintln!("{}: {error}; using defaults", path.display());
                Settings::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(error) => {
                eprintln!("{}: {error}; using defaults", path.display());
                Settings::default()
            }
        }
    }

    /// Écrit dans un fichier temporaire puis le renomme : jamais de fichier à moitié écrit.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = path(dir.path());
        assert_eq!(Settings::load(&path), Settings::default());

        let mut settings = Settings { zoom: 0.75, library: Some("OST".into()), ..Settings::default() };
        settings.equalizer.gains[2] = -3.5;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);

        // Champs manquants : valeurs par défaut ; fichier invalide : défauts complets.
        std::fs::write(&path, "dark = false\n[player]\nvolume = 20.0\n").unwrap();
        let partial = Settings::load(&path);
        assert!(!partial.dark);
        assert_eq!((partial.player.volume, partial.zoom), (20.0, 0.9));
        std::fs::write(&path, "dark = [").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }
}
