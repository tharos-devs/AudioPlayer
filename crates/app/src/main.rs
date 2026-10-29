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
use audioplayer_db::{legacy, Database, FileKind, TrackDetails};
use audioplayer_scanner::write::Field;
use library::{format_duration, sort_key, Library};
use settings::Settings;
use slint::{DataTransfer, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

// Glisser-déposer interne : « audioplayer/<sorte>:<index> ».
const TRACK_PAYLOAD_PREFIX: &str = "audioplayer/track:";
const ARTIST_PAYLOAD_PREFIX: &str = "audioplayer/artist:";
const ALBUM_PAYLOAD_PREFIX: &str = "audioplayer/album:";
const SELECTION_PAYLOAD: &str = "audioplayer/selection";

/// Ce qui peut être glissé sur une playlist.
#[derive(Clone, Copy)]
enum Dragged {
    Track(usize),
    Artist(usize),
    Album(usize),
    // Les pistes sélectionnées (sélection multiple), résolues au moment de l'action.
    Selection,
}
// Pochettes affichées jusqu'à 80px ; la base garde des vignettes de 150px.
const COVER_SIZE: u32 = 150;
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

/// Action en attente de la saisie d'un nom.
enum NameRequest {
    // Avec un morceau à y ajouter aussitôt (menu « Ajouter à la playlist → Nouvelle playlist… »).
    NewPlaylist { tracks: Vec<usize> },
    NewLibrary(PathBuf),
    RenamePlaylist(i64),
    RenameLibrary(usize),
}

enum ConfirmRequest {
    DeletePlaylist(i64),
    DeleteLibrary(usize),
    RemoveMissing { library: i64, tracks: Vec<i64> },
    // « Delete from Library » : retire de la base, sans toucher aux fichiers.
    DeleteTracks { library: i64, tracks: Vec<i64> },
}

/// Messages du fil de scan ou de vérification vers l'interface.
enum ScanMessage {
    Progress(audioplayer_scanner::Progress),
    Scanned(Result<audioplayer_scanner::Report, audioplayer_scanner::Error>),
    // Vérification : `None` si le dossier est introuvable, sinon les morceaux absents.
    Checked(Option<Vec<i64>>),
    CheckStopped,
    // Écriture de tags ou d'une pochette dans les fichiers, avant leur relecture.
    Writing { done: usize, total: usize },
    Written {
        written: usize,
        failed: Vec<(PathBuf, String)>,
        artwork: bool,
        result: Result<audioplayer_scanner::Report, audioplayer_scanner::Error>,
    },
}

struct ScanJob {
    library: i64,
    name: String,
    messages: std::sync::mpsc::Receiver<ScanMessage>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct App {
    db: Database,
    db_path: PathBuf,
    scan: Option<ScanJob>,
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
    // Id en base de la playlist affichée.
    selected_playlist: Option<i64>,
    query: String,
    // Playlists de la bibliothèque affichée.
    playlists: Vec<audioplayer_db::Playlist>,
    name_request: Option<NameRequest>,
    confirm_request: Option<ConfirmRequest>,
    recent: (u32, String),
    // Morceaux affichés, dans l'ordre : sert aussi à précédent/suivant.
    visible: Vec<usize>,
    // Ligne de chaque morceau affiché (les en-têtes d'album occupent aussi des lignes).
    rows: HashMap<usize, usize>,
    // Sélection multiple (index de morceaux) et point de départ d'un Maj-clic.
    selection: std::collections::HashSet<usize>,
    anchor: Option<usize>,
    // Modèle affiché : la sélection ne met à jour que les lignes qui changent.
    track_model: Option<Rc<VecModel<TrackItem>>>,
    reveal_serial: i32,
    // Artistes et albums listés, dans l'ordre : pour la navigation au clavier.
    artist_rows: Vec<usize>,
    album_rows: Vec<usize>,
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
    // Tiroir des tags : morceaux montrés (ids en base, stables d'un rechargement à
    // l'autre), lignes d'origine et saisies par ligne.
    tag_tracks: Vec<i64>,
    // Valeur d'origine de chaque champ (et « valeurs multiples ») ; saisies par champ.
    tag_values: HashMap<String, (String, bool)>,
    tag_edits: HashMap<String, String>,
    // Champs visibles (réglage « tags ») et section technique.
    tag_fields: Vec<String>,
    tag_technical: bool,
    // Lecture suspendue pendant l'écriture : position et état à reprendre.
    tag_resume: Option<(Duration, bool)>,
}

impl App {
    /// `library` : nom de la bibliothèque à ouvrir, la première sinon.
    fn new(db: Database, db_path: PathBuf, library: Option<&str>) -> audioplayer_db::Result<Self> {
        let libraries = db.libraries()?;
        let recent_cutoff = db.datetime_from_now(RECENT_MODIFIER)?;

        let mut app = App {
            db,
            db_path,
            scan: None,
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
            playlists: Vec::new(),
            name_request: None,
            confirm_request: None,
            recent: (2, "weeks".into()),
            visible: Vec::new(),
            rows: HashMap::new(),
            selection: Default::default(),
            anchor: None,
            track_model: None,
            reveal_serial: 0,
            artist_rows: Vec::new(),
            album_rows: Vec::new(),
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
            tag_tracks: Vec::new(),
            tag_values: HashMap::new(),
            tag_edits: HashMap::new(),
            tag_fields: settings::TagsSettings::default().fields,
            tag_technical: true,
            tag_resume: None,
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
        self.selection.clear();
        self.anchor = None;
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

    fn refresh_artists(&mut self, ui: &AppWindow) {
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
                favorite: self.library.artists[artist].is_favorite,
                ..Default::default()
            })
            .collect();
        ui.set_artists_count(rows.len() as i32);
        self.artist_rows = rows.iter().map(|row| row.id as usize).collect();
        ui.set_artists(model(rows));
        ui.set_selected_artist(to_ui_id(self.selected_artist));
    }

    fn refresh_albums(&mut self, ui: &AppWindow) {
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
        self.album_rows = ids.clone();
        let rows = ids.into_iter().map(|album| {
            let (artwork, has_artwork) = self.cover(album);
            ListItem {
                id: album as i32,
                label: albums[album].title.as_str().into(),
                favorite: albums[album].is_favorite,
                count: self.album_counts[album] as i32,
                artwork,
                has_artwork,
            }
        });
        ui.set_albums(model(rows.collect()));
        ui.set_selected_album(to_ui_id(self.selected_album));
    }

    fn refresh_playlists(&mut self, ui: &AppWindow) {
        let library = self.selected_library.map(|i| self.libraries[i].id);
        self.playlists = match library.map(|id| self.db.playlists(id)) {
            Some(Ok(playlists)) => playlists,
            Some(Err(error)) => {
                eprintln!("playlists: {error}");
                Vec::new()
            }
            None => Vec::new(),
        };
        if self.selected_playlist.is_some_and(|id| !self.playlists.iter().any(|p| p.id == id)) {
            self.selected_playlist = None;
        }
        let rows: Vec<ListItem> = self
            .playlists
            .iter()
            .map(|playlist| ListItem {
                id: playlist.id as i32,
                label: playlist.name.as_str().into(),
                count: playlist.track_count as i32,
                ..Default::default()
            })
            .collect();
        let menu = ui.global::<TrackMenu>();
        menu.set_playlists(model(rows.clone()));
        menu.set_in_playlist(self.view == View::Playlist);
        menu.set_most_played(self.view == View::MostPlayed);
        ui.set_playlists(model(rows));
        ui.set_selected_playlist(self.selected_playlist.map_or(-1, |id| id as i32));
        let name = self.selected_playlist.and_then(|id| self.playlists.iter().find(|p| p.id == id));
        ui.set_playlist_name(name.map_or("", |p| p.name.as_str()).into());
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
            Some(playlist) => {
                let ids = self.db.playlist_tracks(playlist).unwrap_or_else(|error| {
                    eprintln!("playlist {playlist}: {error}");
                    Vec::new()
                });
                ids.into_iter().filter_map(|id| self.library.track_by_id(id)).collect()
            }
            None => (0..tracks.len())
                .filter(|&t| listed && self.in_scope(t))
                .filter(|&t| self.selected_artist.is_none_or(|artist| self.library.albums[tracks[t].album].artist == artist))
                .filter(|&t| self.selected_album.is_none_or(|album| tracks[t].album == album))
                .filter(|&t| query.is_empty() || self.matches(t, &query))
                .collect(),
        };

        // La sélection ne garde que les morceaux encore affichés.
        let shown: std::collections::HashSet<usize> = ids.iter().copied().collect();
        self.selection.retain(|t| shown.contains(t));
        if self.anchor.is_some_and(|a| !shown.contains(&a)) {
            self.anchor = None;
        }

        // Une playlist garde l'ordre d'ajout ; la bibliothèque est groupée par album.
        if self.view == View::Playlist {
            let items = ids.iter().enumerate().map(|(position, &t)| self.track_item(t, position as i32 + 1, false));
            let rows = Rc::new(VecModel::from(items.collect::<Vec<_>>()));
            ui.set_playlist_items(ModelRc::from(rows.clone()));
            self.track_model = Some(rows);
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
            let rows = Rc::new(VecModel::from(items));
            ui.set_track_items(ModelRc::from(rows.clone()));
            self.track_model = Some(rows);
        }

        let total: i32 = ids.iter().map(|&t| tracks[t].duration).sum();
        ui.set_tracks_count(ids.len() as i32);
        ui.set_tracks_duration(total);
        self.visible = ids;
        self.refresh_tags(ui);
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
            favorite: t.is_favorite,
            selected: self.selection.contains(&track),
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
        ui.set_active_panel(0);
        ui.set_artist_cursor(id);
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
        ui.set_active_panel(1);
        ui.set_album_cursor(id);
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
        let playlist = (id >= 0).then_some(id as i64);
        let toggled_off = self.view == View::Playlist && self.selected_playlist == playlist;
        self.reset_navigation();
        if playlist.is_some() && !toggled_off {
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
        self.select_only(ui, track);
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

    // Tab / Maj+Tab : panneau suivant ou précédent parmi ceux qui ont du contenu.
    fn cycle_panel(&mut self, ui: &AppWindow, direction: i32) {
        let available = [!self.artist_rows.is_empty() && self.view != View::Playlist,
            !self.album_rows.is_empty() && self.view != View::Playlist, true];
        let mut panel = ui.get_active_panel();
        for _ in 0..3 {
            panel = (panel + direction).rem_euclid(3);
            if available[panel as usize] {
                break;
            }
        }
        ui.set_active_panel(panel);
        // En arrivant, le curseur se place sur l'élément sélectionné, sinon le premier.
        match panel {
            0 => self.place_cursor(ui, 0, self.selected_artist.or(self.artist_rows.first().copied())),
            1 => self.place_cursor(ui, 1, self.selected_album.or(self.album_rows.first().copied())),
            _ => {}
        }
    }

    fn place_cursor(&mut self, ui: &AppWindow, panel: i32, item: Option<usize>) {
        let rows = if panel == 0 { &self.artist_rows } else { &self.album_rows };
        let Some(row) = item.and_then(|item| rows.iter().position(|&r| r == item)) else { return };
        let id = rows[row] as i32;
        self.reveal_serial += 1;
        if panel == 0 {
            ui.set_artist_cursor(id);
            ui.set_artist_reveal_row(row as i32);
            ui.set_artist_reveal_serial(self.reveal_serial);
        } else {
            ui.set_album_cursor(id);
            ui.set_album_reveal_row(row as i32);
            ui.set_album_reveal_serial(self.reveal_serial);
        }
    }

    // Entrée : valide l'élément sous le curseur, comme un clic.
    fn activate(&mut self, ui: &AppWindow) {
        match ui.get_active_panel() {
            0 => {
                if let Some(artist) = from_ui_id(ui.get_artist_cursor()).filter(|a| self.artist_rows.contains(a)) {
                    self.select_artist(ui, artist as i32);
                }
            }
            1 => {
                if let Some(album) = from_ui_id(ui.get_album_cursor()).filter(|a| self.album_rows.contains(a)) {
                    self.select_album(ui, album as i32);
                }
            }
            _ => self.play_selected(ui),
        }
    }

    // ↑/↓ : élément précédent/suivant du panneau actif ; sans position, le premier (↓) ou le dernier (↑).
    fn move_selection(&mut self, ui: &AppWindow, delta: isize, extend: bool) {
        let panel = ui.get_active_panel();
        if panel == 0 || panel == 1 {
            let (rows, cursor) = if panel == 0 {
                (&self.artist_rows, ui.get_artist_cursor())
            } else {
                (&self.album_rows, ui.get_album_cursor())
            };
            if rows.is_empty() {
                return;
            }
            let current = from_ui_id(cursor).and_then(|c| rows.iter().position(|&r| r == c));
            let index = match current {
                Some(index) => index.saturating_add_signed(delta).min(rows.len() - 1),
                None if delta < 0 => rows.len() - 1,
                None => 0,
            };
            let item = rows[index];
            self.place_cursor(ui, panel, Some(item));
            return;
        }
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
        if extend {
            self.anchor = self.anchor.or(current.map(|c| self.visible[c])).or(Some(track));
            self.select_range(ui, track);
        } else {
            self.select_only(ui, track);
        }
        self.reveal(ui, track);
    }

    // Applique une nouvelle sélection en ne retouchant que les lignes qui changent.
    fn set_selection(&mut self, ui: &AppWindow, selection: std::collections::HashSet<usize>, current: Option<usize>) {
        let changed: Vec<usize> = self.selection.symmetric_difference(&selection).copied().collect();
        self.selection = selection;
        if let Some(model) = &self.track_model {
            for track in changed {
                if let Some(mut item) = self.rows.get(&track).and_then(|&row| model.row_data(row).map(|i| (row, i))) {
                    item.1.selected = self.selection.contains(&track);
                    model.set_row_data(item.0, item.1);
                }
            }
        }
        ui.set_selected_track(to_ui_id(current));
        self.refresh_tags(ui);
    }

    fn select_only(&mut self, ui: &AppWindow, track: usize) {
        self.anchor = Some(track);
        self.set_selection(ui, std::iter::once(track).collect(), Some(track));
    }

    // Maj : de l'ancre jusqu'à `track`, dans l'ordre affiché.
    fn select_range(&mut self, ui: &AppWindow, track: usize) {
        let position = |t: usize| self.visible.iter().position(|&v| v == t);
        let (Some(to), Some(from)) = (position(track), self.anchor.and_then(position)) else {
            return self.select_only(ui, track);
        };
        let range = self.visible[from.min(to)..=from.max(to)].iter().copied().collect();
        self.set_selection(ui, range, Some(track));
    }

    // Clic dans la liste : simple, Cmd/Ctrl (ajoute ou retire) ou Maj (plage).
    fn track_clicked(&mut self, ui: &AppWindow, track: usize, toggle: bool, extend: bool) {
        if extend && self.anchor.is_some() {
            self.select_range(ui, track);
        } else if toggle {
            let mut selection = self.selection.clone();
            if !selection.remove(&track) {
                selection.insert(track);
            }
            self.anchor = Some(track);
            self.set_selection(ui, selection, Some(track));
        } else {
            self.select_only(ui, track);
        }
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
            self.select_only(ui, track);
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
            let artwork = self.now_track().and_then(|(lib, t)| self.db.artwork(lib.albums[t.album].id, COVER_SIZE).ok().flatten());
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
        self.poll_scan(ui);
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
        self.show_status_for(ui, status, STATUS_MESSAGE_DURATION);
    }

    fn show_status_for(&self, ui: &AppWindow, status: Status, duration: Duration) {
        ui.set_status(status);
        let weak = ui.as_weak();
        self.status_timer.start(TimerMode::SingleShot, duration, move || {
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
                ..Default::default()
            },
        );
    }

    fn track_action(&mut self, ui: &AppWindow, id: i32, action: &str) {
        let Some(track) = from_ui_id(id).filter(|&t| t < self.library.tracks.len()) else { return };
        match action {
            "show-album" => {
                self.reveal_album(ui, self.library.tracks[track].album);
                self.select_only(ui, track);
            }
            _ => {
                // Clic droit dans une sélection multiple : l'action vaut pour toute la sélection.
                let item = if self.selection.len() > 1 && self.selection.contains(&track) { Dragged::Selection } else { Dragged::Track(track) };
                match action {
                    "remove-from-playlist" => self.remove_from_playlist(ui, item),
                    "reset-plays" => self.reset_plays(ui, item),
                    _ => self.item_action(ui, item, action),
                }
            }
        }
    }

    // Favori d'un morceau, d'un album ou d'un artiste.
    fn toggle_favorite(&mut self, ui: &AppWindow, item: Dragged) {
        let (favorite, view) = match item {
            Dragged::Album(a) => match self.db.toggle_album_favorite(self.library.albums[a].id) {
                Ok(favorite) => {
                    self.library.albums[a].is_favorite = favorite;
                    (favorite, View::FavoriteAlbums)
                }
                Err(error) => return eprintln!("favorite: {error}"),
            },
            Dragged::Artist(a) => match self.db.toggle_artist_favorite(self.library.artists[a].id) {
                Ok(favorite) => {
                    self.library.artists[a].is_favorite = favorite;
                    (favorite, View::FavoriteArtists)
                }
                Err(error) => return eprintln!("favorite: {error}"),
            },
            // Plusieurs pistes : toutes en favori, ou toutes retirées si elles l'étaient déjà.
            Dragged::Track(_) | Dragged::Selection => {
                let tracks = self.tracks_of(item);
                let target = !tracks.iter().all(|&t| self.library.tracks[t].is_favorite);
                let to_change: Vec<usize> = tracks.into_iter().filter(|&t| self.library.tracks[t].is_favorite != target).collect();
                for t in to_change {
                    match self.db.toggle_track_favorite(self.library.tracks[t].id) {
                        Ok(favorite) => self.library.tracks[t].is_favorite = favorite,
                        Err(error) => return eprintln!("favorite: {error}"),
                    }
                }
                (target, View::FavoriteTracks)
            }
        };
        let lib = &self.library;
        ui.set_favorite_tracks_count(lib.tracks.iter().filter(|t| t.is_favorite).count() as i32);
        ui.set_favorite_albums_count(lib.albums.iter().filter(|a| a.is_favorite).count() as i32);
        ui.set_favorite_artists_count(lib.artists.iter().filter(|a| a.is_favorite).count() as i32);
        if self.view == view {
            self.update_scope();
            self.refresh_navigation(ui);
        } else {
            self.refresh_artists(ui);
            self.refresh_albums(ui);
            self.refresh_tracks(ui);
        }
        let count = self.tracks_of(item).len() as i32;
        let kind = match (favorite, item) {
            (true, Dragged::Selection) => StatusKind::SelectionFavorited,
            (false, Dragged::Selection) => StatusKind::SelectionUnfavorited,
            (true, _) => StatusKind::FavoriteAdded,
            (false, _) => StatusKind::FavoriteRemoved,
        };
        self.show_status(ui, Status { kind, track: self.label_of(item).into(), count, ..Default::default() });
    }

    fn reset_plays(&mut self, ui: &AppWindow, item: Dragged) {
        let tracks = self.tracks_of(item);
        let ids: Vec<i64> = tracks.iter().map(|&t| self.library.tracks[t].id).collect();
        if let Err(error) = self.db.reset_play_counts(&ids) {
            return eprintln!("reset plays: {error}");
        }
        for t in tracks {
            self.library.tracks[t].play_count = 0;
        }
        ui.set_most_played_count(self.library.tracks.iter().filter(|t| t.play_count > 0).count() as i32);
        if self.view == View::MostPlayed {
            self.update_scope();
            self.refresh_navigation(ui);
        }
    }

    // Lance tous les morceaux d'un élément, dans l'ordre (file vide au départ).
    fn play_items(&mut self, ui: &AppWindow, item: Dragged) {
        let (Some(library), tracks) = (self.selected_library, self.tracks_of(item)) else { return };
        let Some(&first) = tracks.first() else { return };
        self.now = Some(NowPlaying { library, track: first, queue: tracks, index: Some(0), next: None });
        self.failures = 0;
        self.start(ui);
    }

    // « Add to Queue » : à la fin de la file. Sans lecture en cours, ou depuis une autre
    // bibliothèque, les morceaux sont lancés.
    fn enqueue(&mut self, ui: &AppWindow, item: Dragged) {
        let tracks = self.tracks_of(item);
        let same_library = self.now.as_ref().is_some_and(|now| Some(now.library) == self.selected_library);
        if tracks.is_empty() {
            return;
        }
        if !same_library || self.stopped {
            return self.play_items(ui, item);
        }
        let now = self.now.as_mut().unwrap();
        // Un morceau n'est qu'une fois dans la file (le morceau en cours reste à sa place).
        let current = now.track;
        now.queue.retain(|t| *t == current || !tracks.contains(t));
        let index = match now.queue.iter().position(|&t| t == current) {
            Some(index) => index,
            None => {
                now.queue.insert(0, current);
                0
            }
        };
        now.index = Some(index);
        now.queue.extend(tracks);
        self.queue_next();
        let kind = if matches!(item, Dragged::Selection) { StatusKind::SelectionEnqueued } else { StatusKind::Enqueued };
        let count = self.tracks_of(item).len() as i32;
        self.show_status(ui, Status { kind, track: self.label_of(item).into(), count, ..Default::default() });
    }

    // Révèle le fichier (morceau) ou ouvre le dossier (album) dans le gestionnaire de fichiers.
    fn show_on_disk(&self, item: Dragged) {
        let path = match item {
            Dragged::Track(t) => PathBuf::from(&self.library.tracks[t].path),
            _ => match self.tracks_of(item).first().and_then(|&t| Path::new(&self.library.tracks[t].path).parent()) {
                Some(dir) => dir.to_owned(),
                None => return,
            },
        };
        let reveal = matches!(item, Dragged::Track(_));
        let result = if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("open");
            if reveal {
                command.arg("-R");
            }
            command.arg(&path).spawn()
        } else if cfg!(target_os = "windows") {
            let arg = if reveal { format!("/select,{}", path.display()) } else { path.display().to_string() };
            std::process::Command::new("explorer").arg(arg).spawn()
        } else {
            let dir = if reveal { path.parent().map(Path::to_owned).unwrap_or(path) } else { path };
            std::process::Command::new("xdg-open").arg(dir).spawn()
        };
        if let Err(error) = result {
            eprintln!("show on disk: {error}");
        }
    }

    // « Set Album Artwork » : une image choisie devient la vignette de l'album (150px).
    fn set_album_artwork(&mut self, ui: &AppWindow, album: usize) {
        // Les fichiers reçoivent aussi l'image : pas pendant un scan.
        if self.scan.is_some() {
            return;
        }
        let dialog = rfd::FileDialog::new().add_filter("Image", &["jpg", "jpeg", "png", "webp", "gif", "bmp"]);
        let Some(path) = dialog.pick_file() else { return };
        let Some(thumbnail) = std::fs::read(&path).ok().and_then(|data| audioplayer_scanner::artwork::thumbnail(&data)) else {
            return eprintln!("{}: not a readable image", path.display());
        };
        let id = self.library.albums[album].id;
        if let Some(pixels) = covers::decode(&thumbnail.2) {
            self.add_covers(vec![(id, pixels)]);
        }
        let cover = (thumbnail.1.clone(), thumbnail.2.clone());
        if let Err(error) = self.db.set_artwork(id, &[thumbnail]) {
            return eprintln!("artwork {id}: {error}");
        }
        let paths = self.library.tracks.iter().filter(|t| t.album == album).map(|t| PathBuf::from(&t.path)).collect();
        self.write_files(ui, paths, Vec::new(), Some(cover));
        self.refresh_albums(ui);
        self.refresh_tracks(ui);
        self.published_track = None;
        self.refresh_player(ui);
    }

    // Morceaux d'un élément glissé, dans l'ordre de la bibliothèque et limités à la vue
    // en cours (en « Added last… », un artiste n'apporte que ses morceaux récents).
    fn tracks_of(&self, item: Dragged) -> Vec<usize> {
        let tracks = &self.library.tracks;
        let mut ids: Vec<usize> = match item {
            Dragged::Track(track) => return vec![track],
            Dragged::Artist(artist) => (0..tracks.len())
                .filter(|&t| self.library.albums[tracks[t].album].artist == artist && self.in_scope(t))
                .collect(),
            Dragged::Album(album) => (0..tracks.len()).filter(|&t| tracks[t].album == album && self.in_scope(t)).collect(),
            // Dans l'ordre affiché, pas dans l'ordre des clics.
            Dragged::Selection => return self.visible.iter().copied().filter(|t| self.selection.contains(t)).collect(),
        };
        ids.sort_by(|&a, &b| self.compare(a, b));
        ids
    }

    fn label_of(&self, item: Dragged) -> String {
        match item {
            Dragged::Track(track) => self.library.tracks[track].title.clone(),
            Dragged::Artist(artist) => self.library.artists[artist].name.clone(),
            Dragged::Album(album) => self.library.albums[album].title.clone(),
            // Les messages d'une sélection donnent un nombre, pas un nom.
            Dragged::Selection => String::new(),
        }
    }

    fn add_dragged(&mut self, ui: &AppWindow, playlist: i64, item: Dragged) {
        let Some(name) = self.playlists.iter().find(|p| p.id == playlist).map(|p| p.name.clone()) else { return };
        let ids: Vec<i64> = self.tracks_of(item).into_iter().map(|t| self.library.tracks[t].id).collect();
        let added = match self.db.add_to_playlist(playlist, &ids) {
            Ok(added) => added,
            Err(error) => return eprintln!("playlist {playlist}: {error}"),
        };
        self.refresh_playlists(ui);
        if self.view == View::Playlist && self.selected_playlist == Some(playlist) {
            self.refresh_tracks(ui);
        }
        let kind = match (added, item) {
            (0, Dragged::Selection) => StatusKind::SelectionAlreadyInPlaylist,
            (0, _) => StatusKind::AlreadyInPlaylist,
            (_, Dragged::Track(_)) => StatusKind::AddedToPlaylist,
            (_, Dragged::Selection) => StatusKind::SelectionAdded,
            _ => StatusKind::TracksAdded,
        };
        let label = self.label_of(item).into();
        self.show_status(ui, Status { kind, track: label, playlist: name.into(), count: added as i32, ..Default::default() });
    }

    // Glisser dans une playlist affichée : déplacer ses morceaux, ou insérer un artiste/album.
    fn place_in_playlist(&mut self, ui: &AppWindow, item: Dragged, row: usize) {
        let Some(playlist) = self.selected_playlist.filter(|_| self.view == View::Playlist) else { return };
        let ids: Vec<i64> = self.tracks_of(item).into_iter().map(|t| self.library.tracks[t].id).collect();
        if let Err(error) = self.db.place_in_playlist(playlist, row, &ids) {
            return eprintln!("playlist {playlist}: {error}");
        }
        self.refresh_playlists(ui);
        self.refresh_tracks(ui);
    }

    // Menus contextuels : actions communes aux morceaux, albums et artistes.
    fn item_action(&mut self, ui: &AppWindow, item: Dragged, action: &str) {
        match action {
            "enqueue" => self.enqueue(ui, item),
            "show-on-disk" => self.show_on_disk(item),
            "set-artwork" => {
                if let Dragged::Album(album) = item {
                    self.set_album_artwork(ui, album);
                }
            }
            "favorite" => self.toggle_favorite(ui, item),
            "rescan" => self.start_rescan(ui, item),
            "edit-tags" => {
                if self.scan.is_none() {
                    let ids = self.tracks_of(item).into_iter().map(|t| self.library.tracks[t].id).collect();
                    ui.set_tags_open(true);
                    ui.set_tags_editing(false);
                    self.show_tags(ui, ids);
                    ui.set_tags_editing(!self.tag_tracks.is_empty());
                }
            }
            "delete" => {
                let Some(library) = self.selected_library.map(|i| self.libraries[i].id) else { return };
                let tracks: Vec<i64> = self.tracks_of(item).into_iter().map(|t| self.library.tracks[t].id).collect();
                ui.set_confirm_count(tracks.len() as i32);
                let label = self.label_of(item);
                let kind = if matches!(item, Dragged::Selection) { ConfirmKind::DeleteSelection } else { ConfirmKind::DeleteTracks };
                self.ask_confirmation(ui, kind, &label, ConfirmRequest::DeleteTracks { library, tracks });
            }
            "add-to-new-playlist" => {
                let tracks = self.tracks_of(item);
                let name = if matches!(item, Dragged::Track(_)) { String::new() } else { self.label_of(item) };
                self.ask_name(ui, NameDialogKind::NewPlaylist, &name, NameRequest::NewPlaylist { tracks });
            }
            _ => {
                if let Some(playlist) = action.strip_prefix("add-to-playlist:").and_then(|id| id.parse().ok()) {
                    self.add_dragged(ui, playlist, item);
                }
            }
        }
    }

    fn remove_from_playlist(&mut self, ui: &AppWindow, item: Dragged) {
        let Some(playlist) = self.selected_playlist.filter(|_| self.view == View::Playlist) else { return };
        let ids: Vec<i64> = self.tracks_of(item).into_iter().map(|t| self.library.tracks[t].id).collect();
        if let Err(error) = self.db.remove_from_playlist(playlist, &ids) {
            return eprintln!("playlist {playlist}: {error}");
        }
        self.refresh_playlists(ui);
        self.refresh_tracks(ui);
    }

    fn ask_name(&mut self, ui: &AppWindow, kind: NameDialogKind, text: &str, request: NameRequest) {
        self.name_request = Some(request);
        ui.set_dialog_kind(kind);
        ui.set_dialog_text(text.into());
        ui.set_dialog_taken(false);
        ui.invoke_open_dialog();
    }

    fn name_accepted(&mut self, ui: &AppWindow, name: &str) {
        let name = name.trim();
        let Some(request) = self.name_request.as_ref() else { return ui.invoke_close_dialog() };
        if name.is_empty() {
            return;
        }
        let result = match *request {
            NameRequest::NewPlaylist { ref tracks } => {
                let Some(library) = self.selected_library.map(|i| self.libraries[i].id) else { return };
                let ids: Vec<i64> = tracks.iter().map(|&t| self.library.tracks[t].id).collect();
                match self.db.create_playlist(library, name) {
                    Ok(Some(playlist)) => {
                        if let Err(error) = self.db.add_to_playlist(playlist, &ids) {
                            eprintln!("playlist {playlist}: {error}");
                        }
                        Ok(true)
                    }
                    Ok(None) => Ok(false),
                    Err(error) => Err(error),
                }
            }
            NameRequest::RenamePlaylist(playlist) => self.db.rename_playlist(playlist, name),
            NameRequest::NewLibrary(ref folder) => {
                let root = folder.to_string_lossy().into_owned();
                match self.db.create_library(name, &root) {
                    Ok(Some(id)) => {
                        self.name_request = None;
                        ui.invoke_close_dialog();
                        let old: Vec<i64> = self.libraries.iter().map(|l| l.id).collect();
                        self.libraries = self.db.libraries().unwrap_or_default();
                        // Les index ont pu bouger (tri par nom) : catalogues et lecture recalés.
                        self.reindex_libraries(ui, &old, id);
                        if let Some(index) = self.libraries.iter().position(|l| l.id == id) {
                            self.start_scan(ui, index, audioplayer_scanner::Mode::Changes);
                        }
                        return;
                    }
                    other => other.map(|created| created.is_some()),
                }
            }
            NameRequest::RenameLibrary(index) => {
                let result = self.db.rename_library(self.libraries[index].id, name);
                if let Ok(true) = result {
                    self.libraries[index].name = name.to_owned();
                }
                result
            }
        };
        match result {
            Ok(true) => {
                self.name_request = None;
                ui.invoke_close_dialog();
                self.refresh_all(ui);
            }
            // Nom déjà pris : la saisie reste ouverte avec un avertissement.
            Ok(false) => ui.set_dialog_taken(true),
            Err(error) => {
                eprintln!("rename: {error}");
                self.name_request = None;
                ui.invoke_close_dialog();
            }
        }
    }

    fn ask_confirmation(&mut self, ui: &AppWindow, kind: ConfirmKind, subject: &str, request: ConfirmRequest) {
        self.confirm_request = Some(request);
        ui.set_confirm_kind(kind);
        ui.set_confirm_subject(subject.into());
        ui.invoke_open_confirm();
    }

    fn confirmed(&mut self, ui: &AppWindow) {
        match self.confirm_request.take() {
            Some(ConfirmRequest::DeletePlaylist(playlist)) => {
                if let Err(error) = self.db.delete_playlist(playlist) {
                    return eprintln!("playlist {playlist}: {error}");
                }
                if self.selected_playlist == Some(playlist) {
                    self.reset_navigation();
                }
                self.refresh_navigation(ui);
            }
            Some(ConfirmRequest::DeleteLibrary(index)) => self.delete_library(ui, index),
            Some(ConfirmRequest::RemoveMissing { library, tracks } | ConfirmRequest::DeleteTracks { library, tracks }) => {
                if let Err(error) = self.db.remove_tracks(library, &tracks) {
                    return eprintln!("library {library}: {error}");
                }
                self.reload_library(ui, library);
            }
            None => {}
        }
    }

    // Les index des bibliothèques changent : catalogues et lecture sont recalés par id.
    fn delete_library(&mut self, ui: &AppWindow, index: usize) {
        let id = self.libraries[index].id;
        if let Err(error) = self.db.delete_library(id) {
            return eprintln!("library {id}: {error}");
        }
        if self.now.as_ref().is_some_and(|now| now.library == index) {
            self.engine.stop();
            self.now = None;
            self.is_playing = false;
        }
        let old: Vec<i64> = self.libraries.iter().map(|l| l.id).collect();
        self.libraries = self.db.libraries().unwrap_or_default();
        let remap = |i: usize| self.libraries.iter().position(|l| l.id == old[i]);
        let catalogs = std::mem::take(&mut self.catalogs);
        self.catalogs = catalogs.into_iter().filter_map(|(i, library)| Some((remap(i)?, library))).collect();
        let now_library = self.now.as_ref().and_then(|now| remap(now.library));
        if let (Some(now), Some(library)) = (self.now.as_mut(), now_library) {
            now.library = library;
        }
        let current = self.selected_library.filter(|&i| i != index).and_then(remap);
        self.selected_library = None;
        self.library = Library::default();
        match current.or((!self.libraries.is_empty()).then_some(0)) {
            Some(next) => {
                if let Err(error) = self.load_library(next) {
                    eprintln!("library: {error}");
                }
            }
            None => self.reset_navigation(),
        }
        ui.set_selected_track(-1);
        self.refresh_all(ui);
    }

    fn playlist_menu(&mut self, ui: &AppWindow, action: &str) {
        let current = self.selected_playlist.and_then(|id| self.playlists.iter().find(|p| p.id == id)).cloned();
        match (action, current) {
            ("new-playlist", _) => self.ask_name(ui, NameDialogKind::NewPlaylist, "", NameRequest::NewPlaylist { tracks: Vec::new() }),
            ("rename-playlist", Some(playlist)) => {
                self.ask_name(ui, NameDialogKind::RenamePlaylist, &playlist.name, NameRequest::RenamePlaylist(playlist.id))
            }
            ("delete-playlist", Some(playlist)) => {
                self.ask_confirmation(ui, ConfirmKind::DeletePlaylist, &playlist.name, ConfirmRequest::DeletePlaylist(playlist.id))
            }
            _ => {}
        }
    }

    fn library_menu(&mut self, ui: &AppWindow, action: &str) {
        if action == "new-library" {
            return self.new_library(ui);
        }
        let Some(index) = self.selected_library else { return };
        let name = self.libraries[index].name.clone();
        match action {
            "rename-library" => self.ask_name(ui, NameDialogKind::RenameLibrary, &name, NameRequest::RenameLibrary(index)),
            "scan" => self.start_scan(ui, index, audioplayer_scanner::Mode::Changes),
            "rescan" => self.start_scan(ui, index, audioplayer_scanner::Mode::Full),
            "health" => self.check_library(ui, index),
            "delete-library" => self.ask_confirmation(ui, ConfirmKind::DeleteLibrary, &name, ConfirmRequest::DeleteLibrary(index)),
            _ => {}
        }
    }

    // Un dossier ne peut appartenir qu'à une bibliothèque : ni égal, ni contenu, ni contenant.
    fn new_library(&mut self, ui: &AppWindow) {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else { return };
        let overlapping = self.libraries.iter().find(|l| {
            let root = Path::new(&l.root_path);
            folder.starts_with(root) || root.starts_with(&folder)
        });
        if let Some(library) = overlapping {
            let status = Status {
                kind: StatusKind::FolderInLibrary,
                track: folder.display().to_string().into(),
                playlist: library.name.as_str().into(),
                ..Default::default()
            };
            return self.show_status(ui, status);
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.ask_name(ui, NameDialogKind::NewLibrary, &name, NameRequest::NewLibrary(folder));
    }

    // Après un changement de la liste des bibliothèques : catalogues gardés et lecture
    // recalés par id (`old` = ids d'avant, dans l'ordre des anciens index), `select` ouverte.
    fn reindex_libraries(&mut self, ui: &AppWindow, old: &[i64], select: i64) {
        let remap = |i: usize| old.get(i).and_then(|id| self.libraries.iter().position(|l| l.id == *id));
        if let Some(previous) = self.selected_library {
            self.catalogs.insert(previous, std::mem::take(&mut self.library));
        }
        let catalogs = std::mem::take(&mut self.catalogs);
        self.catalogs = catalogs.into_iter().filter_map(|(i, library)| Some((remap(i)?, library))).collect();
        let playing = self.now.as_ref().and_then(|now| remap(now.library));
        match (self.now.as_mut(), playing) {
            (Some(now), Some(library)) => now.library = library,
            (Some(_), None) => {
                self.engine.stop();
                self.now = None;
                self.is_playing = false;
            }
            _ => {}
        }
        self.selected_library = None;
        if let Some(index) = self.libraries.iter().position(|l| l.id == select) {
            if let Err(error) = self.load_library(index) {
                eprintln!("library: {error}");
            }
        }
        ui.set_selected_track(-1);
        self.refresh_all(ui);
    }

    fn start_scan(&mut self, ui: &AppWindow, index: usize, mode: audioplayer_scanner::Mode) {
        if self.scan.is_some() {
            return;
        }
        let library = self.libraries[index].clone();
        let (sender, messages) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (db_path, stop) = (self.db_path.clone(), cancel.clone());
        std::thread::spawn(move || {
            let progress = sender.clone();
            let result = audioplayer_scanner::scan(&db_path, library.id, Path::new(&library.root_path), mode, &stop, &move |p| {
                let _ = progress.send(ScanMessage::Progress(p));
            });
            let _ = sender.send(ScanMessage::Scanned(result));
        });
        self.scan = Some(ScanJob { library: library.id, name: library.name.clone(), messages, cancel });
        ui.set_scanning(true);
        ui.set_status(Status { kind: StatusKind::ScanListing, track: library.name.into(), ..Default::default() });
    }

    // « Rescan Audio Tags » : relit les fichiers d'un artiste, d'un album ou d'un morceau.
    fn start_rescan(&mut self, ui: &AppWindow, item: Dragged) {
        let Some(index) = self.selected_library.filter(|_| self.scan.is_none()) else { return };
        let library = self.libraries[index].clone();
        let paths: Vec<PathBuf> = self.tracks_of(item).into_iter().map(|t| PathBuf::from(&self.library.tracks[t].path)).collect();
        let (sender, messages) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (db_path, stop) = (self.db_path.clone(), cancel.clone());
        std::thread::spawn(move || {
            let progress = sender.clone();
            let result = audioplayer_scanner::rescan_files(&db_path, library.id, &paths, &stop, &move |p| {
                let _ = progress.send(ScanMessage::Progress(p));
            });
            let _ = sender.send(ScanMessage::Scanned(result));
        });
        self.scan = Some(ScanJob { library: library.id, name: library.name.clone(), messages, cancel });
        ui.set_scanning(true);
    }

    // Bouton « Arrêter » de la barre d'état : le scan s'arrête au plus vite, sans rien retirer.
    fn stop_scan(&self) {
        if let Some(job) = &self.scan {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // Vérification : chaque fichier de la bibliothèque existe-t-il encore ?
    fn check_library(&mut self, ui: &AppWindow, index: usize) {
        if self.scan.is_some() {
            return;
        }
        let library = self.libraries[index].clone();
        let tracks: Vec<(i64, String)> = self.library.tracks.iter().map(|t| (t.id, t.path.clone())).collect();
        let (sender, messages) = std::sync::mpsc::channel();
        let root = PathBuf::from(&library.root_path);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let stopped = || stop.load(std::sync::atomic::Ordering::Relaxed);
            let result = root.is_dir().then(|| {
                tracks.into_iter().take_while(|_| !stopped()).filter(|(_, path)| !Path::new(path).exists()).map(|(id, _)| id).collect()
            });
            let _ = sender.send(if stopped() { ScanMessage::CheckStopped } else { ScanMessage::Checked(result) });
        });
        self.scan = Some(ScanJob { library: library.id, name: library.name.clone(), messages, cancel });
        ui.set_scanning(true);
        ui.set_status(Status { kind: StatusKind::ScanListing, track: library.name.into(), ..Default::default() });
    }

    fn poll_scan(&mut self, ui: &AppWindow) {
        let Some(job) = self.scan.as_ref() else { return };
        let (library, name) = (job.library, job.name.clone());
        let mut finished = None;
        while let Ok(message) = job.messages.try_recv() {
            match message {
                ScanMessage::Progress(audioplayer_scanner::Progress::Listing { folders }) => {
                    ui.set_status(Status { kind: StatusKind::ScanListing, track: name.as_str().into(), count: folders as i32, ..Default::default() });
                }
                ScanMessage::Progress(audioplayer_scanner::Progress::Reading { done, total, folder }) => {
                    let folder = Path::new(&folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.set_status(Status {
                        kind: StatusKind::ScanReading,
                        track: name.as_str().into(),
                        count: done as i32,
                        total: total as i32,
                        playlist: folder.into(),
                        ..Default::default()
                    });
                }
                ScanMessage::Writing { done, total } => {
                    ui.set_status(Status { kind: StatusKind::TagsWriting, count: done as i32 + 1, total: total as i32, ..Default::default() });
                }
                other => finished = Some(other),
            }
        }
        let Some(message) = finished else { return };
        self.scan = None;
        ui.set_scanning(false);
        let root = self.libraries.iter().find(|l| l.id == library).map(|l| l.root_path.clone()).unwrap_or_default();
        let status = |kind| Status { kind, track: name.as_str().into(), playlist: root.as_str().into(), ..Default::default() };
        match message {
            ScanMessage::Scanned(Ok(report)) => {
                self.reload_library(ui, library);
                self.show_status_for(ui, Status {
                    kind: if report.cancelled { StatusKind::ScanStopped } else { StatusKind::ScanFinished },
                    track: name.as_str().into(),
                    count: report.added as i32,
                    total: report.updated as i32,
                    removed: report.removed as i32,
                    ..Default::default()
                }, Duration::from_secs(10));
            }
            ScanMessage::Scanned(Err(audioplayer_scanner::Error::RootMissing(_))) => self.show_status_for(ui, status(StatusKind::ScanRootMissing), Duration::from_secs(10)),
            ScanMessage::Scanned(Err(audioplayer_scanner::Error::RootEmpty(_))) => self.show_status_for(ui, status(StatusKind::ScanRootEmpty), Duration::from_secs(10)),
            ScanMessage::Scanned(Err(error)) => {
                let status = Status { kind: StatusKind::ScanFailed, track: name.as_str().into(), playlist: error.to_string().into(), ..Default::default() };
                self.show_status_for(ui, status, Duration::from_secs(10));
            }
            ScanMessage::Checked(None) => self.show_status_for(ui, status(StatusKind::ScanRootMissing), Duration::from_secs(10)),
            ScanMessage::Checked(Some(missing)) if missing.is_empty() => {
                ui.set_status(Status::default());
                self.show_status(ui, status(StatusKind::LibraryHealthy));
            }
            ScanMessage::Checked(Some(missing)) => {
                ui.set_status(Status::default());
                ui.set_confirm_count(missing.len() as i32);
                self.ask_confirmation(ui, ConfirmKind::RemoveMissing, &name, ConfirmRequest::RemoveMissing { library, tracks: missing });
            }
            ScanMessage::CheckStopped => ui.set_status(Status::default()),
            ScanMessage::Written { written, failed, artwork, result } => {
                ui.set_status(Status::default());
                self.files_written(ui, library, written, failed, artwork, result);
            }
            ScanMessage::Progress(_) | ScanMessage::Writing { .. } => {}
        }
    }

    // Catalogue relu depuis la base ; le morceau en cours et sa file sont retrouvés par id.
    fn reload_library(&mut self, ui: &AppWindow, library_id: i64) {
        let Some(index) = self.libraries.iter().position(|l| l.id == library_id) else { return };
        let playing = self.now.as_ref().filter(|now| now.library == index).and_then(|now| {
            let lib = self.library_at(index)?;
            let ids = |tracks: &[usize]| tracks.iter().map(|&t| lib.tracks[t].id).collect::<Vec<_>>();
            Some((lib.tracks[now.track].id, ids(&now.queue)))
        });
        self.catalogs.remove(&index);
        if self.selected_library == Some(index) {
            // La navigation est retrouvée par id : les index changent avec le catalogue.
            let (view, letter, playlist, query) = (self.view, self.letter.clone(), self.selected_playlist, self.query.clone());
            let artist = self.selected_artist.map(|a| self.library.artists[a].id);
            let album = self.selected_album.map(|a| self.library.albums[a].id);
            self.selected_library = None;
            self.library = Library::default();
            if let Err(error) = self.load_library(index) {
                return eprintln!("library {library_id}: {error}");
            }
            (self.view, self.letter, self.selected_playlist, self.query) = (view, letter, playlist, query);
            self.update_scope();
            self.selected_artist = artist.and_then(|id| self.library.artists.iter().position(|a| a.id == id));
            self.selected_album = album.and_then(|id| self.library.albums.iter().position(|a| a.id == id));
            ui.set_selected_track(-1);
            self.refresh_all(ui);
        } else if let Ok((library, pixels)) = Library::load_with_covers(&self.db, library_id, COVER_SIZE) {
            self.add_covers(pixels);
            self.catalogs.insert(index, library);
        }
        if let Some((track_id, queue_ids)) = playing {
            let lib = self.library_at(index);
            let track = lib.and_then(|l| l.track_by_id(track_id));
            match (lib, track) {
                (Some(lib), Some(track)) => {
                    let queue: Vec<usize> = queue_ids.iter().filter_map(|&id| lib.track_by_id(id)).collect();
                    let now = self.now.as_mut().unwrap();
                    now.index = queue.iter().position(|&t| t == track);
                    now.track = track;
                    now.queue = queue;
                    self.queue_next();
                }
                // Le morceau a disparu : la lecture s'arrête.
                _ => {
                    self.engine.stop();
                    self.now = None;
                    self.is_playing = false;
                }
            }
            self.published_track = None;
            self.refresh_player(ui);
        }
    }

    // Le tiroir suit la sélection, sauf pendant une édition.
    fn refresh_tags(&mut self, ui: &AppWindow) {
        if !ui.get_tags_open() || ui.get_tags_editing() {
            return;
        }
        let ids: Vec<i64> = self.tracks_of(Dragged::Selection).into_iter().map(|t| self.library.tracks[t].id).collect();
        if ids != self.tag_tracks || self.tag_values.is_empty() {
            self.show_tags(ui, ids);
        }
    }

    fn show_tags(&mut self, ui: &AppWindow, ids: Vec<i64>) {
        let details = self.db.track_details(&ids).unwrap_or_else(|error| {
            eprintln!("tags: {error}");
            Vec::new()
        });
        let mut values = HashMap::new();
        let mut put = |key: &str, all: Vec<String>| {
            let multiple = all.windows(2).any(|pair| pair[0] != pair[1]);
            let value = if multiple { String::new() } else { all.into_iter().next().unwrap_or_default() };
            values.insert(key.to_owned(), (value, multiple));
        };
        for field in Field::ALL {
            put(field.key(), details.iter().map(|d| field_value(d, field)).collect());
        }
        let number = |value: Option<u32>| value.map_or(String::new(), |n| n.to_string());
        put("compilation", details.iter().map(|d| u8::from(d.compilation).to_string()).collect());
        put("codec", details.iter().map(|d| d.codec.clone().unwrap_or_default()).collect());
        put("bitrate", details.iter().map(|d| number(d.bitrate)).collect());
        put("sample-rate", details.iter().map(|d| number(d.sample_rate)).collect());
        put("bit-depth", details.iter().map(|d| number(d.bit_depth)).collect());
        put("channels", details.iter().map(|d| number(d.channels)).collect());
        // Durée et taille : le total de la sélection.
        let seconds: u64 = details.iter().map(|d| d.duration_ms / 1000).sum();
        put("duration", vec![if details.is_empty() { String::new() } else { seconds.to_string() }]);
        let size = details.iter().map(|d| d.file_size).sum::<Option<u64>>();
        put("file-size", vec![size.map_or(String::new(), |s| s.to_string())]);
        put("path", details.iter().map(|d| d.path.clone()).collect());

        self.tag_tracks = details.iter().map(|d| d.id).collect();
        self.tag_values = values;
        self.tag_edits.clear();
        ui.set_tag_count(self.tag_tracks.len() as i32);
        self.refresh_tag_rows(ui);
    }

    // Lignes du tiroir : champs visibles dans l'ordre de la v0.1, puis la section technique.
    fn refresh_tag_rows(&self, ui: &AppWindow) {
        let empty = (String::new(), false);
        let value = |key: &str| self.tag_values.get(key).unwrap_or(&empty);
        let mut rows = Vec::new();
        for &key in TAG_FIELDS.iter().filter(|&&key| self.tag_fields.iter().any(|f| f == key)) {
            let (text, multiple) = value(key);
            let pair_key = match key {
                "track-number" => "track-total",
                "disc-number" => "disc-total",
                _ => "",
            };
            let (pair_value, pair_multiple) = if pair_key.is_empty() { &empty } else { value(pair_key) };
            rows.push(TagRow {
                key: key.into(),
                value: text.into(),
                multiple: *multiple,
                editable: Field::from_key(key).is_some(),
                pair_key: pair_key.into(),
                pair_value: pair_value.into(),
                pair_multiple: *pair_multiple,
            });
        }
        if self.tag_technical {
            for &key in TECHNICAL_FIELDS {
                let (text, multiple) = value(key);
                rows.push(TagRow { key: key.into(), value: text.into(), multiple: *multiple, ..Default::default() });
            }
        }
        ui.set_tag_rows(model(rows));
        let mut choices: Vec<TagChoice> =
            TAG_FIELDS.iter().map(|&key| TagChoice { key: key.into(), shown: self.tag_fields.iter().any(|f| f == key) }).collect();
        choices.push(TagChoice { key: "technical".into(), shown: self.tag_technical });
        ui.set_tag_choices(model(choices));
    }

    fn toggle_tag_field(&mut self, ui: &AppWindow, key: &str) {
        if key == "technical" {
            self.tag_technical = !self.tag_technical;
        } else if let Some(at) = self.tag_fields.iter().position(|f| f == key) {
            self.tag_fields.remove(at);
        } else if TAG_FIELDS.contains(&key) {
            self.tag_fields.push(key.to_owned());
        }
        self.refresh_tag_rows(ui);
    }

    fn edit_tags(&mut self, ui: &AppWindow) {
        if self.scan.is_none() && !self.tag_tracks.is_empty() {
            self.tag_edits.clear();
            ui.set_tags_editing(true);
        }
    }

    fn cancel_tags(&mut self, ui: &AppWindow) {
        ui.set_tags_editing(false);
        // Les champs reprennent leurs valeurs d'origine.
        self.tag_values.clear();
        self.refresh_tags(ui);
    }

    fn save_tags(&mut self, ui: &AppWindow) {
        if self.scan.is_some() {
            return;
        }
        let mut edits = Vec::new();
        for field in Field::ALL {
            let (Some(value), Some((original, multiple))) = (self.tag_edits.get(field.key()), self.tag_values.get(field.key())) else {
                continue;
            };
            let value = value.trim();
            // Valeurs multiples : un champ laissé vide garde la valeur de chaque morceau.
            if (*multiple && value.is_empty()) || (!*multiple && value == original.trim()) {
                continue;
            }
            if field.is_numeric() && !value.is_empty() && value.parse::<u16>().is_err() {
                return self.show_status(ui, Status { kind: StatusKind::TagsInvalid, track: value.into(), ..Default::default() });
            }
            edits.push((field, (!value.is_empty()).then(|| value.to_owned())));
        }
        if edits.is_empty() {
            return self.cancel_tags(ui);
        }
        let paths = self.tag_tracks.iter().filter_map(|&id| self.library.track_by_id(id)).map(|t| PathBuf::from(&self.library.tracks[t].path)).collect();
        ui.set_tags_saving(true);
        self.write_files(ui, paths, edits, None);
    }

    // Écrit tags ou pochette dans les fichiers en arrière-plan, puis les relit dans la base.
    fn write_files(&mut self, ui: &AppWindow, mut paths: Vec<PathBuf>, edits: Vec<audioplayer_scanner::write::Edit>, cover: Option<(String, Vec<u8>)>) {
        let Some(index) = self.selected_library.filter(|_| self.scan.is_none() && !paths.is_empty()) else {
            ui.set_tags_saving(false);
            return;
        };
        self.tag_resume = self.suspend_playback(&mut paths);
        let library = self.libraries[index].clone();
        let (sender, messages) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (db_path, stop) = (self.db_path.clone(), cancel.clone());
        let artwork = cover.is_some();
        std::thread::spawn(move || {
            let mut failed = Vec::new();
            let total = paths.len();
            for (done, path) in paths.iter().enumerate() {
                let _ = sender.send(ScanMessage::Writing { done, total });
                let cover = cover.as_ref().map(|(mime, data)| (mime.as_str(), data.as_slice()));
                if let Err(error) = audioplayer_scanner::write::write_tags(path, &edits, cover) {
                    failed.push((path.clone(), error));
                }
            }
            let progress = sender.clone();
            let result = audioplayer_scanner::rescan_files(&db_path, library.id, &paths, &stop, &move |p| {
                let _ = progress.send(ScanMessage::Progress(p));
            });
            let _ = sender.send(ScanMessage::Written { written: total - failed.len(), failed, artwork, result });
        });
        self.scan = Some(ScanJob { library: library.id, name: library.name.clone(), messages, cancel });
        ui.set_scanning(true);
        ui.set_status(Status { kind: StatusKind::TagsWriting, total: 0, ..Default::default() });
    }

    // Un fichier ouvert par le moteur (en cours ou déjà en file) ne doit pas être réécrit
    // sous ses pieds : la lecture s'arrête et le morceau en cours passe en dernier.
    fn suspend_playback(&mut self, paths: &mut Vec<PathBuf>) -> Option<(Duration, bool)> {
        if self.stopped {
            return None;
        }
        let now = self.now.as_ref()?;
        let lib = self.library_at(now.library)?;
        let current = PathBuf::from(&lib.tracks[now.track].path);
        let next = now.next.map(|n| PathBuf::from(&lib.tracks[n].path));
        if !paths.iter().any(|p| *p == current || Some(p) == next.as_ref()) {
            return None;
        }
        if let Some(at) = paths.iter().position(|p| *p == current) {
            let path = paths.remove(at);
            paths.push(path);
        }
        let resume = (self.engine.position(), self.is_playing);
        self.engine.stop();
        Some(resume)
    }

    fn resume_playback(&mut self) {
        let Some((position, playing)) = self.tag_resume.take() else { return };
        let Some(now) = &self.now else { return };
        let Some(lib) = self.library_at(now.library) else { return };
        let path = PathBuf::from(&lib.tracks[now.track].path);
        let next = now.next.map(|n| PathBuf::from(&lib.tracks[n].path));
        self.engine.play(&path, next.as_deref());
        self.engine.seek(position);
        if !playing {
            self.engine.pause();
        }
    }

    fn files_written(
        &mut self,
        ui: &AppWindow,
        library: i64,
        written: usize,
        failed: Vec<(PathBuf, String)>,
        artwork: bool,
        result: Result<audioplayer_scanner::Report, audioplayer_scanner::Error>,
    ) {
        if let Err(error) = &result {
            eprintln!("rescan after writing: {error}");
        }
        // Le rechargement perd la sélection (index) : on la retrouve par id.
        let selected: Vec<i64> = self.tracks_of(Dragged::Selection).into_iter().map(|t| self.library.tracks[t].id).collect();
        self.reload_library(ui, library);
        self.resume_playback();
        let selection: std::collections::HashSet<usize> =
            selected.iter().filter_map(|&id| self.library.track_by_id(id)).filter(|t| self.visible.contains(t)).collect();
        let current = selection.iter().copied().min_by_key(|t| self.visible.iter().position(|v| v == t));
        ui.set_tags_saving(false);
        ui.set_tags_editing(false);
        self.set_selection(ui, selection, current);
        if !artwork {
            // Le tiroir garde les morceaux édités, même s'ils ont quitté la vue.
            let ids = self.tag_tracks.clone();
            self.show_tags(ui, ids);
        }
        let status = match failed.first() {
            None => Status { kind: if artwork { StatusKind::ArtworkSaved } else { StatusKind::TagsSaved }, count: written as i32, ..Default::default() },
            Some((path, error)) => Status {
                kind: StatusKind::TagsFailed,
                track: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into(),
                playlist: error.as_str().into(),
                count: failed.len() as i32,
                ..Default::default()
            },
        };
        self.show_status_for(ui, status, Duration::from_secs(if failed.is_empty() { 5 } else { 15 }));
    }

    // « Added last n unités » ; SQLite ne connaît pas les semaines.
    fn set_recent(&mut self, ui: &AppWindow, amount: u32, unit: &str) {
        let amount = amount.clamp(1, 99);
        let unit = match unit {
            "days" | "months" | "years" => unit,
            _ => "weeks",
        };
        let modifier = if unit == "weeks" { format!("-{} days", amount * 7) } else { format!("-{amount} {unit}") };
        match self.db.datetime_from_now(&modifier) {
            Ok(cutoff) => self.recent_cutoff = cutoff,
            Err(error) => eprintln!("recent: {error}"),
        }
        self.recent = (amount, unit.to_owned());
        ui.set_recent_amount(amount as i32);
        ui.set_recent_unit(unit.into());
        if self.view == View::Recent {
            self.update_scope();
            self.refresh_navigation(ui);
        }
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

fn payload(prefix: &str, id: i32) -> DataTransfer {
    DataTransfer::from(SharedString::from(format!("{prefix}{id}")))
}

fn dragged(data: &DataTransfer) -> Option<Dragged> {
    let text = data.plain_text().ok()?;
    let parse = |prefix: &str| text.strip_prefix(prefix).and_then(|id| id.parse().ok());
    if text == SELECTION_PAYLOAD {
        return Some(Dragged::Selection);
    }
    parse(TRACK_PAYLOAD_PREFIX)
        .map(Dragged::Track)
        .or_else(|| parse(ARTIST_PAYLOAD_PREFIX).map(Dragged::Artist))
        .or_else(|| parse(ALBUM_PAYLOAD_PREFIX).map(Dragged::Album))
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

// Champs du tiroir, dans l'ordre de la v0.1 ; les totaux s'affichent avec leur numéro.
const TAG_FIELDS: &[&str] = &[
    "title", "artist", "album-artist", "album", "track-number", "disc-number", "year", "genre", "grouping", "composer",
    "conductor", "lyricist", "comment", "publisher", "catalog-number", "disc-subtitle", "barcode", "work", "compilation",
];
const TECHNICAL_FIELDS: &[&str] = &["codec", "bitrate", "sample-rate", "bit-depth", "channels", "duration", "file-size", "path"];

// Valeur d'un champ modifiable, telle que la base la connaît.
fn field_value(d: &TrackDetails, field: Field) -> String {
    let text = |value: &Option<String>| value.clone().unwrap_or_default();
    let number = |value: Option<u32>| value.map_or(String::new(), |n| n.to_string());
    match field {
        Field::Title => d.title.clone(),
        Field::Artist => d.artist.clone(),
        Field::AlbumArtist => d.album_artist.clone(),
        Field::Album => d.album.clone(),
        Field::TrackNumber => number(d.track_number),
        Field::DiscNumber => number(d.disc_number),
        Field::TrackTotal => number(d.track_total),
        Field::DiscTotal => number(d.disc_total),
        Field::Year => d.year.map_or(String::new(), |y| y.to_string()),
        Field::Genre => text(&d.genre),
        Field::Composer => text(&d.composer),
        Field::Conductor => text(&d.conductor),
        Field::Lyricist => text(&d.lyricist),
        Field::Grouping => text(&d.grouping),
        Field::Work => text(&d.work),
        Field::Comment => text(&d.comment),
        Field::Publisher => text(&d.publisher),
        Field::CatalogNumber => text(&d.catalog_number),
        Field::Barcode => text(&d.barcode),
        Field::DiscSubtitle => text(&d.disc_subtitle),
    }
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
    ui.set_tags_width(settings.panels.tags_width.clamp(220.0, 600.0));
    ui.set_tags_open(settings.panels.tags_visible);
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
            tags_width: ui.get_tags_width(),
            tags_visible: ui.get_tags_open(),
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
        recent: settings::RecentSettings { amount: app.recent.0, unit: app.recent.1.clone() },
        tags: settings::TagsSettings { fields: app.tag_fields.clone(), technical: app.tag_technical },
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
    let mut app = app;
    app.tag_fields = initial.tags.fields.clone();
    app.tag_technical = initial.tags.technical;

    let ui = AppWindow::new().map_err(|e| Problem::failed(path, e))?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    apply_settings(&ui, &initial);
    let ctx = Context { app: Rc::new(RefCell::new(app)), ui: ui.as_weak() };
    ctx.update(|app, ui| app.set_recent(ui, initial.recent.amount, &initial.recent.unit));
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
    ui.on_move_selection(move |delta, extend| c.update(|app, ui| app.move_selection(ui, delta as isize, extend)));
    let c = ctx.clone();
    ui.on_track_clicked(move |id, toggle, extend| {
        c.update(|app, ui| {
            if let Some(track) = from_ui_id(id).filter(|&t| t < app.library.tracks.len()) {
                app.track_clicked(ui, track, toggle, extend);
            }
        })
    });
    let c = ctx.clone();
    ui.on_cycle_panel(move |direction| c.update(|app, ui| app.cycle_panel(ui, direction)));
    let c = ctx.clone();
    ui.on_activate(move || c.update(|app, ui| app.activate(ui)));
    let c = ctx.clone();
    ui.on_track_action(move |id, action| c.update(|app, ui| app.track_action(ui, id, &action)));
    let c = ctx.clone();
    ui.on_drop_on_playlist(move |playlist, data| {
        c.update(|app, ui| {
            if let Some(item) = dragged(&data).filter(|_| playlist >= 0) {
                app.add_dragged(ui, playlist as i64, item);
            }
        })
    });
    let w = ui.as_weak();
    ui.on_select_language(move |language| {
        if let Some(ui) = w.upgrade() {
            select_language(&ui, &language);
        }
    });
    let c = ctx.clone();
    ui.on_track_payload(move |id| {
        let app = c.app.borrow();
        let in_selection = from_ui_id(id).is_some_and(|t| app.selection.len() > 1 && app.selection.contains(&t));
        if in_selection { DataTransfer::from(SharedString::from(SELECTION_PAYLOAD)) } else { payload(TRACK_PAYLOAD_PREFIX, id) }
    });
    ui.on_artist_payload(|id| payload(ARTIST_PAYLOAD_PREFIX, id));
    ui.on_album_payload(|id| payload(ALBUM_PAYLOAD_PREFIX, id));
    ui.on_is_track_payload(|data| dragged(&data).is_some());
    let c = ctx.clone();
    ui.on_playlist_reorder(move |data, row| {
        c.update(|app, ui| {
            if let (Some(item), Ok(row)) = (dragged(&data), usize::try_from(row)) {
                app.place_in_playlist(ui, item, row);
            }
        })
    });
    let c = ctx.clone();
    ui.on_artist_action(move |id, action| {
        c.update(|app, ui| {
            if let Some(artist) = from_ui_id(id).filter(|&a| a < app.library.artists.len()) {
                app.item_action(ui, Dragged::Artist(artist), &action);
            }
        })
    });
    let c = ctx.clone();
    ui.on_album_action(move |id, action| {
        c.update(|app, ui| {
            if let Some(album) = from_ui_id(id).filter(|&a| a < app.library.albums.len()) {
                app.item_action(ui, Dragged::Album(album), &action);
            }
        })
    });

    ui.set_database_path(path.display().to_string().into());
    let c = ctx.clone();
    ui.on_library_menu(move |action| match action.as_str() {
        "new-database" => database_action(&c, choose_new_database),
        "open-database" => database_action(&c, choose_existing_database),
        action => c.update(|app, ui| app.library_menu(ui, action)),
    });
    let c = ctx.clone();
    ui.on_stop_scan(move || c.update(|app, _| app.stop_scan()));
    let c = ctx.clone();
    ui.on_tags_toggled(move || {
        c.update(|app, ui| {
            if ui.get_tags_open() {
                app.refresh_tags(ui);
            } else if !ui.get_tags_saving() {
                // Fermé en pleine édition : les saisies sont abandonnées.
                ui.set_tags_editing(false);
            }
        })
    });
    let c = ctx.clone();
    ui.on_tags_edit(move || c.update(|app, ui| app.edit_tags(ui)));
    let c = ctx.clone();
    ui.on_tags_save(move || c.update(|app, ui| app.save_tags(ui)));
    let c = ctx.clone();
    ui.on_tags_cancel(move || c.update(|app, ui| app.cancel_tags(ui)));
    let c = ctx.clone();
    ui.on_tag_edited(move |key, value| {
        c.update(|app, _| {
            app.tag_edits.insert(key.into(), value.into());
        })
    });
    let c = ctx.clone();
    ui.on_tag_field_toggled(move |key| c.update(|app, ui| app.toggle_tag_field(ui, &key)));
    let c = ctx.clone();
    ui.on_playlist_menu(move |action| c.update(|app, ui| app.playlist_menu(ui, &action)));
    let c = ctx.clone();
    ui.on_dialog_accepted(move |name| c.update(|app, ui| app.name_accepted(ui, &name)));
    let c = ctx.clone();
    ui.on_confirm_accepted(move || c.update(|app, ui| app.confirmed(ui)));
    let c = ctx.clone();
    ui.on_recent_changed(move |amount, unit| c.update(|app, ui| app.set_recent(ui, amount.max(1) as u32, &unit)));
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
