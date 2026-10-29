//! Écriture des tags dans les fichiers (« Edit Tags » et « Set Album Artwork » de la v0.1).
//! Seuls les champs demandés sont touchés : tout le reste du fichier (totaux de pistes,
//! autres tags, autres images) est conservé.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::AudioFile as _;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey, TaggedFileExt};
use lofty::tag::items::Timestamp;
use lofty::tag::Tag;

/// Champs modifiables depuis le tiroir d'informations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    TrackNumber,
    DiscNumber,
    Year,
    Genre,
    Composer,
    Conductor,
    Lyricist,
    Grouping,
    Work,
    Comment,
    Publisher,
    CatalogNumber,
    Barcode,
    DiscSubtitle,
}

impl Field {
    pub const ALL: [Field; 18] = [
        Field::Title,
        Field::Artist,
        Field::AlbumArtist,
        Field::Album,
        Field::TrackNumber,
        Field::DiscNumber,
        Field::Year,
        Field::Genre,
        Field::Composer,
        Field::Conductor,
        Field::Lyricist,
        Field::Grouping,
        Field::Work,
        Field::Comment,
        Field::Publisher,
        Field::CatalogNumber,
        Field::Barcode,
        Field::DiscSubtitle,
    ];

    /// Nom stable, partagé avec l'interface.
    pub fn key(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Artist => "artist",
            Field::AlbumArtist => "album-artist",
            Field::Album => "album",
            Field::TrackNumber => "track-number",
            Field::DiscNumber => "disc-number",
            Field::Year => "year",
            Field::Genre => "genre",
            Field::Composer => "composer",
            Field::Conductor => "conductor",
            Field::Lyricist => "lyricist",
            Field::Grouping => "grouping",
            Field::Work => "work",
            Field::Comment => "comment",
            Field::Publisher => "publisher",
            Field::CatalogNumber => "catalog-number",
            Field::Barcode => "barcode",
            Field::DiscSubtitle => "disc-subtitle",
        }
    }

    pub fn from_key(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.key() == key)
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Field::TrackNumber | Field::DiscNumber | Field::Year)
    }

    fn item_key(self) -> Option<ItemKey> {
        Some(match self {
            Field::AlbumArtist => ItemKey::AlbumArtist,
            Field::Composer => ItemKey::Composer,
            Field::Conductor => ItemKey::Conductor,
            Field::Lyricist => ItemKey::Lyricist,
            Field::Grouping => ItemKey::ContentGroup,
            Field::Work => ItemKey::Work,
            Field::Publisher => ItemKey::Label,
            Field::CatalogNumber => ItemKey::CatalogNumber,
            Field::Barcode => ItemKey::Barcode,
            Field::DiscSubtitle => ItemKey::SetSubtitle,
            _ => return None,
        })
    }
}

/// Modification d'un champ : `None` (ou un texte vide) retire le tag.
pub type Edit = (Field, Option<String>);

fn number(value: &str) -> Result<u32, String> {
    value.trim().parse().map_err(|_| format!("not a number: {value}"))
}

fn apply(tag: &mut Tag, field: Field, value: Option<&str>) -> Result<(), String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    match (field, value) {
        (Field::Title, Some(v)) => tag.set_title(v.to_owned()),
        (Field::Title, None) => tag.remove_title(),
        (Field::Artist, Some(v)) => tag.set_artist(v.to_owned()),
        (Field::Artist, None) => tag.remove_artist(),
        (Field::Album, Some(v)) => tag.set_album(v.to_owned()),
        (Field::Album, None) => tag.remove_album(),
        (Field::Genre, Some(v)) => tag.set_genre(v.to_owned()),
        (Field::Genre, None) => tag.remove_genre(),
        (Field::Comment, Some(v)) => tag.set_comment(v.to_owned()),
        (Field::Comment, None) => tag.remove_comment(),
        (Field::TrackNumber, Some(v)) => tag.set_track(number(v)?),
        (Field::TrackNumber, None) => tag.remove_track(),
        (Field::DiscNumber, Some(v)) => tag.set_disk(number(v)?),
        (Field::DiscNumber, None) => tag.remove_disk(),
        (Field::Year, Some(v)) => {
            let year = u16::try_from(number(v)?).map_err(|_| format!("not a year: {v}"))?;
            tag.remove_key(ItemKey::Year);
            tag.set_date(Timestamp { year, ..Timestamp::default() });
        }
        (Field::Year, None) => {
            tag.remove_date();
            tag.remove_key(ItemKey::Year);
        }
        (field, value) => {
            let key = field.item_key().expect("text field");
            // L'éditeur peut aussi se trouver sous Publisher (lu en secours par le scan).
            if field == Field::Publisher {
                tag.remove_key(ItemKey::Publisher);
            }
            match value {
                Some(v) => {
                    tag.insert_text(key, v.to_owned());
                }
                None => tag.remove_key(key),
            }
        }
    }
    Ok(())
}

/// Écrit `edits` (et, s'il y en a une, la pochette en couverture avant) dans le fichier.
pub fn write_tags(path: &Path, edits: &[Edit], cover: Option<(&str, &[u8])>) -> Result<(), String> {
    let mut tagged = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged.primary_tag_mut().expect("primary tag inserted above");
    for (field, value) in edits {
        apply(tag, *field, value.as_deref())?;
    }
    if let Some((mime, data)) = cover {
        let mime = if mime == "image/png" { MimeType::Png } else { MimeType::Jpeg };
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(Picture::unchecked(data.to_vec()).pic_type(PictureType::CoverFront).mime_type(mime).build());
    }
    tagged.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
}
