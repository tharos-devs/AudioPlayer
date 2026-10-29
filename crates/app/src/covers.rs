//! Jaquettes factices dessinées à la volée, en attendant les vraies pochettes
//! extraites des fichiers audio. Un petit jeu de visuels est partagé entre les albums.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

// 160px = jaquette de 80px en Retina (@2x).
const SIZE: u32 = 160;

pub fn generate(count: usize) -> Vec<Image> {
    (0..count).map(|seed| render(seed as u32)).collect()
}

fn render(seed: u32) -> Image {
    let hue = (seed * 47 % 360) as f32;
    let base = hsv(hue, 0.55, 0.85);
    let second = hsv((hue + 40.0 + (seed % 5) as f32 * 25.0) % 360.0, 0.65, 0.45);
    let accent = hsv((hue + 180.0) % 360.0, 0.35, 0.95);
    let style = seed % 4;

    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(SIZE, SIZE);
    let size = SIZE as f32;
    for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = (i as u32 % SIZE) as f32 / size;
        let y = (i as u32 / SIZE) as f32 / size;
        let gradient = mix(base, second, (x + y) / 2.0);
        let color = match style {
            // Disque clair sur dégradé diagonal
            0 => {
                let d = ((x - 0.62).powi(2) + (y - 0.38).powi(2)).sqrt();
                mix(gradient, accent, coverage(0.24 - d))
            }
            // Bandes horizontales
            1 => {
                let band = ((y * 7.0).floor() as u32 + seed) % 3;
                mix(gradient, accent, if band == 0 { 0.55 } else { 0.0 })
            }
            // Soleil couchant sur un horizon
            2 => {
                let d = ((x - 0.5).powi(2) + (y - 0.62).powi(2)).sqrt();
                let sky = mix(second, base, y);
                let sun = mix(sky, accent, coverage(0.22 - d));
                if y > 0.62 { mix(second, [0, 0, 0], 0.35) } else { sun }
            }
            // Damier décalé
            _ => {
                let cell = ((x * 4.0).floor() as u32 + (y * 4.0).floor() as u32) % 2;
                mix(gradient, accent, if cell == 0 { 0.25 } else { 0.0 })
            }
        };
        *pixel = Rgba8Pixel { r: color[0], g: color[1], b: color[2], a: 255 };
    }
    Image::from_rgba8(buffer)
}

// Anti-crénelage approximatif : passe de 0 à 1 sur environ un pixel au bord d'une forme.
fn coverage(distance_inside: f32) -> f32 {
    (distance_inside * SIZE as f32).clamp(0.0, 1.0)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

fn hsv(hue: f32, saturation: f32, value: f32) -> [u8; 3] {
    let c = value * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    [r, g, b].map(|v| ((v + m) * 255.0) as u8)
}
