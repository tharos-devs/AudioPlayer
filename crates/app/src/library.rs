//! Catalogue d'une bibliothèque chargé en mémoire depuis la base. Dans l'app, les
//! artistes, albums et morceaux sont désignés par leur index dans ces vecteurs.

use std::collections::HashMap;

use audioplayer_db::Database;

pub struct Artist {
    pub name: String,
    pub sort_key: String,
}

pub struct Album {
    pub id: i64,
    pub artist: usize,
    pub title: String,
    pub sort_key: String,
    pub year: i32,
    pub total_discs: i32,
    pub has_artwork: bool,
}

pub struct Track {
    pub id: i64,
    pub album: usize,
    pub artist: usize,
    pub disc: i32,
    pub number: i32,
    pub title: String,
    pub sort_key: String,
    pub duration: i32,
}

#[derive(Default)]
pub struct Library {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    track_index: HashMap<i64, usize>,
}

impl Library {
    pub fn load(db: &Database, library_id: i64) -> audioplayer_db::Result<Self> {
        let mut library = Library::default();

        let mut artist_index = HashMap::new();
        for artist in db.artists(library_id)? {
            artist_index.insert(artist.id, library.artists.len());
            library.artists.push(Artist { sort_key: sort_key(&artist.name), name: artist.name });
        }

        let mut album_index = HashMap::new();
        for album in db.albums(library_id)? {
            let Some(&artist) = artist_index.get(&album.artist_id) else { continue };
            album_index.insert(album.id, library.albums.len());
            library.albums.push(Album {
                id: album.id,
                artist,
                sort_key: sort_key(&album.title),
                title: album.title,
                year: album.year.unwrap_or(0),
                total_discs: album.disc_count as i32,
                has_artwork: album.has_artwork,
            });
        }

        for track in db.tracks(library_id)? {
            let (Some(&album), Some(&artist)) = (album_index.get(&track.album_id), artist_index.get(&track.artist_id))
            else {
                continue;
            };
            library.track_index.insert(track.id, library.tracks.len());
            library.tracks.push(Track {
                id: track.id,
                album,
                artist,
                disc: track.disc_number.unwrap_or(1) as i32,
                number: track.track_number.unwrap_or(0) as i32,
                sort_key: sort_key(&track.title),
                title: track.title,
                duration: ((track.duration_ms + 500) / 1000) as i32,
            });
        }
        Ok(library)
    }

    pub fn track_by_id(&self, id: i64) -> Option<usize> {
        self.track_index.get(&id).copied()
    }
}

pub fn sort_key(text: &str) -> String {
    text.to_lowercase()
}

pub fn format_duration(seconds: i32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
