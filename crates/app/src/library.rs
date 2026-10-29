//! Bibliothèque factice générée de façon déterministe, pour éprouver l'interface
//! sans scanner ni base de données. Les ids sont les index dans les vecteurs.

pub struct Artist {
    pub name: String,
    pub sort_key: String,
}

pub struct Album {
    pub artist: usize,
    pub title: String,
    pub sort_key: String,
    pub year: i32,
    pub total_discs: i32,
    // Index dans le jeu de jaquettes factices ; `None` = album sans pochette.
    pub cover: Option<usize>,
}

pub struct Track {
    pub album: usize,
    pub artist: usize,
    pub disc: i32,
    pub number: i32,
    pub title: String,
    pub sort_key: String,
    pub duration: i32,
}

pub struct Library {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
}

const FIRST_NAMES: &[&str] = &[
    "Ana", "Bruno", "Clara", "David", "Elsa", "Félix", "Gaëlle", "Hugo", "Inès", "Jules",
    "Karim", "Léa", "Marius", "Nina", "Oscar", "Paula", "Quentin", "Rosa", "Sacha", "Théo",
];
const LAST_NAMES: &[&str] = &[
    "Moreau", "Laurent", "Garcia", "Fontaine", "Rossi", "Nakamura", "Okafor", "Lindqvist",
    "Dubois", "Kowalski", "Silva", "Martel", "Brennan", "Haddad", "Vidal", "Novak",
];
const BAND_WORDS: &[&str] = &[
    "Echoes", "Satellites", "Wolves", "Mirrors", "Tides", "Embers", "Comets", "Lanterns",
    "Pilots", "Shadows", "Rivers", "Glaciers", "Owls", "Vandals", "Prophets", "Machines",
];
const ADJECTIVES: &[&str] = &[
    "Silent", "Electric", "Golden", "Broken", "Midnight", "Endless", "Crimson", "Hidden",
    "Northern", "Velvet", "Lost", "Bright", "Distant", "Wild", "Hollow", "Neon",
];
const NOUNS: &[&str] = &[
    "Light", "Dreams", "Roads", "Summer", "Hearts", "City", "Ocean", "Signals", "Garden",
    "Fire", "Horizon", "Rain", "Stars", "Machine", "Waves", "Mountain", "Mirror", "Night",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[(self.next() % words.len() as u64) as usize]
    }
}

impl Library {
    pub fn generate(artist_count: usize, cover_count: usize) -> Self {
        let mut rng = Rng(0x5eed);
        let mut library = Library { artists: Vec::new(), albums: Vec::new(), tracks: Vec::new() };

        for artist in 0..artist_count {
            let name = if rng.range(0, 2) == 0 {
                format!("The {} {}", rng.pick(ADJECTIVES), rng.pick(BAND_WORDS))
            } else {
                format!("{} {}", rng.pick(FIRST_NAMES), rng.pick(LAST_NAMES))
            };
            library.artists.push(Artist { sort_key: sort_key(&name), name });

            let mut year = rng.range(1965, 2018) as i32;
            for _ in 0..rng.range(3, 7) {
                let album = library.albums.len();
                let title = format!("{} {}", rng.pick(ADJECTIVES), rng.pick(NOUNS));
                let track_count = rng.range(4, 12) as i32;
                let total_discs = if track_count >= 10 { 2 } else { 1 };
                let cover = (album % 11 != 0).then(|| rng.next() as usize % cover_count);
                library.albums.push(Album { artist, sort_key: sort_key(&title), title, year, total_discs, cover });

                let per_disc = (track_count + total_discs - 1) / total_discs;
                for index in 0..track_count {
                    let (disc, number) = (index / per_disc + 1, index % per_disc + 1);
                    let title = match rng.range(0, 3) {
                        0 => format!("{} {}", rng.pick(ADJECTIVES), rng.pick(NOUNS)),
                        1 => format!("{} of {}", rng.pick(NOUNS), rng.pick(NOUNS)),
                        2 => format!("The {}", rng.pick(NOUNS)),
                        _ => format!("{} ({} Mix)", rng.pick(NOUNS), rng.pick(ADJECTIVES)),
                    };
                    library.tracks.push(Track {
                        album,
                        artist,
                        disc,
                        number,
                        sort_key: sort_key(&title),
                        title,
                        duration: rng.range(95, 420) as i32,
                    });
                }
                year += rng.range(1, 4) as i32;
            }
        }
        library
    }
}

pub fn sort_key(text: &str) -> String {
    text.to_lowercase()
}

pub fn format_duration(seconds: i32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
