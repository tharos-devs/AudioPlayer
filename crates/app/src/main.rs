// Prototype : la bibliothèque vient de la base SQLite (importée de la v0.1), mais
// il n'y a pas encore d'audio : la lecture est simulée par un minuteur.

mod covers;
mod library;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use audioplayer_db::Database;
use library::{format_duration, sort_key, Library};
use slint::{DataTransfer, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

const TRACK_PAYLOAD_PREFIX: &str = "audioplayer/track:";
// Vignettes de 80px affichées jusqu'à 80px : 160px suffisent en Retina.
const COVER_SIZE: u32 = 160;

// Pas encore enregistrées en base : elles retiennent des ids de morceaux.
struct Playlist {
    name: String,
    tracks: Vec<i64>,
}

struct App {
    db: Database,
    libraries: Vec<audioplayer_db::Library>,
    selected_library: Option<usize>,
    library: Library,
    // Décodées à la première demande.
    covers: RefCell<HashMap<usize, Option<Image>>>,
    artist_order: Vec<usize>,
    artist_album_counts: Vec<usize>,
    album_track_counts: Vec<usize>,
    selected_artist: Option<usize>,
    selected_album: Option<usize>,
    selected_playlist: Option<usize>,
    query: String,
    playlists: Vec<Playlist>,
    // Morceaux affichés, dans l'ordre : sert aussi à précédent/suivant.
    visible: Vec<usize>,
    playing: Option<usize>,
    position: i32,
    is_playing: bool,
}

impl App {
    fn new(db: Database) -> audioplayer_db::Result<Self> {
        let libraries = db.libraries()?;
        let playlists = ["Favorites", "Road Trip", "Focus", "Late Night"]
            .into_iter()
            .map(|name| Playlist { name: name.to_owned(), tracks: Vec::new() })
            .collect();

        let mut app = App {
            db,
            libraries,
            selected_library: None,
            library: Library::default(),
            covers: RefCell::default(),
            artist_order: Vec::new(),
            artist_album_counts: Vec::new(),
            album_track_counts: Vec::new(),
            selected_artist: None,
            selected_album: None,
            selected_playlist: None,
            query: String::new(),
            playlists,
            visible: Vec::new(),
            playing: None,
            position: 0,
            is_playing: false,
        };
        if !app.libraries.is_empty() {
            app.load_library(0)?;
        }
        Ok(app)
    }

    // Remet à zéro la navigation : les index de l'ancienne bibliothèque ne valent plus rien.
    fn load_library(&mut self, index: usize) -> audioplayer_db::Result<()> {
        let library = Library::load(&self.db, self.libraries[index].id)?;

        let mut artist_order: Vec<usize> = (0..library.artists.len()).collect();
        artist_order.sort_by(|&a, &b| library.artists[a].sort_key.cmp(&library.artists[b].sort_key));
        let mut artist_album_counts = vec![0; library.artists.len()];
        let mut album_track_counts = vec![0; library.albums.len()];
        for album in &library.albums {
            artist_album_counts[album.artist] += 1;
        }
        for track in &library.tracks {
            album_track_counts[track.album] += 1;
        }

        self.selected_library = Some(index);
        self.library = library;
        self.covers.borrow_mut().clear();
        self.artist_order = artist_order;
        self.artist_album_counts = artist_album_counts;
        self.album_track_counts = album_track_counts;
        self.selected_artist = None;
        self.selected_album = None;
        self.visible.clear();
        self.playing = None;
        self.position = 0;
        self.is_playing = false;
        Ok(())
    }

    fn select_library(&mut self, ui: &AppWindow, id: i32) {
        let Some(index) = from_ui_id(id).filter(|&i| i < self.libraries.len()) else { return };
        if self.selected_library == Some(index) {
            return;
        }
        if let Err(error) = self.load_library(index) {
            eprintln!("library {}: {error}", self.libraries[index].name);
            return;
        }
        self.selected_playlist = None;
        self.clear_search(ui);
        ui.set_selected_track(-1);
        self.refresh_all(ui);
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

    fn refresh_all(&mut self, ui: &AppWindow) {
        ui.set_library_count(self.library.tracks.len() as i32);
        self.refresh_libraries(ui);
        self.refresh_artists(ui);
        self.refresh_albums(ui);
        self.refresh_playlists(ui);
        self.refresh_tracks(ui);
        self.refresh_player(ui);
    }

    fn cover(&self, album: usize) -> (Image, bool) {
        if !self.library.albums[album].has_artwork {
            return (Image::default(), false);
        }
        let mut covers = self.covers.borrow_mut();
        let cover = covers.entry(album).or_insert_with(|| {
            let artwork = self.db.artwork(self.library.albums[album].id, COVER_SIZE);
            artwork.ok().flatten().and_then(|artwork| covers::decode(&artwork.data))
        });
        match cover {
            Some(image) => (image.clone(), true),
            None => (Image::default(), false),
        }
    }

    fn refresh_artists(&self, ui: &AppWindow) {
        let rows = self.artist_order.iter().map(|&artist| ListItem {
            id: artist as i32,
            label: self.library.artists[artist].name.as_str().into(),
            count: self.artist_album_counts[artist] as i32,
            ..Default::default()
        });
        ui.set_artists(model(rows.collect()));
        ui.set_artists_count(self.library.artists.len() as i32);
        ui.set_selected_artist(to_ui_id(self.selected_artist));
    }

    fn refresh_albums(&self, ui: &AppWindow) {
        let albums = &self.library.albums;
        let mut ids: Vec<usize> = match self.selected_artist {
            Some(artist) => (0..albums.len()).filter(|&a| albums[a].artist == artist).collect(),
            None => (0..albums.len()).collect(),
        };
        // Comme la v0.1 : albums par titre, même pour un seul artiste.
        ids.sort_by(|&a, &b| albums[a].sort_key.cmp(&albums[b].sort_key).then(a.cmp(&b)));

        ui.set_albums_count(ids.len() as i32);
        let rows = ids.into_iter().map(|album| {
            let (artwork, has_artwork) = self.cover(album);
            ListItem {
                id: album as i32,
                label: albums[album].title.as_str().into(),
                count: self.album_track_counts[album] as i32,
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
        let query = sort_key(self.query.trim());

        let mut ids: Vec<usize> = match self.selected_playlist {
            Some(playlist) => self.playlists[playlist].tracks.iter().filter_map(|&id| self.library.track_by_id(id)).collect(),
            None if !query.is_empty() => (0..tracks.len()).collect(),
            None => (0..tracks.len())
                .filter(|&t| self.selected_artist.is_none_or(|artist| self.library.albums[tracks[t].album].artist == artist))
                .filter(|&t| self.selected_album.is_none_or(|album| tracks[t].album == album))
                .collect(),
        };
        if !query.is_empty() {
            ids.retain(|&t| self.matches(t, &query));
        }

        // Une playlist garde l'ordre d'ajout ; la bibliothèque est groupée par album.
        if self.selected_playlist.is_some() {
            let items = ids.iter().enumerate().map(|(position, &t)| self.track_item(t, position as i32 + 1, false));
            ui.set_playlist_items(model(items.collect()));
        } else {
            ids.sort_by(|&a, &b| self.compare(a, b));
            ui.set_track_items(model(self.grouped_items(&ids)));
        }

        let total: i32 = ids.iter().map(|&t| tracks[t].duration).sum();
        ui.set_tracks_count(ids.len() as i32);
        ui.set_tracks_duration(total);
        ui.set_status(Status { kind: StatusKind::Summary, ..Default::default() });
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
            track_count: self.album_track_counts[album] as i32,
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

    fn refresh_player(&self, ui: &AppWindow) {
        let track = self.playing.map(|t| &self.library.tracks[t]);
        ui.set_playing_track(to_ui_id(self.playing));
        ui.set_playing(self.is_playing);
        ui.set_now_title(track.map_or("", |t| &t.title).into());
        ui.set_now_artist(
            track
                .map(|t| {
                    format!("{} — {}", self.library.artists[t.artist].name, self.library.albums[t.album].title)
                })
                .unwrap_or_default()
                .into(),
        );
        let (artwork, has_artwork) = track.map_or((Image::default(), false), |t| self.cover(t.album));
        ui.set_now_artwork(artwork);
        ui.set_now_has_artwork(has_artwork);
        let duration = track.map_or(0, |t| t.duration);
        ui.set_position(self.position);
        ui.set_duration(duration);
        ui.set_position_text(format_duration(self.position).into());
        ui.set_duration_text(format_duration(duration).into());
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

    fn clear_search(&mut self, ui: &AppWindow) {
        self.query.clear();
        ui.set_search_text(SharedString::new());
    }

    // Un second clic sur l'artiste sélectionné le désélectionne.
    fn select_artist(&mut self, ui: &AppWindow, id: i32) {
        let artist = from_ui_id(id);
        self.selected_artist = if self.selected_artist == artist { None } else { artist };
        self.selected_album = None;
        self.clear_search(ui);
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
        self.clear_search(ui);
        ui.set_selected_album(-1);
        self.refresh_tracks(ui);
    }

    fn reveal_album(&mut self, ui: &AppWindow, album: usize) {
        let artist = self.library.albums[album].artist;
        let artist_changed = self.selected_artist.replace(artist) != Some(artist);
        self.selected_album = Some(album);
        self.clear_search(ui);
        if self.selected_playlist.take().is_some() {
            self.refresh_playlists(ui);
        }
        if artist_changed {
            self.refresh_albums(ui);
        }
        ui.set_selected_artist(artist as i32);
        ui.set_selected_album(album as i32);
        self.refresh_tracks(ui);
    }

    fn select_playlist(&mut self, ui: &AppWindow, id: i32) {
        self.selected_playlist = from_ui_id(id);
        self.refresh_playlists(ui);
        self.refresh_tracks(ui);
    }

    fn search(&mut self, ui: &AppWindow, query: &str) {
        self.query = query.to_owned();
        self.refresh_tracks(ui);
    }

    fn play(&mut self, ui: &AppWindow, track: usize) {
        self.playing = Some(track);
        self.position = 0;
        self.is_playing = true;
        ui.set_selected_track(track as i32);
        self.refresh_player(ui);
    }

    fn show_current(&mut self, ui: &AppWindow) {
        if let Some(track) = self.playing {
            self.reveal_album(ui, self.library.tracks[track].album);
            ui.set_selected_track(track as i32);
        }
    }

    fn toggle_play(&mut self, ui: &AppWindow) {
        match self.playing {
            Some(_) => self.is_playing = !self.is_playing,
            None => {
                if let Some(&first) = self.visible.first() {
                    return self.play(ui, first);
                }
            }
        }
        self.refresh_player(ui);
    }

    fn step(&mut self, ui: &AppWindow, delta: isize) {
        let current = self.playing.and_then(|t| self.visible.iter().position(|&v| v == t));
        let next = match current {
            Some(index) => index.checked_add_signed(delta).filter(|&i| i < self.visible.len()),
            None => (!self.visible.is_empty()).then_some(0),
        };
        if let Some(index) = next {
            self.play(ui, self.visible[index]);
        }
    }

    fn seek(&mut self, ui: &AppWindow, ratio: f32) {
        if let Some(track) = self.playing {
            let duration = self.library.tracks[track].duration;
            self.position = (ratio.clamp(0.0, 1.0) * duration as f32) as i32;
            self.refresh_player(ui);
        }
    }

    fn tick(&mut self, ui: &AppWindow) {
        let Some(track) = self.playing.filter(|_| self.is_playing) else { return };
        self.position += 1;
        if self.position >= self.library.tracks[track].duration {
            self.step(ui, 1);
        } else {
            self.refresh_player(ui);
        }
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
            "play-next" => ui.set_status(status(StatusKind::PlayNext)),
            "enqueue" => ui.set_status(status(StatusKind::Enqueued)),
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
        };
        self.refresh_playlists(ui);
        if self.selected_playlist == Some(playlist) {
            self.refresh_tracks(ui);
        }
        ui.set_status(status);
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

// `AUDIOPLAYER_DB` permet de pointer une autre base, par exemple pendant les essais.
fn database_path() -> PathBuf {
    if let Some(path) = std::env::var_os("AUDIOPLAYER_DB") {
        return path.into();
    }
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("AudioPlayer");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("library.db")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    // Slint suit la langue du système par défaut : l'interface démarre en anglais.
    select_language(&ui, "en");

    let path = database_path();
    let app = App::new(Database::open(&path).map_err(|e| format!("{}: {e}", path.display()))?)?;
    let ctx = Context { app: Rc::new(RefCell::new(app)), ui: ui.as_weak() };
    ctx.update(|app, ui| app.refresh_all(ui));

    let c = ctx.clone();
    ui.on_select_library(move |id| c.update(|app, ui| app.select_library(ui, id)));
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
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_secs(1), move || c.update(|app, ui| app.tick(ui)));

    ui.run()?;
    Ok(())
}
