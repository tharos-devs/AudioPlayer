//! Décodage des pochettes stockées en base vers des images Slint.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub fn decode(data: &[u8]) -> Option<Image> {
    let rgba = image::load_from_memory(data).ok()?.into_rgba8();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height());
    Some(Image::from_rgba8(buffer))
}
