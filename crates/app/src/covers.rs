//! Décodage des pochettes stockées en base vers des images Slint.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub type Pixels = SharedPixelBuffer<Rgba8Pixel>;

pub fn decode(data: &[u8]) -> Option<Pixels> {
    let rgba = image::load_from_memory(data).ok()?.into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()))
}

/// Décode un lot de pochettes sur tous les cœurs ; les échecs sont ignorés.
pub fn decode_all(artworks: Vec<(i64, Vec<u8>)>) -> Vec<(i64, Pixels)> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = artworks.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = artworks
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().filter_map(|(id, data)| Some((*id, decode(data)?))).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
    })
}

// `Image` n'est pas `Send` : la conversion se fait sur le fil de l'interface.
pub fn to_image(pixels: Pixels) -> Image {
    Image::from_rgba8(pixels)
}
