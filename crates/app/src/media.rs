//! Touches multimédia et infos « À l'écoute » du système (Centre de contrôle sur macOS,
//! MPRIS sous Linux, SMTC sous Windows), via souvlaki. Les touches marchent même
//! quand la fenêtre n'a pas le focus, comme dans la v0.1.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

pub use souvlaki::SeekDirection;

pub struct NowPlayingInfo<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub duration: Duration,
    /// Pochette encodée (JPEG, PNG…), écrite dans un fichier pour le système.
    pub artwork: Option<&'a [u8]>,
}

pub struct SystemMedia {
    controls: Option<MediaControls>,
    events: Receiver<MediaControlEvent>,
    cover: Option<PathBuf>,
}

impl SystemMedia {
    /// `hwnd` : fenêtre native, exigée sous Windows (souvlaki panique sans elle). Elle
    /// n'existe qu'une fois la fenêtre affichée : d'ici là, pas d'intégration système.
    pub fn new(hwnd: Option<*mut c_void>) -> Self {
        remove_stale_covers();
        let (sender, events) = mpsc::channel();
        if cfg!(windows) && hwnd.is_none() {
            return SystemMedia { controls: None, events, cover: None };
        }
        let config = PlatformConfig { display_name: "AudioPlayer", dbus_name: "audioplayer", hwnd };
        // Sans intégration système (Windows sans fenêtre native, Linux sans D-Bus…), l'app marche quand même.
        let controls = MediaControls::new(config).and_then(|mut controls| {
            controls.attach(move |event| {
                let _ = sender.send(event);
            })?;
            Ok(controls)
        });
        let controls = match controls {
            Ok(controls) => Some(controls),
            Err(error) => {
                eprintln!("media keys: {error:?}");
                None
            }
        };
        SystemMedia { controls, events, cover: None }
    }

    pub fn commands(&self) -> Vec<MediaControlEvent> {
        self.events.try_iter().collect()
    }

    pub fn set_track(&mut self, info: Option<NowPlayingInfo>) {
        let Some(controls) = self.controls.as_mut() else { return };
        let Some(info) = info else {
            let _ = controls.set_metadata(MediaMetadata::default());
            let _ = controls.set_playback(MediaPlayback::Stopped);
            return;
        };
        if let Some(old) = self.cover.take() {
            let _ = std::fs::remove_file(old);
        }
        self.cover = info.artwork.and_then(write_cover);
        let url = self.cover.as_ref().map(|path| format!("file://{}", path.display()));
        let metadata = MediaMetadata {
            title: Some(info.title),
            artist: Some(info.artist),
            album: Some(info.album),
            duration: Some(info.duration),
            cover_url: url.as_deref(),
        };
        if let Err(error) = controls.set_metadata(metadata) {
            eprintln!("now playing: {error:?}");
        }
    }

    pub fn set_state(&mut self, playing: bool, position: Duration) {
        let Some(controls) = self.controls.as_mut() else { return };
        let progress = Some(MediaPosition(position));
        let playback = if playing { MediaPlayback::Playing { progress } } else { MediaPlayback::Paused { progress } };
        let _ = controls.set_playback(playback);
    }
}

impl Drop for SystemMedia {
    fn drop(&mut self) {
        if let Some(cover) = self.cover.take() {
            let _ = std::fs::remove_file(cover);
        }
    }
}

fn cover_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("AudioPlayer"))
}

// La fermeture ne libère pas toujours la session : on nettoie les restes au démarrage.
fn remove_stale_covers() {
    let Some(entries) = cover_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("now-playing-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// Un nom par morceau : le système recharge l'image quand l'adresse change.
fn write_cover(data: &[u8]) -> Option<PathBuf> {
    let dir = cover_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let path = dir.join(format!("now-playing-{stamp}.img"));
    std::fs::write(&path, data).ok()?;
    Some(path)
}
