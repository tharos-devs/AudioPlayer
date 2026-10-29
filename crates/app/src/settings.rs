//! Réglages de l'interface, rangés dans la table `preferences` de la base comme dans
//! la v0.1 : une clé par section, valeur JSON. Ils suivent la base où qu'elle soit.

use audioplayer_db::Database;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub appearance: Appearance,
    pub session: Session,
    pub window: WindowSettings,
    pub panels: PanelSettings,
    pub player: PlayerSettings,
    pub equalizer: EqualizerSettings,
    pub recent: RecentSettings,
    pub tags: TagsSettings,
}

// Tout champ absent prend sa valeur par défaut : on peut en ajouter sans migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub dark: bool,
    pub accent: String,
    pub zoom: f32,
    pub language: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Nom de la bibliothèque ouverte au démarrage.
    pub library: Option<String>,
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
    pub tags_width: f32,
    pub tags_visible: bool,
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

/// « Added last 2 weeks » : quantité et unité (days, weeks, months, years).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentSettings {
    pub amount: u32,
    pub unit: String,
}

/// Tiroir des tags : champs affichés (clés de l'interface) et section technique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TagsSettings {
    pub fields: Vec<String>,
    pub technical: bool,
}

// Ceux cochés par défaut dans la v0.1.
impl Default for TagsSettings {
    fn default() -> Self {
        let fields = ["title", "artist", "album", "track-number", "disc-number", "year", "genre", "album-artist", "composer", "comment", "grouping"];
        TagsSettings { fields: fields.map(String::from).to_vec(), technical: true }
    }
}

impl Default for RecentSettings {
    fn default() -> Self {
        RecentSettings { amount: 2, unit: "weeks".into() }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { dark: true, accent: "amber".into(), zoom: 0.9, language: "en".into() }
    }
}

impl Default for WindowSettings {
    fn default() -> Self {
        WindowSettings { width: 1360.0, height: 820.0, maximized: false }
    }
}

impl Default for PanelSettings {
    fn default() -> Self {
        PanelSettings { artists_width: 255.0, albums_width: 300.0, tags_width: 320.0, tags_visible: false }
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

// Une section absente ou illisible garde ses valeurs par défaut, sans bloquer le démarrage.
fn read<T: DeserializeOwned + Default>(db: &Database, key: &str) -> T {
    match db.preference(key) {
        Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|error| {
            eprintln!("preference {key}: {error}; using defaults");
            T::default()
        }),
        Ok(None) => T::default(),
        Err(error) => {
            eprintln!("preference {key}: {error}; using defaults");
            T::default()
        }
    }
}

fn write<T: Serialize + PartialEq>(db: &Database, key: &str, value: &T, previous: &T) -> audioplayer_db::Result<()> {
    if value == previous {
        return Ok(());
    }
    let text = serde_json::to_string(value).expect("settings serialize to JSON");
    db.set_preference(key, &text)
}

impl Settings {
    pub fn load(db: &Database) -> Self {
        Settings {
            appearance: read(db, "appearance"),
            session: read(db, "session"),
            window: read(db, "window"),
            panels: read(db, "panels"),
            player: read(db, "player"),
            equalizer: read(db, "equalizer"),
            recent: read(db, "recent"),
            tags: read(db, "tags"),
        }
    }

    /// N'écrit que les sections qui ont changé depuis `previous`.
    pub fn save(&self, db: &Database, previous: &Settings) -> audioplayer_db::Result<()> {
        write(db, "appearance", &self.appearance, &previous.appearance)?;
        write(db, "session", &self.session, &previous.session)?;
        write(db, "window", &self.window, &previous.window)?;
        write(db, "panels", &self.panels, &previous.panels)?;
        write(db, "player", &self.player, &previous.player)?;
        write(db, "equalizer", &self.equalizer, &previous.equalizer)?;
        write(db, "recent", &self.recent, &previous.recent)?;
        write(db, "tags", &self.tags, &previous.tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_sections() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(Settings::load(&db), Settings::default());

        let mut settings = Settings::default();
        settings.appearance.zoom = 0.75;
        settings.session.library = Some("OST".into());
        settings.equalizer.gains[2] = -3.5;
        settings.save(&db, &Settings::default()).unwrap();
        assert_eq!(Settings::load(&db), settings);
        // Sections inchangées : rien d'écrit.
        assert_eq!(db.preference("player").unwrap(), None);

        // Champs manquants : valeurs par défaut ; JSON invalide : section par défaut.
        db.set_preference("player", r#"{"volume":20.0}"#).unwrap();
        db.set_preference("panels", "{").unwrap();
        let loaded = Settings::load(&db);
        assert_eq!((loaded.player.volume, loaded.player.muted), (20.0, false));
        assert_eq!(loaded.panels, PanelSettings::default());
    }
}
