// Prototype : la bibliothèque vient de la base SQLite (importée de la v0.1), mais
// il n'y a pas encore d'audio : la lecture est simulée par un minuteur.

mod covers;
mod library;
mod location;
mod media;
mod settings;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use audioplayer_audio::{Engine, Event};
use audioplayer_db::{legacy, Database, FileKind};
use library::{format_duration, sort_key, Library};
use settings::Settings;
use slint::{DataTransfer, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

const TRACK_PAYLOAD_PREFIX: &str = "audioplayer/track:";
// Vignettes de 80px affichées jusqu'à 80px : 160px suffisent en Retina.
const COVER_SIZE: u32 = 160;
// « Added last 2 weeks », la valeur par défaut de la v0.1.
const RECENT_MODIFIER: &str = "-14 days";
const STATUS_MESSAGE_DURATION: Duration = Duration::from_secs(3);

// Au-delà, des fichiers illisibles d'affilée arrêtent la lecture (disque débranché…).
const MAX_FAILURES: u32 = 5;
const PLAYBACK_POLL: Duration = Duration::from_millis(200);

/// Morceau en cours, repéré dans sa bibliothèque : la lecture continue pendant
/// qu'on en parcourt une autre.
struct NowPlaying {
    library: usize,
    track: usize,
    // File figée au lancement : la liste affichée à ce moment-là.
    queue: Vec<usize>,
    index: Option<usize>,
    // Déjà ouvert par le moteur pour l'enchaînement sans blanc.
    next: Option<usize>,
}

// Pas encore enregistrées en base : elles retiennent des ids de morceaux.
struct Playlist {
    name: String,
    tracks: Vec<i64>,
}

struct App {
    db: Database,
    db_path: PathBuf,
    libraries: Vec<audioplayer_db::Library>,
    selected_library: Option<usize>,
    library: Library,
    // Catalogues déjà chargés, gardés pour revenir instantanément sur une bibliothèque.
    catalogs: HashMap<usize, Library>,
    // Pochettes décodées, par id d'album ; chargées par bibliothèque entière.
    covers: HashMap<i64, Image>,
    artist_order: Vec<usize>,
    view: View,
    // Lettre de l'index des artistes : « * » = tout, « # » = hors A–Z.
    letter: Option<String>,
    recent_cutoff: String,
    // Morceaux de chaque album retenus par la vue et la lettre ; 0 = album masqué.
    album_counts: Vec<usize>,
    selected_artist: Option<usize>,
    selected_album: Option<usize>,
    selected_playlist: Option<usize>,
    query: String,
    playlists: Vec<Playlist>,
    // Morceaux affichés, dans l'ordre : sert aussi à précédent/suivant.
    visible: Vec<usize>,
    // Ligne de chaque morceau affiché (les en-têtes d'album occupent aussi des lignes).
    rows: HashMap<usize, usize>,
    reveal_serial: i32,
    engine: Engine,
    now: Option<NowPlaying>,
    is_playing: bool,
    // Fin de file atteinte : « lecture » rejoue le morceau affiché.
    stopped: bool,
    position: i32,
    random: bool,
    repeat: bool,
    // Dernières valeurs transmises au moteur.
    sent_volume: Option<f32>,
    sent_equalizer: Option<(bool, [f32; 7])>,
    failures: u32,
    rng: u64,
    media: media::SystemMedia,
    // Ce que le système affiche déjà : morceau, et état publié avec son heure.
    published_track: Option<(usize, usize)>,
    published_state: Option<(bool, i32, std::time::Instant)>,
    status_timer: Timer,
}

impl App {
    /// `library` : nom de la bibliothèque à ouvrir, la première sinon.
    fn new(db: Database, db_path: PathBuf, library: Option<&str>) -> audioplayer_db::Result<Self> {
        let libraries = db.libraries()?;
        let recent_cutoff = db.datetime_from_now(RECENT_MODIFIER)?;

        let mut app = App {
            db,
            db_path,
            libraries,
            selected_library: None,
            library: Library::default(),
            catalogs: HashMap::new(),
            covers: HashMap::new(),
            artist_order: Vec::new(),
            view: View::Library,
            letter: None,
            recent_cutoff,
            album_counts: Vec::new(),
            selected_artist: None,
            selected_album: None,
            selected_playlist: None,
            query: String::new(),
            // Les playlists de la base arriveront avec leur enregistrement.
            playlists: Vec::new(),
            visible: Vec::new(),
            rows: HashMap::new(),
            reveal_serial: 0,
            engine: Engine::start(),
            now: None,
            is_playing: false,
            stopped: false,
            position: 0,
            random: false,
            repeat: false,
            sent_volume: None,
            sent_equalizer: None,
            failures: 0,
            media: media::SystemMedia::new(),
            published_track: None,
            published_state: None,
            rng: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x5eed, |d| d.as_nanos() as u64) | 1,
            status_timer: Timer::default(),
        };
        if !app.libraries.is_empty() {
            let index = library.and_then(|name| app.libraries.iter().position(|l| l.name == name));
            app.load_library(index.unwrap_or(0))?;
        }
        Ok(app)
    }

    // Remet à zéro la navigation : les index de l'ancienne bibliothèque ne valent plus rien.
    fn load_library(&mut self, index: usize) -> audioplayer_db::Result<()> {
        let library = match self.catalogs.remove(&index) {
            Some(library) => library,
            None => {
                let (library, pixels) = Library::load_with_covers(&self.db, self.libraries[index].id, COVER_SIZE)?;
                self.add_covers(pixels);
                library
            }
        };
        if let Some(previous) = self.selected_library {
            self.catalogs.insert(previous, std::mem::take(&mut self.library));
        }
        let mut artist_order: Vec<usize> = (0..library.artists.len()).collect();
        artist_order.sort_by(|&a, &b| library.artists[a].sort_key.cmp(&library.artists[b].sort_key));

        self.selected_library = Some(index);
        self.library = library;
        self.artist_order = artist_order;
        self.visible.clear();
        self.reset_navigation();
        Ok(())
    }

    fn add_covers(&mut self, pixels: Vec<(i64, covers::Pixels)>) {
        self.covers.extend(pixels.into_iter().map(|(album, pixels)| (album, covers::to_image(pixels))));
    }

    // Catalogue préchargé en arrière-plan ; ignoré si l'utilisateur l'a déjà ouvert.
    fn add_preloaded(&mut self, index: usize, library: Library, pixels: Vec<(i64, covers::Pixels)>) {
        if self.selected_library != Some(index) && !self.catalogs.contains_key(&index) {
            self.catalogs.insert(index, library);
            self.add_covers(pixels);
        }
    }

    // Retour à la racine de la bibliothèque (navigateToRoot de la v0.1).
    fn reset_navigation(&mut self) {
        self.view = View::Library;
        self.letter = None;
        self.selected_artist = None;
        self.selected_album = None;
        self.selected_playlist = None;
        self.query.clear();
        self.update_scope();
    }

    fn update_scope(&mut self) {
        let mut counts = vec![0; self.library.albums.len()];
        for track in 0..self.library.tracks.len() {
            if self.in_scope(track) {
                counts[self.library.tracks[track].album] += 1;
            }
        }
        self.album_counts = counts;
    }

    // Morceau retenu par la vue courante (et la lettre, en vue bibliothèque).
    fn in_scope(&self, track: usize) -> bool {
        let t = &self.library.tracks[track];
        let album = &self.library.albums[t.album];
        match self.view {
            View::Library => self.letter_matches(album.artist),
            View::Recent => t.added_at >= self.recent_cutoff,
            View::MostPlayed => t.play_count > 0,
            View::FavoriteArtists => self.library.artists[album.artist].is_favorite,
            View::FavoriteAlbums => album.is_favorite,
            View::FavoriteTracks => t.is_favorite,
            _ => false,
        }
    }

    fn letter_matches(&self, artist: usize) -> bool {
        let Some(letter) = self.letter.as_deref().filter(|&l| l != "*") else { return true };
        let first = self.library.artists[artist].name.chars().next().map(|c| c.to_ascii_uppercase());
        match letter {
            "#" => !first.is_some_and(|c| c.is_ascii_uppercase()),
            _ => first.is_some_and(|c| letter.starts_with(c)),
        }
    }

    fn query_key(&self) -> String {
        sort_key(self.query.trim())
    }

    fn refresh_all(&mut self, ui: &AppWindow) {
        let library = self.selected_library.map(|i| &self.libraries[i]);
        ui.set_library_name(library.map_or("", |l| l.name.as_str()).into());
        ui.set_library_count(self.library.tracks.len() as i32);
        ui.set_library_artists(self.library.artists.len() as i32);
        ui.set_library_albums(self.library.albums.len() as i32);
        let tracks = &self.library.tracks;
        ui.set_most_played_count(tracks.iter().filter(|t| t.play_count > 0).count() as i32);
        ui.set_favorite_tracks_count(tracks.iter().filter(|t| t.is_favorite).count() as i32);
        ui.set_favorite_albums_count(self.library.albums.iter().filter(|a| a.is_favorite).count() as i32);
        ui.set_favorite_artists_count(self.library.artists.iter().filter(|a| a.is_favorite).count() as i32);
        self.refresh_libraries(ui);
        self.refresh_navigation(ui);
        self.refresh_player(ui);
    }

    fn refresh_navigation(&mut self, ui: &AppWindow) {
        ui.set_view(self.view);
        ui.set_letter(self.letter.as_deref().unwrap_or("").into());
        ui.set_search_text(self.query.as_str().into());
        self.refresh_artists(ui);
        self.refresh_albums(ui);
        self.refresh_playlists(ui);
        self.refresh_tracks(ui);
    }

    fn refresh_libraries(&self, ui: &AppWindow) {
        let rows = self.libraries.iter().enumerate().map(|(id, library)| ListItem {
            id: id as i32,
            label: library.name.as_str().into(),
            ..Default::default()
        });
        ui.set_libraries(model(rows.collect()));
        ui.set_selected_library(to_ui_id(self.selected_library));
    }

    fn cover(&self, album: usize) -> (Image, bool) {
        match self.covers.get(&self.library.albums[album].id) {
            Some(image) => (image.clone(), true),
            None => (Image::default(), false),
        }
    }

    fn refresh_artists(&self, ui: &AppWindow) {
        let mut album_counts = vec![0; self.library.artists.len()];
        for (album, &count) in self.album_counts.iter().enumerate() {
            if count > 0 {
                album_counts[self.library.albums[album].artist] += 1;
            }
        }
        // Les vues albums, morceaux et plus écoutés n'ont pas de liste d'artistes.
        let listed = matches!(self.view, View::Library | View::Recent | View::FavoriteArtists);
        let query = self.query_key();
        let rows: Vec<ListItem> = self
            .artist_order
            .iter()
            .filter(|&&a| listed && album_counts[a] > 0)
            .filter(|&&a| query.is_empty() || self.library.artists[a].sort_key.contains(&query))
            .map(|&artist| ListItem {
                id: artist as i32,
                label: self.library.artists[artist].name.as_str().into(),
                count: album_counts[artist] as i32,
                ..Default::default()
            })
            .collect();
        ui.set_artists_count(rows.len() as i32);
        ui.set_artists(model(rows));
        ui.set_selected_artist(to_ui_id(self.selected_artist));
    }

    fn refresh_albums(&self, ui: &AppWindow) {
        let albums = &self.library.albums;
        let listed = match self.view {
            View::MostPlayed | View::FavoriteTracks | View::Playlist => false,
            View::FavoriteArtists => self.selected_artist.is_some(),
            _ => true,
        };
        let query = self.query_key();
        let mut ids: Vec<usize> = (0..albums.len())
            .filter(|&a| listed && self.album_counts[a] > 0)
            .filter(|&a| self.selected_artist.is_none_or(|artist| albums[a].artist == artist))
            .filter(|&a| query.is_empty() || albums[a].sort_key.contains(&query))
            .collect();
        // Comme la v0.1 : albums par titre, même pour un seul artiste.
        ids.sort_by(|&a, &b| albums[a].sort_key.cmp(&albums[b].sort_key).then(a.cmp(&b)));

        ui.set_albums_count(ids.len() as i32);
        let rows = ids.into_iter().map(|album| {
            let (artwork, has_artwork) = self.cover(album);
            ListItem {
                id: album as i32,
                label: albums[album].title.as_str().into(),
                count: self.album_counts[album] as i32,
                artwork,
                has_artwork,
            }
        });
        ui.set_albums(model(rows.collect()));
        ui.set_selected_album(to_ui_id(self.selected_album));
    }

    fn refresh_playlists(&self, ui: &AppWindow) {
        let rows = self.playlists.iter().enumerate().map(|(id, playlist)| ListItem {
            id: id as i32,
            label: playlist.name.as_str().into(),
            count: playlist.tracks.iter().filter(|&&id| self.library.track_by_id(id).is_some()).count() as i32,
            ..Default::default()
        });
        ui.set_playlists(model(rows.collect()));
        ui.set_selected_playlist(to_ui_id(self.selected_playlist));
        ui.set_playlist_name(self.selected_playlist.map_or("", |p| self.playlists[p].name.as_str()).into());
    }

    fn refresh_tracks(&mut self, ui: &AppWindow) {
        let tracks = &self.library.tracks;
        let query = self.query_key();
        // Favoris artistes ou albums : les morceaux attendent une sélection.
        let listed = match self.view {
            View::FavoriteArtists => self.selected_artist.is_some(),
            View::FavoriteAlbums => self.selected_album.is_some(),
            _ => true,
        };

        let mut ids: Vec<usize> = match self.selected_playlist.filter(|_| self.view == View::Playlist) {
            Some(playlist) => self.playlists[playlist].tracks.iter().filter_map(|&id| self.library.track_by_id(id)).collect(),
            None => (0..tracks.len())
                .filter(|&t| listed && self.in_scope(t))
                .filter(|&t| self.selected_artist.is_none_or(|artist| self.library.albums[tracks[t].album].artist == artist))
                .filter(|&t| self.selected_album.is_none_or(|album| tracks[t].album == album))
                .filter(|&t| query.is_empty() || self.matches(t, &query))
                .collect(),
        };

        // Une playlist garde l'ordre d'ajout ; la bibliothèque est groupée par album.
        if self.view == View::Playlist {
            let items = ids.iter().enumerate().map(|(position, &t)| self.track_item(t, position as i32 + 1, false));
            ui.set_playlist_items(model(items.collect()));
            self.rows = ids.iter().enumerate().map(|(row, &t)| (t, row)).collect();
        } else {
            if self.view == View::MostPlayed {
                ids.sort_by(|&a, &b| tracks[b].play_count.cmp(&tracks[a].play_count).then(self.compare(a, b)));
            } else {
                ids.sort_by(|&a, &b| self.compare(a, b));
            }
            let items = self.grouped_items(&ids);
            self.rows = items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.kind == TrackItemKind::Track)
                .map(|(row, item)| (item.id as usize, row))
                .collect();
            ui.set_track_items(model(items));
        }

        let total: i32 = ids.iter().map(|&t| tracks[t].duration).sum();
        ui.set_tracks_count(ids.len() as i32);
        ui.set_tracks_duration(total);
        self.visible = ids;
    }

    fn grouped_items(&self, ids: &[usize]) -> Vec<TrackItem> {
        let mut items = Vec::with_capacity(ids.len() + ids.len() / 4);
        let mut current_album = None;
        for &t in ids {
            let album = self.library.tracks[t].album;
            let first_in_album = current_album != Some(album);
            if first_in_album {
                current_album = Some(album);
                items.push(self.album_header(album));
            }
            items.push(self.track_item(t, self.library.tracks[t].number, first_in_album));
        }
        items
    }

    fn album_header(&self, album: usize) -> TrackItem {
        let a = &self.library.albums[album];
        let (artwork, has_artwork) = self.cover(album);
        TrackItem {
            kind: TrackItemKind::AlbumHeader,
            id: album as i32,
            artist: self.library.artists[a.artist].name.as_str().into(),
            album: a.title.as_str().into(),
            year: a.year,
            track_count: self.album_counts[album] as i32,
            artwork,
            has_artwork,
            ..Default::default()
        }
    }

    fn track_item(&self, track: usize, number: i32, first_in_album: bool) -> TrackItem {
        let t = &self.library.tracks[track];
        let album = &self.library.albums[t.album];
        let (artwork, has_artwork) = self.cover(t.album);
        let disc = if album.total_discs > 1 { format!("{}/{}", t.disc, album.total_discs) } else { t.disc.to_string() };
        TrackItem {
            kind: TrackItemKind::Track,
            id: track as i32,
            number,
            title: t.title.as_str().into(),
            duration: format_duration(t.duration).into(),
            disc: disc.into(),
            artist: self.library.artists[t.artist].name.as_str().into(),
            album: album.title.as_str().into(),
            year: album.year,
            track_count: 0,
            artwork,
            has_artwork,
            first_in_album,
        }
    }

    fn library_at(&self, index: usize) -> Option<&Library> {
        if self.selected_library == Some(index) { Some(&self.library) } else { self.catalogs.get(&index) }
    }

    fn library_at_mut(&mut self, index: usize) -> Option<&mut Library> {
        if self.selected_library == Some(index) { Some(&mut self.library) } else { self.catalogs.get_mut(&index) }
    }

    fn now_track(&self) -> Option<(&Library, &library::Track)> {
        let now = self.now.as_ref()?;
        let library = self.library_at(now.library)?;
        Some((library, library.tracks.get(now.track)?))
    }

    fn refresh_player(&self, ui: &AppWindow) {
        let now = self.now_track();
        // Le morceau n'est surligné que dans sa propre bibliothèque.
        let here = self.now.as_ref().filter(|now| Some(now.library) == self.selected_library).map(|now| now.track);
        ui.set_playing_track(to_ui_id(here));
        ui.set_playing(self.is_playing);
        // « Titre - Album », comme le bandeau du lecteur de la v0.1.
        ui.set_now_title(
            now.map(|(lib, t)| format!("{} - {}", t.title, lib.albums[t.album].title)).unwrap_or_default().into(),
        );
        ui.set_now_artist(now.map_or("", |(lib, t)| lib.artists[t.artist].name.as_str()).into());
        let artwork = now.and_then(|(lib, t)| self.covers.get(&lib.albums[t.album].id));
        ui.set_now_artwork(artwork.cloned().unwrap_or_default());
        ui.set_now_has_artwork(artwork.is_some());
        self.refresh_position(ui);
    }

    fn refresh_position(&self, ui: &AppWindow) {
        let duration = self.now_track().map_or(0, |(_, t)| t.duration);
        let position = self.position.min(duration);
        ui.set_position(position);
        ui.set_duration(duration);
        ui.set_position_text(format_duration(position).into());
        ui.set_duration_text(format_duration(duration).into());
        ui.set_remaining_text(format!("-{}", format_duration((duration - position).max(0))).into());
    }

    fn matches(&self, track: usize, query: &str) -> bool {
        let track = &self.library.tracks[track];
        track.sort_key.contains(query)
            || self.library.artists[track.artist].sort_key.contains(query)
            || self.library.albums[track.album].sort_key.contains(query)
    }

    // Ordre de la bibliothèque : artiste, puis albums par titre, puis disque et numéro.
    fn compare(&self, a: usize, b: usize) -> Ordering {
        let (lib, ta, tb) = (&self.library, &self.library.tracks[a], &self.library.tracks[b]);
        let (album_a, album_b) = (&lib.albums[ta.album], &lib.albums[tb.album]);
        lib.artists[album_a.artist]
            .sort_key
            .cmp(&lib.artists[album_b.artist].sort_key)
            .then(album_a.sort_key.cmp(&album_b.sort_key))
            .then(ta.album.cmp(&tb.album))
            .then(ta.disc.cmp(&tb.disc))
            .then(ta.number.cmp(&tb.number))
    }

    fn select_library(&mut self, ui: &AppWindow, id: i32) {
        let Some(index) = from_ui_id(id).filter(|&i| i < self.libraries.len()) else { return };
        // Un clic sur la bibliothèque courante ramène à sa racine.
        if self.selected_library == Some(index) {
            self.reset_navigation();
            ui.set_selected_track(-1);
            self.refresh_navigation(ui);
            return;
        }
        if let Err(error) = self.load_library(index) {
            eprintln!("library {}: {error}", self.libraries[index].name);
            return;
        }
        ui.set_selected_track(-1);
        self.refresh_all(ui);
    }

    // Un second clic sur la lettre active la retire.
    fn select_letter(&mut self, ui: &AppWindow, letter: &str) {
        let letter = (self.letter.as_deref() != Some(letter)).then(|| letter.to_owned());
        self.reset_navigation();
        self.letter = letter;
        self.update_scope();
        self.refresh_navigation(ui);
    }

    // Les entrées intelligentes se comportent en bascule, comme dans la v0.1.
    fn select_view(&mut self, ui: &AppWindow, view: View) {
        let toggled_off = self.view == view;
        self.reset_navigation();
        if !toggled_off {
            self.view = view;
            self.update_scope();
        }
        self.refresh_navigation(ui);
    }

    // Un second clic sur l'artiste sélectionné le désélectionne.
    fn select_artist(&mut self, ui: &AppWindow, id: i32) {
        let artist = from_ui_id(id);
        self.selected_artist = if self.selected_artist == artist { None } else { artist };
        self.selected_album = None;
        self.query.clear();
        ui.set_search_text(SharedString::new());
        self.refresh_artists(ui);
        self.refresh_albums(ui);
        self.refresh_tracks(ui);
    }

    // Sélectionner un album sélectionne aussi son artiste (correctif de la v0.1.1).
    fn select_album(&mut self, ui: &AppWindow, id: i32) {
        let album = from_ui_id(id);
        if self.selected_album == album {
            self.selected_album = None;
        } else if let Some(album) = album {
            self.reveal_album(ui, album);
            return;
        }
        ui.set_selected_album(-1);
        self.refresh_tracks(ui);
    }

    fn reveal_album(&mut self, ui: &AppWindow, album: usize) {
        // Hors de la vue courante (ou depuis une playlist) : retour à la bibliothèque.
        if self.view == View::Playlist || self.album_counts[album] == 0 {
            self.reset_navigation();
        }
        let artist = self.library.albums[album].artist;
        // En vue « albums favoris », l'artiste n'a pas de liste où être sélectionné.
        if self.view != View::FavoriteAlbums {
            self.selected_artist = Some(artist);
        }
        self.selected_album = Some(album);
        self.query.clear();
        self.refresh_navigation(ui);
    }

    fn select_playlist(&mut self, ui: &AppWindow, id: i32) {
        let playlist = from_ui_id(id);
        if playlist.is_none() || (self.view == View::Playlist && self.selected_playlist == playlist) {
            self.reset_navigation();
        } else {
            self.reset_navigation();
            self.view = View::Playlist;
            self.selected_playlist = playlist;
        }
        self.refresh_navigation(ui);
    }

    // Validée par Entrée ; efface la sélection et la lettre, comme la v0.1.
    fn search(&mut self, ui: &AppWindow, query: &str) {
        self.reset_navigation();
        self.query = query.to_owned();
        self.refresh_navigation(ui);
    }

    /// Lance un morceau de la bibliothèque affichée ; la file est la liste visible.
    fn play(&mut self, ui: &AppWindow, track: usize) {
        let Some(library) = self.selected_library else { return };
        let queue = if self.visible.contains(&track) { self.visible.clone() } else { vec![track] };
        let index = queue.iter().position(|&t| t == track);
        self.now = Some(NowPlaying { library, track, queue, index, next: None });
        self.failures = 0;
        ui.set_selected_track(track as i32);
        self.start(ui);
    }

    // Joue `now.track` depuis le début et met le suivant en file.
    fn start(&mut self, ui: &AppWindow) {
        let next = self.pick_next();
        let Some(now) = self.now.as_mut() else { return };
        now.next = next;
        let (library, track) = (now.library, now.track);
        let Some(lib) = self.library_at(library) else { return };
        let path = PathBuf::from(&lib.tracks[track].path);
        let next_path = next.map(|n| PathBuf::from(&lib.tracks[n].path));
        self.engine.play(&path, next_path.as_deref());
        self.is_playing = true;
        self.stopped = false;
        self.position = 0;
        self.record_play(ui, library, track);
        self.refresh_player(ui);
    }

    // Comptée au lancement de chaque morceau, comme dans la v0.1.
    fn record_play(&mut self, ui: &AppWindow, library: usize, track: usize) {
        let Some(lib) = self.library_at_mut(library) else { return };
        let entry = &mut lib.tracks[track];
        entry.play_count += 1;
        let id = entry.id;
        if let Err(error) = self.db.record_play(id) {
            eprintln!("play count {id}: {error}");
        }
        if Some(library) == self.selected_library {
            ui.set_most_played_count(self.library.tracks.iter().filter(|t| t.play_count > 0).count() as i32);
        }
    }

    fn random_track(&mut self, library: usize, avoid: usize) -> Option<usize> {
        let count = self.library_at(library)?.tracks.len();
        if count == 0 {
            return None;
        }
        // xorshift64 : largement suffisant pour tirer un morceau.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let pick = (self.rng % count as u64) as usize;
        Some(if pick == avoid && count > 1 { (pick + 1) % count } else { pick })
    }

    // « rand » : un morceau au hasard de la bibliothèque ; « loop » : la file reboucle.
    fn pick_next(&mut self) -> Option<usize> {
        let now = self.now.as_ref()?;
        if self.random {
            let (library, track) = (now.library, now.track);
            return self.random_track(library, track);
        }
        let index = now.index?;
        match now.queue.get(index + 1) {
            Some(&next) => Some(next),
            None if self.repeat => now.queue.first().copied(),
            None => None,
        }
    }

    // Le moteur est passé au morceau mis en file.
    fn advance(&mut self, ui: &AppWindow) {
        let Some(now) = self.now.as_mut() else { return };
        let Some(next) = now.next.take() else { return };
        now.track = next;
        now.index = now.queue.iter().position(|&t| t == next);
        let library = now.library;
        self.position = 0;
        self.failures = 0;
        self.record_play(ui, library, next);
        self.queue_next();
        self.refresh_player(ui);
    }

    // Recalcule le morceau à enchaîner (après un changement de « rand » ou « loop »).
    fn queue_next(&mut self) {
        let next = self.pick_next();
        let Some(now) = self.now.as_mut() else { return };
        now.next = next;
        let library = now.library;
        let path = next.and_then(|n| self.library_at(library).map(|lib| PathBuf::from(&lib.tracks[n].path)));
        self.engine.set_next(path.as_deref());
    }

    // Fait défiler la liste jusqu'au morceau, s'il y est affiché.
    fn reveal(&mut self, ui: &AppWindow, track: usize) {
        if let Some(&row) = self.rows.get(&track) {
            self.reveal_serial += 1;
            ui.set_reveal_row(row as i32);
            ui.set_reveal_serial(self.reveal_serial);
        }
    }

    // ↑/↓ : morceau précédent/suivant de la liste ; sans sélection, le premier (↓) ou le dernier (↑).
    fn move_selection(&mut self, ui: &AppWindow, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = from_ui_id(ui.get_selected_track()).and_then(|t| self.visible.iter().position(|&v| v == t));
        let index = match current {
            Some(index) => index.saturating_add_signed(delta).min(self.visible.len() - 1),
            None if delta < 0 => self.visible.len() - 1,
            None => 0,
        };
        let track = self.visible[index];
        ui.set_selected_track(track as i32);
        self.reveal(ui, track);
    }

    fn play_selected(&mut self, ui: &AppWindow) {
        if let Some(track) = from_ui_id(ui.get_selected_track()).filter(|t| self.visible.contains(t)) {
            self.play(ui, track);
        }
    }

    fn show_current(&mut self, ui: &AppWindow) {
        let Some((library, track)) = self.now.as_ref().map(|now| (now.library, now.track)) else { return };
        if self.selected_library != Some(library) {
            self.select_library(ui, library as i32);
        }
        if self.selected_library == Some(library) {
            self.reveal_album(ui, self.library.tracks[track].album);
            ui.set_selected_track(track as i32);
            self.reveal(ui, track);
        }
    }

    fn toggle_play(&mut self, ui: &AppWindow) {
        if self.now.is_none() {
            if let Some(&first) = self.visible.first() {
                self.play(ui, first);
            }
            return;
        }
        if self.is_playing {
            self.engine.pause();
            self.is_playing = false;
        } else if self.stopped {
            self.start(ui);
            return;
        } else {
            self.engine.resume();
            self.is_playing = true;
        }
        self.refresh_player(ui);
    }

    fn step(&mut self, ui: &AppWindow, delta: isize) {
        let Some(now) = self.now.as_ref() else {
            if let Some(&first) = self.visible.first() {
                self.play(ui, first);
            }
            return;
        };
        let (library, track, index, len) = (now.library, now.track, now.index, now.queue.len());
        let target = if self.random {
            self.random_track(library, track).map(|t| (t, None))
        } else {
            index.and_then(|i| {
                let wrapped = (i as isize + delta).rem_euclid(len as isize) as usize;
                let inside = i.checked_add_signed(delta).filter(|&j| j < len);
                inside.or((self.repeat).then_some(wrapped))
            })
            .map(|j| (self.now.as_ref().unwrap().queue[j], Some(j)))
        };
        if let Some((track, index)) = target {
            let now = self.now.as_mut().unwrap();
            now.track = track;
            now.index = index;
            self.start(ui);
        }
    }

    // Flèches maintenues et touches d'avance rapide : déplacement relatif, en secondes.
    fn seek_by(&mut self, ui: &AppWindow, seconds: i32) {
        let Some(duration) = self.now_track().map(|(_, t)| t.duration) else { return };
        if duration > 0 {
            let target = (self.position + seconds).clamp(0, duration);
            self.seek(ui, target as f32 / duration as f32);
        }
    }

    fn seek(&mut self, ui: &AppWindow, ratio: f32) {
        let Some(duration) = self.now_track().map(|(_, t)| t.duration) else { return };
        let target = ratio.clamp(0.0, 1.0) * duration as f32;
        self.engine.seek(Duration::from_secs_f32(target));
        self.position = target as i32;
        self.refresh_position(ui);
    }

    // Réglages de l'interface vers le moteur : volume (courbe au carré, comme la v0.1),
    // égaliseur, et « rand » / « loop » qui changent le morceau à enchaîner.
    fn sync_controls(&mut self, ui: &AppWindow) {
        let volume = if ui.get_muted() { 0.0 } else { (ui.get_volume() / 100.0).powi(2) };
        if self.sent_volume != Some(volume) {
            self.engine.set_volume(volume);
            self.sent_volume = Some(volume);
        }

        let state = ui.global::<EqualizerState>();
        let mut gains = [0.0f32; 7];
        for (gain, value) in gains.iter_mut().zip(state.get_gains().iter()) {
            *gain = value;
        }
        let equalizer = (state.get_enabled(), gains);
        if self.sent_equalizer != Some(equalizer) {
            self.engine.set_equalizer(equalizer.0, equalizer.1);
            self.sent_equalizer = Some(equalizer);
        }

        let (random, repeat) = (ui.get_random_mode(), ui.get_loop_mode());
        if (random, repeat) != (self.random, self.repeat) {
            let random_turned_on = random && !self.random;
            (self.random, self.repeat) = (random, repeat);
            // Comme la v0.1 : activer « rand » lance aussitôt un morceau au hasard.
            if random_turned_on {
                let library = self.now.as_ref().map(|n| n.library).or(self.selected_library);
                let avoid = self.now.as_ref().map_or(usize::MAX, |n| n.track);
                if let Some(track) = library.and_then(|l| self.random_track(l, avoid)) {
                    self.now = Some(NowPlaying { library: library.unwrap(), track, queue: vec![track], index: Some(0), next: None });
                    self.start(ui);
                }
            } else if self.now.is_some() && !self.stopped {
                self.queue_next();
            }
        }
    }

    fn handle_media_commands(&mut self, ui: &AppWindow) {
        use souvlaki::MediaControlEvent as Command;
        for command in self.media.commands() {
            match command {
                Command::Toggle => self.toggle_play(ui),
                Command::Play if !self.is_playing => self.toggle_play(ui),
                Command::Pause | Command::Stop if self.is_playing => self.toggle_play(ui),
                Command::Next => self.step(ui, 1),
                Command::Previous => self.step(ui, -1),
                Command::Seek(direction) => self.seek_by(ui, if direction == media::SeekDirection::Forward { 5 } else { -5 }),
                Command::SeekBy(direction, amount) => {
                    let seconds = amount.as_secs() as i32;
                    self.seek_by(ui, if direction == media::SeekDirection::Forward { seconds } else { -seconds });
                }
                Command::SetPosition(souvlaki::MediaPosition(position)) => {
                    let duration = self.now_track().map_or(0, |(_, t)| t.duration);
                    if duration > 0 {
                        self.seek(ui, position.as_secs_f32() / duration as f32);
                    }
                }
                _ => {}
            }
        }
    }

    // Tient le système au courant : morceau (titre, pochette) et lecture/pause avec la position.
    // La position n'est republiée qu'après un saut : le système l'extrapole entre-temps.
    fn publish(&mut self) {
        let key = self.now.as_ref().map(|now| (now.library, now.track));
        if key != self.published_track {
            let artwork = self.now_track().and_then(|(lib, t)| self.db.artwork(lib.albums[t.album].id, 600).ok().flatten());
            let details = self.now_track().map(|(lib, t)| {
                let album = &lib.albums[t.album];
                (t.title.clone(), lib.artists[t.artist].name.clone(), album.title.clone(), t.duration)
            });
            let info = details.as_ref().map(|(title, artist, album, duration)| media::NowPlayingInfo {
                title,
                artist,
                album,
                duration: Duration::from_secs((*duration).max(0) as u64),
                artwork: artwork.as_ref().map(|a| a.data.as_slice()),
            });
            self.media.set_track(info);
            self.published_track = key;
            self.published_state = None;
        }
        if key.is_none() {
            return;
        }
        let expected = self.published_state.map(|(playing, position, at)| {
            let elapsed = if playing { at.elapsed().as_secs() as i32 } else { 0 };
            (playing, position + elapsed)
        });
        let drifted = expected.is_none_or(|(playing, position)| playing != self.is_playing || (position - self.position).abs() > 2);
        if drifted {
            self.media.set_state(self.is_playing, Duration::from_secs(self.position.max(0) as u64));
            self.published_state = Some((self.is_playing, self.position, std::time::Instant::now()));
        }
    }

    fn tick(&mut self, ui: &AppWindow) {
        self.handle_media_commands(ui);
        self.sync_controls(ui);
        for event in self.engine.events() {
            match event {
                Event::Started { path, .. } => {
                    let queued = self.now.as_ref().and_then(|now| {
                        let next = now.next?;
                        Some(self.library_at(now.library)?.tracks[next].path == path.to_string_lossy())
                    });
                    if queued == Some(true) {
                        self.advance(ui);
                    } else {
                        self.failures = 0;
                    }
                }
                Event::Ended => {
                    self.is_playing = false;
                    self.stopped = true;
                    self.position = 0;
                    self.refresh_player(ui);
                }
                Event::Failed { path, message } => {
                    let title = self.now_track().map(|(_, t)| t.title.clone()).unwrap_or_else(|| path.display().to_string());
                    self.show_status(ui, Status { kind: StatusKind::PlaybackFailed, track: title.into(), playlist: message.into(), ..Default::default() });
                    self.failures += 1;
                    self.is_playing = false;
                    self.stopped = true;
                    if self.failures < MAX_FAILURES {
                        self.step(ui, 1);
                    } else {
                        self.refresh_player(ui);
                    }
                }
                Event::NoOutput { message } => {
                    self.show_status(ui, Status { kind: StatusKind::NoOutput, playlist: message.into(), ..Default::default() });
                    self.is_playing = false;
                }
            }
        }
        if self.is_playing {
            self.position = self.engine.position().as_secs() as i32;
            self.refresh_position(ui);
        }
        self.publish();
    }

    // Message temporaire dans la barre d'état, effacé après quelques secondes.
    fn show_status(&self, ui: &AppWindow, status: Status) {
        ui.set_status(status);
        let weak = ui.as_weak();
        self.status_timer.start(TimerMode::SingleShot, STATUS_MESSAGE_DURATION, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_status(Status { kind: StatusKind::Summary, ..Default::default() });
            }
        });
    }

    fn show_problem(&self, ui: &AppWindow, problem: &Problem) {
        self.show_status(
            ui,
            Status {
                kind: StatusKind::DatabaseProblem,
                track: problem.path.display().to_string().into(),
                playlist: problem.detail.as_str().into(),
                problem: problem.kind,
            },
        );
    }

    fn track_action(&mut self, ui: &AppWindow, id: i32, action: &str) {
        let Some(track) = from_ui_id(id) else { return };
        let status = |kind| Status { kind, track: self.library.tracks[track].title.as_str().into(), ..Default::default() };
        match action {
            "play" => self.play(ui, track),
            "show-album" => {
                self.reveal_album(ui, self.library.tracks[track].album);
                ui.set_selected_track(id);
            }
            "favorite" => self.add_to_playlist(ui, 0, track),
            "play-next" => self.show_status(ui, status(StatusKind::PlayNext)),
            "enqueue" => self.show_status(ui, status(StatusKind::Enqueued)),
            _ => {}
        }
    }

    fn add_to_playlist(&mut self, ui: &AppWindow, playlist: usize, track: usize) {
        let Some(target) = self.playlists.get_mut(playlist) else { return };
        let id = self.library.tracks[track].id;
        let kind = if target.tracks.contains(&id) {
            StatusKind::AlreadyInPlaylist
        } else {
            target.tracks.push(id);
            StatusKind::AddedToPlaylist
        };
        let status = Status {
            kind,
            track: self.library.tracks[track].title.as_str().into(),
            playlist: target.name.as_str().into(),
            ..Default::default()
        };
        self.refresh_playlists(ui);
        if self.view == View::Playlist && self.selected_playlist == Some(playlist) {
            self.refresh_tracks(ui);
        }
        self.show_status(ui, status);
    }
}

#[derive(Clone)]
struct Context {
    app: Rc<RefCell<App>>,
    ui: slint::Weak<AppWindow>,
}

impl Context {
    fn update(&self, f: impl FnOnce(&mut App, &AppWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&mut self.app.borrow_mut(), &ui);
        }
    }
}

fn track_payload(id: i32) -> DataTransfer {
    DataTransfer::from(SharedString::from(format!("{TRACK_PAYLOAD_PREFIX}{id}")))
}

fn payload_track(data: &DataTransfer) -> Option<usize> {
    let text = data.plain_text().ok()?;
    text.strip_prefix(TRACK_PAYLOAD_PREFIX)?.parse().ok()
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn to_ui_id(id: Option<usize>) -> i32 {
    id.map_or(-1, |id| id as i32)
}

fn from_ui_id(id: i32) -> Option<usize> {
    usize::try_from(id).ok()
}

fn select_language(ui: &AppWindow, language: &str) {
    match slint::select_bundled_translation(language) {
        Ok(()) => ui.set_language(language.into()),
        Err(error) => eprintln!("language {language}: {error}"),
    }
}

/// Problème rencontré avec un fichier de base ; le message est formé côté Slint.
struct Problem {
    kind: DatabaseProblem,
    path: PathBuf,
    detail: String,
}

impl Problem {
    fn new(kind: DatabaseProblem, path: &Path) -> Self {
        Problem { kind, path: path.to_owned(), detail: String::new() }
    }

    fn failed(path: &Path, error: impl std::fmt::Display) -> Self {
        Problem { kind: DatabaseProblem::Failed, path: path.to_owned(), detail: error.to_string() }
    }
}

// Vérifie sans écrire qu'un fichier est bien une base de cette application.
fn check_database(path: &Path) -> Result<(), Problem> {
    match audioplayer_db::inspect(path).map_err(|e| Problem::failed(path, e))? {
        Some(FileKind::Current { .. }) | None => Ok(()),
        Some(FileKind::Legacy) => Err(Problem::new(DatabaseProblem::LegacyFile, path)),
        Some(FileKind::TooNew { .. }) => Err(Problem::new(DatabaseProblem::TooNew, path)),
        Some(FileKind::Unknown) => Err(Problem::new(DatabaseProblem::UnknownFile, path)),
    }
}

fn database_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("AudioPlayer database", &["db"])
}

// Une base neuve ne remplace jamais un fichier existant, quel qu'il soit.
fn choose_new_database() -> Result<Option<PathBuf>, Problem> {
    let Some(path) = database_dialog().set_file_name("audioplayer.db").save_file() else { return Ok(None) };
    if path.exists() {
        return Err(Problem::new(DatabaseProblem::AlreadyExists, &path));
    }
    Database::open(&path).map_err(|e| Problem::failed(&path, e))?;
    Ok(Some(path))
}

fn choose_existing_database() -> Result<Option<PathBuf>, Problem> {
    let Some(path) = database_dialog().pick_file() else { return Ok(None) };
    check_database(&path)?;
    Ok(Some(path))
}

// L'ancienne base est lue en lecture seule ; la nouvelle est créée à côté de l'endroit choisi.
fn import_legacy_database() -> Result<Option<PathBuf>, Problem> {
    let Some(legacy_path) = database_dialog().set_title("v0.1 database").pick_file() else { return Ok(None) };
    if audioplayer_db::inspect(&legacy_path).map_err(|e| Problem::failed(&legacy_path, e))? != Some(FileKind::Legacy) {
        return Err(Problem::new(DatabaseProblem::NotLegacy, &legacy_path));
    }
    let directory = legacy_path.parent().map(Path::to_owned).unwrap_or_default();
    let dialog = database_dialog().set_directory(directory).set_file_name("audioplayer-library.db");
    let Some(path) = dialog.save_file() else { return Ok(None) };
    if path.exists() {
        return Err(Problem::new(DatabaseProblem::AlreadyExists, &path));
    }
    let result = Database::open(&path).and_then(|mut db| legacy::import(&mut db, &legacy_path).map(|_| ()));
    if let Err(error) = result {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        return Err(Problem::failed(&path, error));
    }
    Ok(Some(path))
}

fn apply_settings(ui: &AppWindow, settings: &Settings) {
    let theme = ui.global::<Theme>();
    let appearance = &settings.appearance;
    theme.set_dark(appearance.dark);
    if let Some(accent) = theme.get_accents().iter().find(|a| a.id == appearance.accent.as_str()) {
        theme.set_accent_id(accent.id);
        theme.set_accent(accent.value);
    }
    theme.set_zoom(appearance.zoom.clamp(theme.get_zoom_min(), theme.get_zoom_max()));
    ui.invoke_sync_color_scheme();
    select_language(ui, &appearance.language);

    ui.window().set_size(slint::LogicalSize::new(settings.window.width, settings.window.height));
    ui.set_artists_width(settings.panels.artists_width);
    ui.set_albums_width(settings.panels.albums_width);
    ui.set_volume(settings.player.volume.clamp(0.0, 100.0));
    ui.set_muted(settings.player.muted);
    ui.set_random_mode(settings.player.random);
    ui.set_loop_mode(settings.player.repeat);

    ui.set_show_equalizer(settings.equalizer.visible);
    let equalizer = ui.global::<EqualizerState>();
    equalizer.set_enabled(settings.equalizer.enabled);
    equalizer.set_preset(settings.equalizer.preset.as_str().into());
    // Une valeur modifiée à la main ne doit pas casser l'égaliseur : 7 bandes, ±12 dB.
    let count = equalizer.get_frequencies().row_count();
    let gains: Vec<f32> = (0..count).map(|i| settings.equalizer.gains.get(i).copied().unwrap_or(0.0).clamp(-12.0, 12.0)).collect();
    equalizer.set_gains(model(gains));
}

// `previous` garde la taille d'avant agrandissement, que la fenêtre agrandie ne donne pas.
fn capture_settings(ui: &AppWindow, app: &App, previous: &Settings) -> Settings {
    let theme = ui.global::<Theme>();
    let equalizer = ui.global::<EqualizerState>();
    let window = ui.window();
    let maximized = window.is_maximized();
    let size = window.size().to_logical(window.scale_factor());
    Settings {
        appearance: settings::Appearance {
            dark: theme.get_dark(),
            accent: theme.get_accent_id().into(),
            zoom: theme.get_zoom(),
            language: ui.get_language().into(),
        },
        session: settings::Session { library: app.selected_library.map(|i| app.libraries[i].name.clone()) },
        window: settings::WindowSettings {
            width: if maximized { previous.window.width } else { size.width },
            height: if maximized { previous.window.height } else { size.height },
            maximized,
        },
        panels: settings::PanelSettings {
            artists_width: ui.get_artists_width(),
            albums_width: ui.get_albums_width(),
        },
        player: settings::PlayerSettings {
            volume: ui.get_volume(),
            muted: ui.get_muted(),
            random: ui.get_random_mode(),
            repeat: ui.get_loop_mode(),
        },
        equalizer: settings::EqualizerSettings {
            visible: ui.get_show_equalizer(),
            enabled: equalizer.get_enabled(),
            preset: equalizer.get_preset().into(),
            gains: equalizer.get_gains().iter().collect(),
        },
    }
}

/// Fenêtre principale ouverte sur une base ; la lâcher ferme tout.
struct Session {
    ui: AppWindow,
    ctx: Context,
    // Derniers réglages écrits en base, pour n'écrire que les changements.
    saved: Rc<RefCell<Settings>>,
    _timers: Vec<Timer>,
}

impl Session {
    fn save_settings(&self) {
        save_settings(&self.ctx, &self.saved);
    }
}

fn save_settings(ctx: &Context, saved: &RefCell<Settings>) {
    let Some(ui) = ctx.ui.upgrade() else { return };
    let app = ctx.app.borrow();
    let current = capture_settings(&ui, &app, &saved.borrow());
    if current != *saved.borrow() {
        if let Err(error) = current.save(&app.db, &saved.borrow()) {
            eprintln!("settings: {error}");
        }
        *saved.borrow_mut() = current;
    }
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static SETUP: RefCell<Option<SetupWindow>> = const { RefCell::new(None) };
}

fn launch(path: &Path) -> Result<(), Problem> {
    check_database(path)?;
    let db = Database::open(path).map_err(|e| Problem::failed(path, e))?;
    let initial = Settings::load(&db);
    let app = App::new(db, path.to_owned(), initial.session.library.as_deref()).map_err(|e| Problem::failed(path, e))?;
    let first = app.selected_library;

    let ui = AppWindow::new().map_err(|e| Problem::failed(path, e))?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    apply_settings(&ui, &initial);
    let ctx = Context { app: Rc::new(RefCell::new(app)), ui: ui.as_weak() };
    ctx.update(|app, ui| app.refresh_all(ui));

    let c = ctx.clone();
    ui.on_select_library(move |id| c.update(|app, ui| app.select_library(ui, id)));
    let c = ctx.clone();
    ui.on_select_letter(move |letter| c.update(|app, ui| app.select_letter(ui, &letter)));
    let c = ctx.clone();
    ui.on_select_view(move |view| c.update(|app, ui| app.select_view(ui, view)));
    let c = ctx.clone();
    ui.on_select_artist(move |id| c.update(|app, ui| app.select_artist(ui, id)));
    let c = ctx.clone();
    ui.on_select_album(move |id| c.update(|app, ui| app.select_album(ui, id)));
    let c = ctx.clone();
    ui.on_select_playlist(move |id| c.update(|app, ui| app.select_playlist(ui, id)));
    let c = ctx.clone();
    ui.on_search(move |query| c.update(|app, ui| app.search(ui, &query)));
    let c = ctx.clone();
    ui.on_play_track(move |id| {
        c.update(|app, ui| {
            if let Some(track) = from_ui_id(id) {
                app.play(ui, track);
            }
        })
    });
    let c = ctx.clone();
    ui.on_show_current(move || c.update(|app, ui| app.show_current(ui)));
    let c = ctx.clone();
    ui.on_toggle_play(move || c.update(|app, ui| app.toggle_play(ui)));
    let c = ctx.clone();
    ui.on_previous(move || c.update(|app, ui| app.step(ui, -1)));
    let c = ctx.clone();
    ui.on_next(move || c.update(|app, ui| app.step(ui, 1)));
    let c = ctx.clone();
    ui.on_seek(move |ratio| c.update(|app, ui| app.seek(ui, ratio)));
    let c = ctx.clone();
    ui.on_seek_by(move |seconds| c.update(|app, ui| app.seek_by(ui, seconds)));
    let c = ctx.clone();
    ui.on_move_selection(move |delta| c.update(|app, ui| app.move_selection(ui, delta as isize)));
    let c = ctx.clone();
    ui.on_play_selected(move || c.update(|app, ui| app.play_selected(ui)));
    let c = ctx.clone();
    ui.on_track_action(move |id, action| c.update(|app, ui| app.track_action(ui, id, &action)));
    let c = ctx.clone();
    ui.on_drop_on_playlist(move |playlist, data| {
        c.update(|app, ui| {
            if let (Some(playlist), Some(track)) = (from_ui_id(playlist), payload_track(&data)) {
                app.add_to_playlist(ui, playlist, track);
            }
        })
    });
    let w = ui.as_weak();
    ui.on_select_language(move |language| {
        if let Some(ui) = w.upgrade() {
            select_language(&ui, &language);
        }
    });
    ui.on_track_payload(track_payload);
    ui.on_is_track_payload(|data| payload_track(&data).is_some());

    let c = ctx.clone();
    ui.on_show_database(move || {
        c.update(|app, ui| {
            let track = app.db_path.display().to_string().into();
            app.show_status(ui, Status { kind: StatusKind::Database, track, ..Default::default() });
        })
    });
    let c = ctx.clone();
    ui.on_new_database(move || database_action(&c, choose_new_database));
    let c = ctx.clone();
    ui.on_open_database(move || database_action(&c, choose_existing_database));

    // Préchargement des autres bibliothèques, sur une connexion dédiée.
    let (sender, receiver) = std::sync::mpsc::channel();
    let pending: Vec<(usize, i64)> = ctx.app.borrow().libraries.iter().enumerate()
        .filter(|&(i, _)| Some(i) != first)
        .map(|(i, l)| (i, l.id))
        .collect();
    let preload_path = path.to_owned();
    std::thread::spawn(move || {
        let Ok(db) = Database::open(&preload_path) else { return };
        for (index, id) in pending {
            match Library::load_with_covers(&db, id, COVER_SIZE) {
                Ok(loaded) => {
                    if sender.send((index, loaded)).is_err() {
                        return;
                    }
                }
                Err(error) => eprintln!("preload library {id}: {error}"),
            }
        }
    });
    let c = ctx.clone();
    let preload = Timer::default();
    preload.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        while let Ok((index, (library, pixels))) = receiver.try_recv() {
            c.app.borrow_mut().add_preloaded(index, library, pixels);
        }
    });

    let saved = Rc::new(RefCell::new(initial.clone()));
    let (c, s) = (ctx.clone(), saved.clone());
    let autosave = Timer::default();
    autosave.start(TimerMode::Repeated, Duration::from_secs(1), move || save_settings(&c, &s));
    let (c, s) = (ctx.clone(), saved.clone());
    ui.window().on_close_requested(move || {
        save_settings(&c, &s);
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });

    let c = ctx.clone();
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, PLAYBACK_POLL, move || c.update(|app, ui| app.tick(ui)));

    ui.show().map_err(|e| Problem::failed(path, e))?;
    // L'agrandissement ne s'applique qu'à une fenêtre affichée.
    let w = ui.as_weak();
    let maximized = initial.window.maximized;
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(ui) = w.upgrade() {
            ui.window().set_maximized(maximized);
        }
    });

    SESSION.with(|session| *session.borrow_mut() = Some(Session { ui, ctx, saved, _timers: vec![preload, autosave, ticker] }));
    Ok(())
}

// Menu Fichier : choisir une base puis y basculer, ou signaler le problème dans la barre d'état.
fn database_action(ctx: &Context, choose: fn() -> Result<Option<PathBuf>, Problem>) {
    match choose() {
        Ok(Some(path)) => {
            // Différé : la fenêtre courante est encore dans son propre rappel.
            Timer::single_shot(Duration::ZERO, move || switch_database(path));
        }
        Ok(None) => {}
        Err(problem) => ctx.update(|app, ui| app.show_problem(ui, &problem)),
    }
}

fn switch_database(path: PathBuf) {
    if let Some(session) = SESSION.with(|session| session.borrow_mut().take()) {
        session.save_settings();
        let _ = session.ui.hide();
    }
    match launch(&path) {
        Ok(()) => remember_database(&path),
        Err(problem) => show_setup(None, Some(problem)),
    }
}

fn remember_database(path: &Path) {
    if let Err(error) = location::remember(path) {
        eprintln!("database location: {error}");
    }
}

fn show_setup(missing: Option<&Path>, problem: Option<Problem>) {
    let setup = match SetupWindow::new() {
        Ok(setup) => setup,
        Err(error) => {
            eprintln!("setup window: {error}");
            let _ = slint::quit_event_loop();
            return;
        }
    };
    // Sans base, pas de réglages : l'écran d'accueil est en anglais (Slint suivrait le système).
    if let Err(error) = slint::select_bundled_translation("en") {
        eprintln!("language en: {error}");
    }
    setup.set_missing_path(missing.map(|p| p.display().to_string()).unwrap_or_default().into());
    if let Some(problem) = problem {
        set_problem(&setup, &problem);
    }

    let w = setup.as_weak();
    let finish = move |choice: Result<Option<PathBuf>, Problem>| {
        let Some(setup) = w.upgrade() else { return };
        match choice {
            Ok(Some(path)) => match launch(&path) {
                Ok(()) => {
                    remember_database(&path);
                    let _ = setup.hide();
                    Timer::single_shot(Duration::ZERO, || SETUP.with(|s| s.borrow_mut().take()).map_or((), drop));
                }
                Err(problem) => set_problem(&setup, &problem),
            },
            Ok(None) => {}
            Err(problem) => set_problem(&setup, &problem),
        }
    };
    let f = finish.clone();
    setup.on_create_database(move || f(choose_new_database()));
    let f = finish.clone();
    setup.on_open_database(move || f(choose_existing_database()));
    let f = finish.clone();
    setup.on_import_legacy(move || f(import_legacy_database()));
    let missing = missing.map(Path::to_owned);
    setup.on_retry(move || {
        if let Some(path) = &missing {
            finish(if path.exists() { Ok(Some(path.clone())) } else { Ok(None) });
        }
    });
    setup.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    if let Err(error) = setup.show() {
        eprintln!("setup window: {error}");
    }
    SETUP.with(|s| *s.borrow_mut() = Some(setup));
}

fn set_problem(setup: &SetupWindow, problem: &Problem) {
    setup.set_problem(problem.kind);
    setup.set_problem_path(problem.path.display().to_string().into());
    setup.set_problem_detail(problem.detail.as_str().into());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match location::saved() {
        Some(path) if path.exists() => {
            if let Err(problem) = launch(&path) {
                show_setup(None, Some(problem));
            }
        }
        // Base notée mais absente : disque externe débranché, fichier déplacé…
        Some(path) => show_setup(Some(&path), None),
        None => show_setup(None, None),
    }
    slint::run_event_loop_until_quit()?;
    // Réglages à jour si l'on quitte autrement qu'en fermant la fenêtre (Cmd+Q).
    if let Some(session) = SESSION.with(|session| session.borrow_mut().take()) {
        session.save_settings();
    }
    Ok(())
}
