//! Lecture des tags d'un fichier, avec les règles de la v0.1.

use std::path::Path;

use audioplayer_db::TrackRecord;
use lofty::file::FileType;
use lofty::picture::PictureType;
use lofty::file::AudioFile as _;
use lofty::prelude::{Accessor, ItemKey, TaggedFileExt};
use lofty::tag::Tag;

use crate::files::AudioFile;

pub const UNKNOWN_ARTIST: &str = "Unknown Artist";
pub const UNKNOWN_ALBUM: &str = "Unknown Album";

/// Un fichier lu, avec sa pochette intégrée encodée telle quelle.
pub struct Read {
    pub record: TrackRecord,
    pub picture: Option<Vec<u8>>,
}

// Texte vide ou blanc → absent.
fn clean(value: Option<impl AsRef<str>>) -> Option<String> {
    value.map(|v| v.as_ref().trim().to_owned()).filter(|v| !v.is_empty())
}

fn text(tag: Option<&Tag>, key: ItemKey) -> Option<String> {
    clean(tag.and_then(|t| t.get_string(key)))
}

fn number(value: Option<String>) -> Option<u32> {
    let digits: String = value?.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|&n| n > 0)
}

/// « 2/3 », « 2 of 3 » ou « 2 » → 2.
fn disc(text: &str) -> Option<u32> {
    let first = text.split('/').next()?.split(" of ").next()?;
    number(Some(first.to_owned()))
}

fn codec(file_type: FileType) -> Option<&'static str> {
    Some(match file_type {
        FileType::Mpeg => "MP3",
        FileType::Flac => "FLAC",
        FileType::Mp4 => "AAC",
        FileType::Vorbis | FileType::Opus => "OGG",
        FileType::Wav => "WAV",
        FileType::Aiff => "AIFF",
        _ => return None,
    })
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Unknown Track".into())
}

// Album sans tag : nom du dossier (la v0.1 mettait « Unknown Album », qui mélangeait
// tous les morceaux sans album d'un même artiste).
fn folder_album(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| UNKNOWN_ALBUM.into())
}

fn folder(path: &Path) -> Option<String> {
    path.parent().map(|p| p.to_string_lossy().into_owned())
}

/// Tags illisibles : le titre vient du nom de fichier, l'album du nom du dossier (comme la v0.1).
pub fn minimal(file: &AudioFile) -> TrackRecord {
    TrackRecord {
        path: file.path.to_string_lossy().into_owned(),
        size: file.size,
        mtime: file.mtime,
        title: stem(&file.path),
        artist: UNKNOWN_ARTIST.into(),
        album: folder_album(&file.path),
        folder: folder(&file.path),
        ..Default::default()
    }
}

pub fn read(file: &AudioFile) -> Result<Read, String> {
    let tagged = lofty::read_from_path(&file.path).map_err(|e| e.to_string())?;
    let properties = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let artist = clean(tag.and_then(|t| t.artist())).unwrap_or_else(|| UNKNOWN_ARTIST.into());
    // Regroupement par artiste d'album s'il est renseigné (correctif 0.1.4 de la v0.1).
    let album_artist = text(tag, ItemKey::AlbumArtist).filter(|a| *a != artist);
    let year = tag
        .and_then(|t| t.date())
        .map(|d| d.year as i32)
        .or_else(|| number(text(tag, ItemKey::Year)).map(|y| y as i32))
        .filter(|&y| y > 0);
    let disc_number = text(tag, ItemKey::DiscNumber).and_then(|d| disc(&d)).or_else(|| tag.and_then(|t| t.disk()));
    let compilation = text(tag, ItemKey::FlagCompilation).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let picture = tag.and_then(|t| {
        let pictures = t.pictures();
        pictures
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .or_else(|| pictures.first())
            .map(|p| p.data().to_vec())
            .filter(|data| !data.is_empty())
    });

    let record = TrackRecord {
        path: file.path.to_string_lossy().into_owned(),
        size: file.size,
        mtime: file.mtime,
        title: clean(tag.and_then(|t| t.title())).unwrap_or_else(|| stem(&file.path)),
        album: clean(tag.and_then(|t| t.album())).unwrap_or_else(|| folder_album(&file.path)),
        artist,
        album_artist,
        year,
        track_number: tag.and_then(|t| t.track()).filter(|&n| n > 0),
        disc_number: disc_number.filter(|&n| n > 0),
        disc_subtitle: text(tag, ItemKey::SetSubtitle),
        duration_ms: properties.duration().as_millis() as u64,
        codec: codec(tagged.file_type()).map(str::to_owned),
        bitrate: properties.overall_bitrate().or_else(|| properties.audio_bitrate()),
        sample_rate: properties.sample_rate(),
        bit_depth: properties.bit_depth().map(u32::from),
        channels: properties.channels().map(u32::from),
        genre: clean(tag.and_then(|t| t.genre())),
        composer: text(tag, ItemKey::Composer),
        conductor: text(tag, ItemKey::Conductor),
        lyricist: text(tag, ItemKey::Lyricist),
        grouping: text(tag, ItemKey::ContentGroup),
        work: text(tag, ItemKey::Work),
        comment: clean(tag.and_then(|t| t.comment())),
        publisher: text(tag, ItemKey::Label).or_else(|| text(tag, ItemKey::Publisher)),
        catalog_number: text(tag, ItemKey::CatalogNumber),
        barcode: text(tag, ItemKey::Barcode),
        bpm: number(text(tag, ItemKey::Bpm).or_else(|| text(tag, ItemKey::IntegerBpm))),
        compilation,
        folder: folder(&file.path),
    };
    Ok(Read { record, picture })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disc_numbers() {
        assert_eq!(disc("2/3"), Some(2));
        assert_eq!(disc("2 of 3"), Some(2));
        assert_eq!(disc(" 4 "), Some(4));
        assert_eq!(disc("0"), None);
        assert_eq!(disc("x"), None);
    }
}
