//! Scan d'une bibliothèque : fichiers nouveaux ou modifiés relus en parallèle, écrits
//! par lots, fichiers disparus retirés. Les règles de tags et de regroupement sont
//! celles de la v0.1, avec ses correctifs (voir `tags`).

pub mod artwork;
pub mod files;
pub mod tags;
pub mod write;

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
    /// Tags illisibles : un nouveau fichier est enregistré avec le nom du fichier et du
    /// dossier ; un fichier déjà connu garde ce que la base en sait.
    pub unreadable: usize,
    /// Fichiers ou dossiers inaccessibles (disque débranché…) : rien n'a été retiré.
    pub inaccessible: usize,
    pub artworks: usize,
    /// Albums apparus et disparus (« artiste — titre », triés) ; un album retagué sous un
    /// autre nom compte dans les deux.
    pub albums_added: Vec<String>,
    pub albums_removed: Vec<String>,
    /// Albums restés en place dont des morceaux ont été relus, ajoutés ou retirés.
    pub albums_updated: usize,
    pub cancelled: bool,
}

#[derive(Debug)]
pub enum Error {
    /// Dossier absent (disque débranché…) : rien n'est modifié.
    RootMissing(PathBuf),
    /// Aucun fichier alors que la base en connaît : par prudence, rien n'est retiré.
    RootEmpty(PathBuf),
    /// « Add Folder » : le dossier choisi n'est pas dans celui de la bibliothèque.
    OutsideLibrary(PathBuf),
    Database(audioplayer_db::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::RootMissing(path) => write!(f, "{}: folder not found", path.display()),
            Error::RootEmpty(path) => write!(f, "{}: no audio file found", path.display()),
            Error::OutsideLibrary(path) => write!(f, "{}: not inside the library folder", path.display()),
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
    // Un fil décode déjà l'image intégrée d'un fichier de ce dossier : les autres fils, qui
    // lisent souvent le même dossier en même temps, ne décodent pas la leur aussi.
    decoding: bool,
}

struct Read {
    // `None` : rien à écrire (fichier connu illisible, ou inaccessible).
    record: Option<TrackRecord>,
    thumbnail: Option<artwork::Thumbnail>,
    folder: String,
    unreadable: bool,
    inaccessible: bool,
}

pub fn scan(
    db_path: &Path,
    library_id: i64,
    root: &Path,
    mode: Mode,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Report, Error> {
    scan_tree(db_path, library_id, root, mode, true, cancel, progress)
}

/// « Add Folder » : « Scan for Changes » limité à un dossier de la bibliothèque (un album
/// copié dans son dossier), sans parcourir le reste.
pub fn scan_folder(
    db_path: &Path,
    library_id: i64,
    root: &Path,
    folder: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Report, Error> {
    if !root.is_dir() {
        return Err(Error::RootMissing(root.to_owned()));
    }
    if !folder.starts_with(root) {
        return Err(Error::OutsideLibrary(folder.to_owned()));
    }
    scan_tree(db_path, library_id, folder, Mode::Changes, folder == root, cancel, progress)
}

// `whole` : `root` est le dossier de la bibliothèque, et non l'un de ses sous-dossiers.
fn scan_tree(
    db_path: &Path,
    library_id: i64,
    root: &Path,
    mode: Mode,
    whole: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Report, Error> {
    if !root.is_dir() {
        return Err(Error::RootMissing(root.to_owned()));
    }
    let mut db = Database::open(db_path)?;
    let mut known = db.library_files(library_id)?;
    let albums_before = db.album_summaries(library_id)?;
    // Sous-dossier : le reste de la bibliothèque n'est ni relu ni retiré.
    if !whole {
        known.retain(|path, _| Path::new(path).starts_with(root));
    }

    let mut folders = 0;
    let listing = files::find(root, |_| {
        folders += 1;
        if folders % 50 == 0 {
            progress(Progress::Listing { folders });
        }
        !cancel.load(Ordering::Relaxed)
    });
    // Liste incomplète : surtout ne rien retirer.
    if cancel.load(Ordering::Relaxed) {
        return Ok(Report { cancelled: true, ..Report::default() });
    }
    let (found, unreadable_dirs) = (listing.files, listing.unreadable_dirs);
    if found.is_empty() && !known.is_empty() {
        return Err(Error::RootEmpty(root.to_owned()));
    }

    // Dossier renommé en changeant seulement des majuscules : le morceau garde sa ligne (écoutes,
    // favoris, playlists) sous son nouveau chemin au lieu de revenir comme un morceau neuf.
    let present: HashSet<String> = found.iter().map(|f| f.path.to_string_lossy().into_owned()).collect();
    let mut gone: HashMap<String, String> = known
        .keys()
        .filter(|path| !present.contains(*path))
        .map(|path| (path.to_lowercase(), path.clone()))
        .collect();
    let new: Vec<&String> = present.iter().filter(|path| !known.contains_key(*path)).collect();
    let mut renamed = Vec::new();
    for path in new {
        if let Some(old) = gone.remove(&path.to_lowercase()) {
            let file = known.remove(&old).expect("known path");
            renamed.push((file.track_id, path.clone()));
            known.insert(path.clone(), file);
        }
    }
    db.rename_tracks(&renamed)?;

    let mut report = Report { inaccessible: listing.errors, ..Report::default() };
    // Morceaux importés de la v0.1, qui ne gardait que la taille : à taille égale, le fichier
    // est tenu pour inchangé et sa date est seulement notée, sans relire ses tags.
    let mut dated = Vec::new();
    let to_read: Vec<&files::AudioFile> = found
        .iter()
        .filter(|file| {
            let key = file.path.to_string_lossy();
            let changed = match known.get(key.as_ref()) {
                None => true,
                Some(_) if mode == Mode::Full => true,
                Some(k) if k.mtime.is_none() && k.size == Some(file.size) => {
                    dated.push((k.track_id, file.mtime));
                    false
                }
                Some(k) => k.size != Some(file.size) || k.mtime != Some(file.mtime),
            };
            if !changed {
                report.unchanged += 1;
            }
            changed
        })
        .collect();
    db.set_file_mtimes(&dated)?;

    // Pochette déjà présente : rien à chercher dans ces dossiers.
    let mut art: HashMap<PathBuf, FolderArt> = db
        .folders_with_artwork(library_id, artwork::SIZE)?
        .into_iter()
        .map(|folder| (PathBuf::from(folder), FolderArt { done: true, folder_tried: true, decoding: false }))
        .collect();
    if mode == Mode::Full {
        art.clear();
    }
    // Scan complet : l'année et le drapeau compilation des albums sont repris des fichiers.
    let mut refreshed = (mode == Mode::Full).then(HashSet::new);
    let mut touched = HashSet::new();
    let processed = process(&mut db, library_id, &known, &to_read, art, refreshed.as_mut(), &mut touched, cancel, progress, &mut report);
    // Quoi qu'il arrive ensuite (arrêt, disque perdu, erreur), les albums vidés par un
    // regroupement déjà écrit ne restent pas affichés.
    db.remove_orphans(library_id)?;
    processed?;

    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        compare_albums(&db, library_id, &albums_before, &touched, &mut report)?;
        return Ok(report);
    }
    // Disque débranché pendant la lecture : la suite retirerait tout ce qui n'a pas été vu.
    if !root.is_dir() {
        return Err(Error::RootMissing(root.to_owned()));
    }

    // Disparus : absents du dossier et introuvables sur le disque (double vérification). Sous
    // un dossier illisible, la liste est incomplète : rien n'y est retiré. Un dossier renommé
    // en changeant seulement des majuscules reste « présent » sur un disque qui ne les
    // distingue pas (exFAT, APFS) : l'ancien chemin, doublon du nouveau déjà en base, est retiré aussi.
    let present_lower: HashSet<String> = present.iter().map(|p| p.to_lowercase()).collect();
    let removed: Vec<i64> = known
        .iter()
        .filter(|(path, _)| !present.contains(*path) && (present_lower.contains(&path.to_lowercase()) || !Path::new(path).exists()))
        .filter(|(path, _)| !unreadable_dirs.iter().any(|dir| Path::new(path).starts_with(dir)))
        .map(|(_, k)| k.track_id)
        .collect();
    report.removed = removed.len();
    db.remove_tracks(library_id, &removed)?;
    if whole {
        db.mark_scanned(library_id)?;
    }
    compare_albums(&db, library_id, &albums_before, &touched, &mut report)?;
    Ok(report)
}

// Bilan par album, d'après les albums d'avant le scan et ceux écrits pendant.
fn compare_albums(
    db: &Database,
    library_id: i64,
    before: &HashMap<i64, (String, u32)>,
    touched: &HashSet<i64>,
    report: &mut Report,
) -> Result<(), Error> {
    let after = db.album_summaries(library_id)?;
    let names = |from: &HashMap<i64, (String, u32)>, other: &HashMap<i64, (String, u32)>| {
        let mut names: Vec<String> = from.iter().filter(|(id, _)| !other.contains_key(*id)).map(|(_, (name, _))| name.clone()).collect();
        names.sort_by_key(|name| name.to_lowercase());
        names
    };
    report.albums_added = names(&after, before);
    report.albums_removed = names(before, &after);
    report.albums_updated = after
        .iter()
        .filter(|(id, (_, count))| before.get(*id).is_some_and(|(_, old)| old != count || touched.contains(*id)))
        .count();
    Ok(())
}

// Lit `to_read` sur tous les cœurs et écrit les résultats par lots.
#[allow(clippy::too_many_arguments)]
fn process(
    db: &mut Database,
    library_id: i64,
    known: &HashMap<String, audioplayer_db::KnownFile>,
    to_read: &[&files::AudioFile],
    art: HashMap<PathBuf, FolderArt>,
    mut refreshed: Option<&mut HashSet<i64>>,
    touched: &mut HashSet<i64>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
    report: &mut Report,
) -> Result<(), Error> {
    let art = Mutex::new(art);
    let total = to_read.len();
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::sync_channel::<Read>(BATCH * 2);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let mut done = 0;

    std::thread::scope(|scope| -> Result<(), Error> {
        for _ in 0..threads {
            let sender = sender.clone();
            let (next, art) = (&next, &art);
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = to_read.get(index) else { return };
                let is_known = known.contains_key(file.path.to_string_lossy().as_ref());
                // Toute la lecture (tags et pochette) : un fichier qui fait paniquer lofty ou image
                // ne doit pas arrêter le scan ; il compte comme illisible.
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_file(file, is_known, art)))
                    .unwrap_or_else(|_| Read {
                        record: (!is_known).then(|| tags::minimal(file)),
                        thumbnail: None,
                        folder: String::new(),
                        unreadable: true,
                        inaccessible: false,
                    });
                if sender.send(read).is_err() {
                    return;
                }
            });
        }
        drop(sender);

        let mut batch: Vec<Read> = Vec::with_capacity(BATCH);
        // Albums écrits pendant ce scan : ceux restés sans pochette en reçoivent une à la fin.
        let mut flush = |batch: &mut Vec<Read>, report: &mut Report, touched: &mut HashSet<i64>| -> Result<(), Error> {
            if batch.is_empty() {
                return Ok(());
            }
            let has_record: Vec<bool> = batch.iter().map(|r| r.record.is_some()).collect();
            let records: Vec<TrackRecord> = batch.iter_mut().filter_map(|r| r.record.take()).collect();
            let paths: Vec<String> = records.iter().map(|r| r.path.clone()).collect();
            let albums = db.write_tracks(library_id, &records, refreshed.as_deref_mut())?;
            let mut with_art = HashSet::new();
            let mut written = paths.into_iter().zip(albums);
            for (read, has_record) in batch.drain(..).zip(has_record) {
                report.unreadable += usize::from(read.unreadable);
                report.inaccessible += usize::from(read.inaccessible);
                if !has_record {
                    continue;
                }
                let Some((path, album)) = written.next() else { break };
                touched.insert(album);
                if known.contains_key(&path) {
                    report.updated += 1;
                } else {
                    report.added += 1;
                }
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
                let folder = read.folder.clone();
                progress(Progress::Reading { done, total, folder });
            }
            batch.push(read);
            if batch.len() >= BATCH {
                flush(&mut batch, report, touched)?;
            }
        }
        flush(&mut batch, report, touched)?;
        if !cancel.load(Ordering::Relaxed) {
            fill_missing_artwork(db, touched, report)?;
        }
        Ok(())
    })
}

// La pochette est cherchée par dossier pendant la lecture ; un album sans pochette à la fin
// (retagué ailleurs dans un dossier qui en avait une, deuxième album d'un dossier, image
// intégrée illisible) la prend dans l'une de ses pistes, sinon dans l'image du dossier.
fn fill_missing_artwork(db: &mut Database, albums: &HashSet<i64>, report: &mut Report) -> Result<(), Error> {
    let albums: Vec<i64> = albums.iter().copied().collect();
    for (album, path) in db.albums_missing_artwork(&albums)? {
        let path = PathBuf::from(path);
        let file = files::AudioFile { path: path.clone(), size: 0, mtime: 0 };
        let picture = tags::read_with(&file, true).ok().and_then(|read| read.picture);
        let thumbnail = picture
            .and_then(|data| artwork::thumbnail(&data))
            .or_else(|| path.parent().and_then(artwork::from_folder).and_then(|data| artwork::thumbnail(&data)));
        if let Some(thumbnail) = thumbnail {
            db.set_artwork(album, &[thumbnail])?;
            report.artworks += 1;
        }
    }
    Ok(())
}

/// « Rescan Audio Tags » de la v0.1 : relit ces fichiers (et leur pochette) même
/// inchangés ; ceux qui n'existent plus sont retirés de la bibliothèque.
pub fn rescan_files(
    db_path: &Path,
    library_id: i64,
    paths: &[PathBuf],
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Report, Error> {
    let mut db = Database::open(db_path)?;
    let known = db.library_files(library_id)?;
    let mut report = Report::default();
    let mut missing = Vec::new();
    let mut present = Vec::new();
    for path in paths {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64);
                present.push(files::AudioFile { path: path.clone(), size: meta.len(), mtime });
            }
            // Disparu seulement si son dossier est là : un disque débranché rend tout
            // illisible, et rien ne doit alors être retiré.
            _ if path.parent().is_some_and(Path::is_dir) => {
                if let Some(file) = known.get(path.to_string_lossy().as_ref()) {
                    missing.push(file.track_id);
                }
            }
            _ => report.inaccessible += 1,
        }
    }
    // Tout l'album de chaque fichier relu : l'année et le drapeau compilation viennent de
    // l'ensemble de ses pistes, pas du seul morceau modifié.
    let rescanned: Vec<i64> = present.iter().filter_map(|f| known.get(f.path.to_string_lossy().as_ref())).map(|k| k.track_id).collect();
    let asked: HashSet<PathBuf> = present.iter().map(|f| f.path.clone()).collect();
    for mate in db.album_mates(&rescanned)? {
        let path = PathBuf::from(&mate);
        if asked.contains(&path) {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            present.push(files::AudioFile { path, size: meta.len(), mtime });
        }
    }
    let to_read: Vec<&files::AudioFile> = present.iter().collect();
    let (mut refreshed, mut touched) = (HashSet::new(), HashSet::new());
    let processed = process(&mut db, library_id, &known, &to_read, HashMap::new(), Some(&mut refreshed), &mut touched, cancel, progress, &mut report);
    db.remove_orphans(library_id)?;
    processed?;
    report.removed = missing.len();
    db.remove_tracks(library_id, &missing)?;
    Ok(report)
}

fn read_file(file: &files::AudioFile, known: bool, art: &Mutex<HashMap<PathBuf, FolderArt>>) -> Read {
    let folder = file.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let skip = |unreadable, inaccessible| Read { record: None, thumbnail: None, folder: folder.clone(), unreadable, inaccessible };
    // Une erreur de lofty ou d'image (fichier corrompu) ne doit pas arrêter tout le scan.
    let dir = file.path.parent().map(Path::to_path_buf).unwrap_or_default();
    // Dossier déjà pourvu d'une pochette : inutile de lire l'image intégrée.
    let picture_wanted = !art.lock().unwrap().get(&dir).is_some_and(|state| state.done);
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tags::read_with(file, picture_wanted)))
        .unwrap_or_else(|_| Err("panic".into()));
    let (record, picture, readable) = match read {
        Ok(read) => (read.record, read.picture, true),
        // Fichier devenu inaccessible (disque débranché…) : surtout ne rien écrire.
        Err(_) if std::fs::metadata(&file.path).is_err() => return skip(false, true),
        // Tags illisibles : un fichier connu garde ce que la base en sait.
        Err(_) if known => return skip(true, false),
        Err(_) => (tags::minimal(file), None, false),
    };
    let wanted = {
        let mut art = art.lock().unwrap();
        let state = art.entry(dir.clone()).or_default();
        if state.done {
            None
        } else if picture.is_some() {
            (!state.decoding).then(|| {
                state.decoding = true;
                false
            })
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
    if let Some(from_folder) = wanted {
        let mut art = art.lock().unwrap();
        let state = art.entry(dir).or_default();
        state.decoding &= from_folder;
        state.done |= thumbnail.is_some();
    }
    Read { record: Some(record), thumbnail, folder, unreadable: !readable, inaccessible: false }
}
