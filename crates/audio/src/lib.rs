//! Moteur de lecture : un fil dédié pilote rodio (décodage symphonia) et l'égaliseur.
//! L'interface envoie des commandes et lit des événements, sans jamais attendre le
//! disque ni la carte son. Le morceau suivant est ouvert à l'avance et mis en file,
//! d'où un enchaînement sans blanc entre les plages d'un album.

pub mod equalizer;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, Player};

/// Délai maximal entre deux mises à jour de la position et des fins de morceau.
const POLL: Duration = Duration::from_millis(50);

enum Command {
    Play { path: PathBuf, next: Option<PathBuf> },
    SetNext(Option<PathBuf>),
    Pause,
    Resume,
    Stop,
    Seek(Duration),
    Volume(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Un morceau commence : demandé par `play`, ou le suivant mis en file (enchaînement).
    Started { path: PathBuf, duration: Option<Duration> },
    /// Plus rien à jouer.
    Ended,
    /// Fichier illisible ou absent ; la lecture s'arrête.
    Failed { path: PathBuf, message: String },
    /// Pas de sortie audio : le moteur ne jouera rien.
    NoOutput { message: String },
}

pub struct Engine {
    commands: Sender<Command>,
    events: Receiver<Event>,
    position_ms: Arc<AtomicU64>,
    equalizer: Arc<equalizer::Settings>,
}

impl Engine {
    pub fn start() -> Engine {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let position_ms = Arc::new(AtomicU64::new(0));
        let equalizer = Arc::new(equalizer::Settings::default());
        let worker = Worker {
            events: event_tx,
            position_ms: position_ms.clone(),
            equalizer: equalizer.clone(),
            player: None,
            current: None,
            queued: None,
            volume: 1.0,
            playing: false,
        };
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || worker.run(command_rx))
            .expect("spawn audio thread");
        Engine { commands, events, position_ms, equalizer }
    }

    /// Joue `path` tout de suite ; `next` est ouvert à l'avance pour l'enchaîner.
    pub fn play(&self, path: &Path, next: Option<&Path>) {
        self.send(Command::Play { path: path.to_owned(), next: next.map(Path::to_owned) });
    }

    /// Change le morceau à enchaîner après celui en cours.
    pub fn set_next(&self, next: Option<&Path>) {
        self.send(Command::SetNext(next.map(Path::to_owned)));
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn resume(&self) {
        self.send(Command::Resume);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn seek(&self, position: Duration) {
        self.send(Command::Seek(position));
    }

    /// Gain linéaire, 0 à 1.
    pub fn set_volume(&self, volume: f32) {
        self.send(Command::Volume(volume.clamp(0.0, 1.0)));
    }

    /// Appliqué en direct aux morceaux en cours et à venir.
    pub fn set_equalizer(&self, enabled: bool, gains: [f32; 7]) {
        self.equalizer.set(enabled, gains);
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms.load(Ordering::Relaxed))
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    fn send(&self, command: Command) {
        // Le fil audio ne s'arrête qu'avec le moteur : un envoi ne peut échouer qu'à la fermeture.
        let _ = self.commands.send(command);
    }
}

struct Worker {
    events: Sender<Event>,
    position_ms: Arc<AtomicU64>,
    equalizer: Arc<equalizer::Settings>,
    player: Option<Player>,
    current: Option<PathBuf>,
    // Morceau déjà ajouté au lecteur, après `current`.
    queued: Option<PathBuf>,
    volume: f32,
    playing: bool,
}

impl Worker {
    fn run(mut self, commands: Receiver<Command>) {
        let mut sink = match DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => sink,
            Err(error) => {
                let _ = self.events.send(Event::NoOutput { message: error.to_string() });
                // Consommer les commandes jusqu'à la fermeture, sans rien jouer.
                while commands.recv().is_ok() {}
                return;
            }
        };
        // Fermée volontairement avec le moteur (fin de l'app, changement de base) : pas de message.
        sink.log_on_drop(false);
        loop {
            match commands.recv_timeout(POLL) {
                Ok(command) => self.handle(command, &sink),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.update();
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn handle(&mut self, command: Command, sink: &rodio::MixerDeviceSink) {
        match command {
            Command::Play { path, next } => {
                self.reset();
                let player = Player::connect_new(sink.mixer());
                player.set_volume(self.volume);
                match self.open(&path) {
                    Ok(source) => {
                        let duration = rodio::Source::total_duration(&source);
                        player.append(source);
                        self.player = Some(player);
                        self.current = Some(path.clone());
                        self.playing = true;
                        self.emit(Event::Started { path, duration });
                        self.queue(next);
                    }
                    Err(message) => self.emit(Event::Failed { path, message }),
                }
            }
            Command::SetNext(next) => {
                if next != self.queued {
                    // Un morceau déjà en file ne se retire pas d'un `Player` rodio :
                    // on relance le morceau courant à sa position, suivi du nouveau.
                    if self.queued.is_some() {
                        self.requeue(next, sink);
                    } else {
                        self.queue(next);
                    }
                }
            }
            Command::Pause => {
                if let Some(player) = &self.player {
                    player.pause();
                }
                self.playing = false;
            }
            Command::Resume => {
                if let Some(player) = &self.player {
                    player.play();
                    self.playing = true;
                }
            }
            Command::Stop => self.reset(),
            Command::Seek(position) => {
                if let Some(player) = &self.player {
                    if player.try_seek(position).is_ok() {
                        self.position_ms.store(position.as_millis() as u64, Ordering::Relaxed);
                    }
                }
            }
            Command::Volume(volume) => {
                self.volume = volume;
                if let Some(player) = &self.player {
                    player.set_volume(volume);
                }
            }
        }
    }

    fn open(&self, path: &Path) -> Result<equalizer::EqualizedSource<Decoder<std::io::BufReader<File>>>, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
        Ok(equalizer::EqualizedSource::new(decoder, self.equalizer.clone()))
    }

    fn queue(&mut self, next: Option<PathBuf>) {
        let (Some(player), Some(path)) = (&self.player, next) else { return };
        // Fichier suivant illisible : rien en file ; l'erreur sortira quand on tentera de le jouer.
        if let Ok(source) = self.open(&path) {
            player.append(source);
            self.queued = Some(path);
        }
    }

    fn requeue(&mut self, next: Option<PathBuf>, sink: &rodio::MixerDeviceSink) {
        let Some(current) = self.current.clone() else { return };
        let position = Duration::from_millis(self.position_ms.load(Ordering::Relaxed));
        let playing = self.playing;
        self.reset();
        let player = Player::connect_new(sink.mixer());
        player.set_volume(self.volume);
        if !playing {
            player.pause();
        }
        if let Ok(source) = self.open(&current) {
            player.append(source);
            let _ = player.try_seek(position);
            self.player = Some(player);
            self.current = Some(current);
            self.playing = playing;
            self.position_ms.store(position.as_millis() as u64, Ordering::Relaxed);
            self.queue(next);
        }
    }

    fn reset(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.current = None;
        self.queued = None;
        self.playing = false;
        self.position_ms.store(0, Ordering::Relaxed);
    }

    fn update(&mut self) {
        let Some(player) = &self.player else { return };
        // Le morceau en file a pris le relais : il devient le morceau courant.
        if self.queued.is_some() && player.len() <= 1 && !player.empty() {
            let path = self.queued.take().unwrap();
            self.current = Some(path.clone());
            self.position_ms.store(player.get_pos().as_millis() as u64, Ordering::Relaxed);
            let duration = None;
            self.emit(Event::Started { path, duration });
            return;
        }
        if player.empty() {
            self.reset();
            self.emit(Event::Ended);
            return;
        }
        self.position_ms.store(player.get_pos().as_millis() as u64, Ordering::Relaxed);
    }
}
