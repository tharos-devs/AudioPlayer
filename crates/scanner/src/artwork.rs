//! Pochettes : image intégrée au fichier, sinon image du dossier. La base ne garde
//! qu'une vignette carrée de 150px par album, la taille des images intégrées aux
//! fichiers de l'utilisateur : une telle image est gardée telle quelle, sans
//! réencodage ni perte. Une grande image se relira dans le fichier si une vue en a besoin.

use std::path::Path;

use image::imageops::FilterType;

/// Taille des pochettes intégrées aux fichiers de l'utilisateur (la v0.1 gardait 80px).
pub const SIZE: u32 = 150;
const QUALITY: u8 = 90;

// Noms usuels, par ordre de préférence (comparés sans la casse ni l'extension).
const FOLDER_NAMES: &[&str] = &["cover", "folder", "front", "album", "albumart", "albumartlarge"];

/// Image du dossier d'un album (`cover.jpg`, `folder.png`…), s'il y en a une.
pub fn from_folder(dir: &Path) -> Option<Vec<u8>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name.starts_with('.') {
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        if !matches!(ext.as_deref(), Some("jpg" | "jpeg" | "png")) {
            continue;
        }
        let stem = path.file_stem()?.to_string_lossy().to_lowercase();
        if let Some(rank) = FOLDER_NAMES.iter().position(|n| *n == stem) {
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, path));
            }
        }
    }
    std::fs::read(best?.1).ok()
}

/// Vignette prête pour la base : `(côté, type MIME, octets)`.
pub type Thumbnail = (u32, String, Vec<u8>);

/// Une image carrée JPEG ou PNG d'au plus 150px est gardée telle quelle ; sinon elle est
/// recadrée au carré (centré) et réduite à 150px, jamais agrandie, puis encodée en JPEG.
pub fn thumbnail(data: &[u8]) -> Option<Thumbnail> {
    let format = image::guess_format(data).ok()?;
    let source = image::load_from_memory_with_format(data, format).ok()?;
    let (width, height) = (source.width(), source.height());
    let mime = match format {
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Png => Some("image/png"),
        _ => None,
    };
    if let Some(mime) = mime.filter(|_| width == height && width <= SIZE) {
        return Some((width, mime.to_owned(), data.to_vec()));
    }
    let size = SIZE.min(width.min(height));
    let square = source.resize_to_fill(size, size, FilterType::Lanczos3).to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, QUALITY).encode_image(&square).ok()?;
    Some((size, "image/jpeg".to_owned(), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([(x * 7) as u8, (y * 3) as u8, 90]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn square_150_is_kept_byte_for_byte() {
        let original = encode(150, 150, image::ImageFormat::Jpeg);
        assert_eq!(thumbnail(&original), Some((150, "image/jpeg".into(), original.clone())));
        let png = encode(120, 120, image::ImageFormat::Png);
        assert_eq!(thumbnail(&png).unwrap().1, "image/png");
    }

    #[test]
    fn larger_or_non_square_is_cropped_and_reduced() {
        let (size, mime, data) = thumbnail(&encode(600, 400, image::ImageFormat::Png)).unwrap();
        assert_eq!((size, mime.as_str()), (150, "image/jpeg"));
        let decoded = image::load_from_memory(&data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (150, 150));
        // Jamais agrandie : 100x80 donne 80x80.
        assert_eq!(thumbnail(&encode(100, 80, image::ImageFormat::Jpeg)).unwrap().0, 80);
        assert_eq!(thumbnail(b"not an image"), None);
    }
}
