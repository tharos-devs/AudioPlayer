//! Moteur de lecture : un fil dédié pilote rodio (décodage symphonia) et l'égaliseur.
//! L'interface envoie des commandes et lit des événements, sans jamais attendre le
//! disque ni la carte son. Le morceau suivant est ouvert à l'avance et mis en file,
//! d'où un enchaînement sans blanc entre les plages d'un album.

pub mod equalizer;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::{CpalError, CpalErrorKind, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

/// Délai maximal entre deux mises à jour de la position et des fins de morceau.
const POLL: Duration = Duration::from_millis(50);
/// Sortie audio perdue ou absente : nouvel essai d'ouverture à cet intervalle.
const REOPEN: Duration = Duration::from_secs(1);

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
    /// Un morceau commence. `gapless` : c'est le morceau mis en file qui prend le relais
    /// (enchaînement sans blanc) ; sinon il a été lancé par `play`.
    Started { path: PathBuf, duration: Option<Duration>, gapless: bool },
    /// Plus rien à jouer.
    Ended,
    /// Fichier illisible ou absent : celui demandé par `play`, ou le suivant mis en file
    /// (signalé quand le morceau en cours se termine). La lecture s'arrête.
    Failed { path: PathBuf, message: String },
    /// Pas de sortie audio : rien ne joue jusqu'à ce qu'elle revienne (nouvel essai chaque seconde).
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
            lost: Arc::new(AtomicBool::new(false)),
            player: None,
            current: None,
            offset: Duration::ZERO,
            queued: None,
            unplayable: None,
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

type EqualizedFile = equalizer::EqualizedSource<Decoder<std::io::BufReader<File>>>;

struct Worker {
    events: Sender<Event>,
    position_ms: Arc<AtomicU64>,
    equalizer: Arc<equalizer::Settings>,
    // Levé par la sortie audio quand son périphérique disparaît (casque, DAC USB débranché).
    lost: Arc<AtomicBool>,
    player: Option<Player>,
    current: Option<PathBuf>,
    // Position où le morceau courant a été ouvert (déplacement) : le lecteur compte depuis là.
    offset: Duration,
    // Morceau déjà ajouté au lecteur, après `current`, avec sa durée.
    queued: Option<(PathBuf, Option<Duration>)>,
    // Suivant illisible : signalé quand le morceau en cours se termine.
    unplayable: Option<(PathBuf, String)>,
    volume: f32,
    playing: bool,
}

impl Worker {
    fn run(mut self, commands: Receiver<Command>) {
        let mut sink = None;
        let mut next_attempt = Instant::now();
        let mut reported = false;
        loop {
            // Sortie perdue : le morceau en cours reprendra à sa position sur la nouvelle.
            if self.lost.swap(false, Ordering::Relaxed) && sink.is_some() {
                sink = None;
                next_attempt = Instant::now();
            }
            if sink.is_none() && Instant::now() >= next_attempt {
                match self.open_sink() {
                    Ok(opened) => {
                        if self.current.is_some() {
                            let next = self.next_path();
                            self.restart(self.position(), next, &opened);
                        }
                        sink = Some(opened);
                        reported = false;
                    }
                    Err(message) => {
                        if !reported {
                            self.emit(Event::NoOutput { message });
                            reported = true;
                        }
                        next_attempt = Instant::now() + REOPEN;
                    }
                }
            }
            match commands.recv_timeout(POLL) {
                Ok(command) => match &sink {
                    Some(sink) => {
                        // Un enchaînement survenu depuis le dernier passage d'abord : sinon un saut
                        // ou un changement de suivant viserait encore le morceau précédent.
                        self.update();
                        self.handle(command, sink);
                    }
                    None => self.handle_without_output(command),
                },
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if sink.is_some() {
                self.update();
            }
        }
    }

    fn open_sink(&mut self) -> Result<MixerDeviceSink, String> {
        // Un indicateur par sortie : une erreur tardive de l'ancienne ne coupe pas la nouvelle.
        self.lost = Arc::new(AtomicBool::new(false));
        let lost = self.lost.clone();
        let opened = DeviceSinkBuilder::from_default_device()
            .map(|builder| {
                builder.with_error_callback(move |error: CpalError| {
                    if matches!(error.kind(), CpalErrorKind::DeviceNotAvailable | CpalErrorKind::StreamInvalidated) {
                        lost.store(true, Ordering::Relaxed);
                    }
                })
            })
            .and_then(|builder| builder.open_stream())
            // Configuration par défaut refusée : rodio essaie les autres périphériques et réglages.
            .or_else(|_| DeviceSinkBuilder::open_default_sink());
        let mut sink = opened.map_err(|error| error.to_string())?;
        // Fermée volontairement avec le moteur (fin de l'app, changement de base) : pas de message.
        sink.log_on_drop(false);
        Ok(sink)
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    // Suivant voulu, même illisible : le remettre en file après un saut le signalera encore.
    fn next_path(&self) -> Option<PathBuf> {
        self.queued.as_ref().map(|(path, _)| path.clone()).or_else(|| self.unplayable.as_ref().map(|(path, _)| path.clone()))
    }

    fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms.load(Ordering::Relaxed))
    }

    // Sans sortie audio, seules les commandes qui changent l'état à reprendre comptent.
    fn handle_without_output(&mut self, command: Command) {
        match command {
            Command::Play { path, next } => {
                self.reset();
                self.current = Some(path);
                self.queued = next.map(|path| (path, None));
                self.playing = true;
            }
            Command::SetNext(next) => self.queued = next.map(|path| (path, None)),
            Command::Pause => self.playing = false,
            Command::Resume => self.playing = self.current.is_some(),
            Command::Stop => self.reset(),
            Command::Seek(position) => self.position_ms.store(position.as_millis() as u64, Ordering::Relaxed),
            Command::Volume(volume) => self.volume = volume,
        }
    }

    fn handle(&mut self, command: Command, sink: &MixerDeviceSink) {
        match command {
            Command::Play { path, next } => {
                self.reset();
                let player = Player::connect_new(sink.mixer());
                player.set_volume(self.volume);
                match self.open(&path) {
                    Ok(source) => {
                        let duration = source.total_duration();
                        player.append(source);
                        self.player = Some(player);
                        self.current = Some(path.clone());
                        self.playing = true;
                        self.emit(Event::Started { path, duration, gapless: false });
                        self.queue(next);
                    }
                    Err(message) => self.emit(Event::Failed { path, message }),
                }
            }
            Command::SetNext(next) => {
                let queued = self.queued.as_ref().map(|(path, _)| path);
                if next.as_ref() != queued || (next.is_none() && self.unplayable.is_some()) {
                    // Un morceau déjà en file ne se retire pas d'un `Player` rodio :
                    // on relance le morceau courant à sa position, suivi du nouveau.
                    if self.queued.is_some() {
                        // Position exacte du lecteur, pas celle notée au dernier passage (50 ms).
                        let at = self.player.as_ref().map_or(self.position(), |p| self.offset + p.get_pos());
                        self.restart(at, next, sink);
                    } else {
                        self.unplayable = None;
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
            // Le fichier est rouvert à la position voulue plutôt que de demander le saut au
            // fil de la sortie audio : rodio l'attendrait sans limite si la sortie a disparu.
            Command::Seek(position) => {
                if self.current.is_some() {
                    let next = self.next_path();
                    self.restart(position, next, sink);
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

    fn open(&self, path: &Path) -> Result<EqualizedFile, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let decoder = Decoder::try_from(file).map_err(|e| e.to_string())?;
        Ok(equalizer::EqualizedSource::new(decoder, self.equalizer.clone()))
    }

    fn queue(&mut self, next: Option<PathBuf>) {
        self.queued = None;
        self.unplayable = None;
        let (Some(player), Some(path)) = (&self.player, next) else { return };
        match self.open(&path) {
            Ok(source) => {
                let duration = source.total_duration();
                player.append(source);
                self.queued = Some((path, duration));
            }
            Err(message) => self.unplayable = Some((path, message)),
        }
    }

    // Rejoue le morceau courant à partir de `at` dans un lecteur neuf, `next` en file.
    fn restart(&mut self, at: Duration, next: Option<PathBuf>, sink: &MixerDeviceSink) {
        let Some(current) = self.current.clone() else { return };
        let playing = self.playing;
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.queued = None;
        let player = Player::connect_new(sink.mixer());
        player.set_volume(self.volume);
        if !playing {
            player.pause();
        }
        match self.open(&current) {
            Ok(mut source) => {
                // Saut refusé : à la fin du morceau (flèche maintenue jusqu'au bout), il se termine ;
                // ailleurs (format sans index), il reprend au début.
                let near_end = source.total_duration().is_some_and(|d| at + Duration::from_secs(1) >= d);
                let at = if source.try_seek(at).is_ok() {
                    at
                } else if near_end {
                    self.reset();
                    self.emit(Event::Ended);
                    return;
                } else {
                    Duration::ZERO
                };
                player.append(source);
                self.player = Some(player);
                self.offset = at;
                self.playing = playing;
                self.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
                self.queue(next);
            }
            Err(message) => {
                self.reset();
                self.emit(Event::Failed { path: current, message });
            }
        }
    }

    fn reset(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.current = None;
        self.queued = None;
        self.unplayable = None;
        self.offset = Duration::ZERO;
        self.playing = false;
        self.position_ms.store(0, Ordering::Relaxed);
    }

    fn update(&mut self) {
        let Some(player) = &self.player else { return };
        // Le morceau en file a pris le relais : il devient le morceau courant.
        if self.queued.is_some() && player.len() <= 1 && !player.empty() {
            let (path, duration) = self.queued.take().unwrap();
            self.current = Some(path.clone());
            self.offset = Duration::ZERO;
            self.position_ms.store(player.get_pos().as_millis() as u64, Ordering::Relaxed);
            self.emit(Event::Started { path, duration, gapless: true });
            return;
        }
        if player.empty() {
            let unplayable = self.unplayable.take();
            self.reset();
            self.emit(match unplayable {
                Some((path, message)) => Event::Failed { path, message },
                None => Event::Ended,
            });
            return;
        }
        let position = self.offset + player.get_pos();
        self.position_ms.store(position.as_millis() as u64, Ordering::Relaxed);
    }
}
