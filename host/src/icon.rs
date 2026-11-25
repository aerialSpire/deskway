// Deskway's D mark, shared by the host, browser client and README.
// Plain Rust with no dependencies, so build.rs can include it too.

const ACCENT: [u8; 3] = [0x25, 0x63, 0xEB]; // design.css --accent
const WHITE: [f32; 3] = [255.0; 3];
const APP_SCALE: f32 = 1.12;

enum Region {
    Screen,
    RightHalf,
    Overlap,
}

impl Region {
    fn opacity(self) -> f32 {
        match self {
            Self::Screen => 0.62,
            Self::RightHalf => 0.36,
            Self::Overlap => 1.0,
        }
    }
}

/// The circle's left half lies entirely inside the screen. Only the screen's left corners round.
fn mark(x: f32, y: f32, scale: f32) -> Option<Region> {
    let x = ((x - 0.5) / scale + 0.5) * 512.0;
    let y = ((y - 0.5) / scale + 0.5) * 512.0;
    const JOIN: f32 = 266.0;
    const CIRCLE_RADIUS: f32 = 150.0;
    const CORNER_RADIUS: f32 = 44.0;
    if (x - JOIN).powi(2) + (y - 256.0).powi(2) <= CIRCLE_RADIUS.powi(2) {
        return Some(if x <= JOIN { Region::Overlap } else { Region::RightHalf });
    }
    let cx = x.max(140.0);
    let cy = y.clamp(150.0, 362.0);
    (x >= 96.0 && x <= JOIN && (x - cx).powi(2) + (y - cy).powi(2) <= CORNER_RADIUS.powi(2))
        .then_some(Region::Screen)
}

fn tile_pixel(x: f32, y: f32, scale: f32) -> ([f32; 3], f32) {
    const LIGHT: [u8; 3] = [0x5A, 0x9B, 0xFF];
    const DARK: [u8; 3] = [0x1A, 0x45, 0xC8];
    const STOP: f32 = 0.52;
    let t = (x + y) / 2.0;
    let (from, to, t) = if t <= STOP {
        (LIGHT, ACCENT, t / STOP)
    } else {
        (ACCENT, DARK, (t - STOP) / (1.0 - STOP))
    };
    let opacity = mark(x, y, scale).map_or(0.0, Region::opacity);
    let rgb = std::array::from_fn(|i| {
        let base = f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * t;
        base + (WHITE[i] - base) * opacity
    });
    (rgb, 1.0)
}

/// Straight RGBA pixels for the native app tile or the white menu bar template.
pub fn rgba(size: u32, template: bool) -> Vec<u8> {
    if template {
        return draw(size, |x, y| mark(x, y, APP_SCALE).map(|region| (WHITE, region.opacity())));
    }
    draw(size, |x, y| {
        const RADIUS: f32 = 0.225;
        let cx = x.clamp(RADIUS, 1.0 - RADIUS);
        let cy = y.clamp(RADIUS, 1.0 - RADIUS);
        ((x - cx).powi(2) + (y - cy).powi(2) <= RADIUS.powi(2))
            .then(|| tile_pixel(x, y, APP_SCALE))
    })
}

fn draw(size: u32, sample_pixel: impl Fn(f32, f32) -> Option<([f32; 3], f32)>) -> Vec<u8> {
    const N: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let mut rgb = [0.0; 3];
            let mut alpha = 0.0;
            for sy in 0..N {
                for sx in 0..N {
                    let x = (px as f32 + (sx as f32 + 0.5) / N as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / N as f32) / size as f32;
                    if let Some((color, opacity)) = sample_pixel(x, y) {
                        for (sum, channel) in rgb.iter_mut().zip(color) {
                            *sum += channel * opacity;
                        }
                        alpha += opacity;
                    }
                }
            }
            let o = ((py * size + px) * 4) as usize;
            if alpha > 0.0 {
                // Average premultiplied samples, then store straight RGBA.
                for (channel, sum) in out[o..o + 3].iter_mut().zip(rgb) {
                    *channel = (sum / alpha).round() as u8;
                }
                out[o + 3] = (alpha * 255.0 / (N * N) as f32).round() as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colour_rgba(size: u32) -> Vec<u8> {
        draw(size, |x, y| {
            mark(x, y, 1.0).map(|region| {
                let rgb = match region {
                    Region::Screen => ACCENT,
                    Region::RightHalf => [0x8D, 0xB8, 0xFF],
                    Region::Overlap => [0x17, 0x3B, 0xB0],
                };
                (rgb.map(f32::from), 1.0)
            })
        })
    }

    // The unscaled mark fits in the maskable safe zone (a circle with radius 40% of the side).
    // Apple applies its own corner rounding to the full-size app tile.
    fn full_bleed_rgba(size: u32, scale: f32) -> Vec<u8> {
        draw(size, |x, y| Some(tile_pixel(x, y, scale)))
    }

    fn png(rgba: &[u8], size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, size, size);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(rgba).unwrap();
        out
    }

    #[test]
    fn the_template_preserves_all_three_opacities() {
        let pixels = rgba(32, true);
        let pixel = |x: usize, y: usize| {
            let offset = (y * 32 + x) * 4;
            &pixels[offset..offset + 4]
        };
        assert_eq!(pixel(0, 0), &[0, 0, 0, 0]);
        assert_eq!(pixel(5, 16), &[255, 255, 255, 158]);
        assert_eq!(pixel(13, 16), &[255, 255, 255, 255]);
        assert_eq!(pixel(22, 16), &[255, 255, 255, 92]);
    }

    /// Run with DESKWAY_WRITE_ICONS=1 to regenerate the browser icons and README logo.
    #[test]
    fn the_pngs_are_these_drawings() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let write = std::env::var_os("DESKWAY_WRITE_ICONS").is_some();
        let logo = colour_rgba(192);
        for (name, size, pixels) in [
            ("web/static/icons/icon-192.png", 192, &logo),
            ("web/static/icons/icon-512.png", 512, &colour_rgba(512)),
            ("web/static/icons/maskable-512.png", 512, &full_bleed_rgba(512, 1.0)),
            ("web/static/icons/apple-touch-icon.png", 180, &full_bleed_rgba(180, APP_SCALE)),
            ("docs/images/logo.png", 192, &logo),
        ] {
            let bytes = png(pixels, size);
            let path = root.join(name);
            if write {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &bytes).unwrap();
            }
            assert!(std::fs::read(&path).ok().as_deref() == Some(&bytes[..]), "{name} is not icon.rs's drawing: run this test with DESKWAY_WRITE_ICONS=1");
        }
    }
}
