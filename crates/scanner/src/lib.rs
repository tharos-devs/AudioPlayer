//! Scan d'une bibliothèque : fichiers nouveaux ou modifiés relus en parallèle, écrits
//! par lots, fichiers disparus retirés. Les règles de tags et de regroupement sont
//! celles de la v0.1, avec ses correctifs (voir `tags`).

pub mod artwork;
pub mod files;
pub mod tags;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};

use audioplayer_db::{Database, TrackRecord};

const BATCH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Seulement les fichiers nouveaux, modifiés (taille ou date) ou disparus.
    Changes,
    /// Relire tous les fichiers.
    Full,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Listing { folders: usize },
    Reading { done: usize, total: usize, folder: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Tags illisibles : enregistrés avec le nom du fichier et du dossier.
    pub unreadable: usize,
    pub artworks: usize,
    pub cancelled: bool,
}

#[derive(Debug)]
pub enum Error {
    /// Dossier absent (disque débranché…) : rien n'est modifié.
    RootMissing(PathBuf),
    /// Aucun fichier alors que la base en connaît : par prudence, rien n'est retiré.
    RootEmpty(PathBuf),
    Database(audioplayer_db::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::RootMissing(path) => write!(f, "{}: folder not found", path.display()),
            Error::RootEmpty(path) => write!(f, "{}: no audio file found", path.display()),
            Error::Database(error) => write!(f, "{error}"),
        }
    }
}

impl From<audioplayer_db::Error> for Error {
    fn from(error: audioplayer_db::Error) -> Self {
        Error::Database(error)
    }
}

// État de la recherche de pochette, par dossier : une image suffit pour tout le dossier.
#[derive(Default)]
struct FolderArt {
    done: bool,
    folder_tried: bool,
}

struct Read {
    record: TrackRecord,
    thumbnail: Option<artwork::Thumbnail>,
    readable: bool,
}

pub fn scan(
    db_path: &Path,
    library_id: i64,
    root: &Path,
    mode: Mode,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Report, Error> {
    if !root.is_dir() {
        return Err(Error::RootMissing(root.to_owned()));
    }
    let mut db = Database::open(db_path)?;
    let known = db.library_files(library_id)?;

    let mut folders = 0;
    let found = files::find(root, |_| {
        folders += 1;
        if folders % 50 == 0 {
            progress(Progress::Listing { folders });
        }
    });
    if found.is_empty() && !known.is_empty() {
        return Err(Error::RootEmpty(root.to_owned()));
    }

    let mut report = Report::default();
    let to_read: Vec<&files::AudioFile> = found
        .iter()
        .filter(|file| {
            let key = file.path.to_string_lossy();
            let changed = match known.get(key.as_ref()) {
                None => true,
                Some(k) => mode == Mode::Full || k.size != Some(file.size) || k.mtime != Some(file.mtime),
            };
            if !changed {
                report.unchanged += 1;
            }
            changed
        })
        .collect();

    // Pochette déjà présente en grande taille : rien à chercher dans ces dossiers.
    let mut art: HashMap<PathBuf, FolderArt> = db
        .folders_with_artwork(library_id, artwork::SIZE)?
        .into_iter()
        .map(|folder| (PathBuf::from(folder), FolderArt { done: true, folder_tried: true }))
        .collect();
    if mode == Mode::Full {
        art.clear();
    }
    let art = Mutex::new(art);

    let total = to_read.len();
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::sync_channel::<Read>(BATCH * 2);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let mut done = 0;

    std::thread::scope(|scope| -> Result<(), Error> {
        for _ in 0..threads {
            let sender = sender.clone();
            let (to_read, next, art) = (&to_read, &next, &art);
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = to_read.get(index) else { return };
                let read = read_file(file, art);
                if sender.send(read).is_err() {
                    return;
                }
            });
        }
        drop(sender);

        let mut batch: Vec<Read> = Vec::with_capacity(BATCH);
        let mut flush = |batch: &mut Vec<Read>, report: &mut Report| -> Result<(), Error> {
            if batch.is_empty() {
                return Ok(());
            }
            let records: Vec<TrackRecord> = batch.iter().map(|r| r.record.clone()).collect();
            let albums = db.write_tracks(library_id, &records)?;
            let mut with_art = HashSet::new();
            for (read, album) in batch.drain(..).zip(albums) {
                if known.contains_key(&read.record.path) {
                    report.updated += 1;
                } else {
                    report.added += 1;
                }
                report.unreadable += usize::from(!read.readable);
                if let Some(thumbnail) = read.thumbnail {
                    if with_art.insert(album) {
                        db.set_artwork(album, &[thumbnail])?;
                        report.artworks += 1;
                    }
                }
            }
            Ok(())
        };
        for read in receiver {
            done += 1;
            if done % 25 == 0 || done == total {
                let folder = read.record.folder.clone().unwrap_or_default();
                progress(Progress::Reading { done, total, folder });
            }
            batch.push(read);
            if batch.len() >= BATCH {
                flush(&mut batch, &mut report)?;
            }
        }
        flush(&mut batch, &mut report)
    })?;

    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        return Ok(report);
    }

    // Disparus : absents du dossier et introuvables sur le disque (double vérification).
    let present: HashSet<String> = found.iter().map(|f| f.path.to_string_lossy().into_owned()).collect();
    let removed: Vec<i64> = known
        .iter()
        .filter(|(path, _)| !present.contains(*path) && !Path::new(path).exists())
        .map(|(_, k)| k.track_id)
        .collect();
    report.removed = removed.len();
    db.remove_tracks(library_id, &removed)?;
    db.mark_scanned(library_id)?;
    Ok(report)
}

fn read_file(file: &files::AudioFile, art: &Mutex<HashMap<PathBuf, FolderArt>>) -> Read {
    let (record, picture, readable) = match tags::read(file) {
        Ok(read) => (read.record, read.picture, true),
        Err(_) => (tags::minimal(file), None, false),
    };
    let dir = file.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let wanted = {
        let mut art = art.lock().unwrap();
        let state = art.entry(dir.clone()).or_default();
        if state.done {
            None
        } else if picture.is_some() {
            Some(false)
        } else if !state.folder_tried {
            state.folder_tried = true;
            Some(true)
        } else {
            None
        }
    };
    let thumbnail = wanted.and_then(|from_folder| {
        let data = if from_folder { artwork::from_folder(&dir) } else { picture };
        artwork::thumbnail(&data?)
    });
    if thumbnail.is_some() {
        art.lock().unwrap().entry(dir).or_default().done = true;
    }
    Read { record, thumbnail, readable }
}
