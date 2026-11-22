// Deskway's two icons, drawn in code so every size is sharp, in the accent on white:
// - Deskway Host (this program): a tile with a monitor on its stand, the computer being
//   controlled. The tray, the panel window, the taskbar, and (build.rs makes the .ico from it)
//   the program file.
// - Deskway (the browser client): a rounded window with a pointer in it. Only the tests draw it,
//   writing the web client's icons (see the end).
// Plain Rust with no dependencies, so build.rs can include it too.

/// The accent (design.css --accent).
const TILE: [u8; 3] = [0x25, 0x63, 0xEB];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];

fn rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}


/// The host's glyph in the unit square: a monitor's frame, its neck and its foot.
fn monitor(x: f32, y: f32) -> bool {
    let frame = rounded_rect(x, y, 0.15, 0.21, 0.85, 0.64, 0.07) && !rounded_rect(x, y, 0.235, 0.295, 0.765, 0.555, 0.02);
    frame || rounded_rect(x, y, 0.45, 0.635, 0.55, 0.735, 0.0) || rounded_rect(x, y, 0.3, 0.72, 0.7, 0.79, 0.034)
}

/// What covers a point of the unit square: None (transparent) or the tile with the monitor.
fn sample(x: f32, y: f32) -> Option<[u8; 3]> {
    rounded_rect(x, y, 0.02, 0.02, 0.98, 0.98, 0.2).then(|| if monitor(x, y) { WHITE } else { TILE })
}

/// Straight RGBA pixels of the host's icon. A template keeps only the white monitor.
pub fn rgba(size: u32, template: bool) -> Vec<u8> {
    draw(size, |x, y| sample(x, y).filter(|color| !template || *color == WHITE))
}

fn draw(size: u32, sample_pixel: impl Fn(f32, f32) -> Option<[u8; 3]>) -> Vec<u8> {
    const N: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..N {
                for sx in 0..N {
                    let x = (px as f32 + (sx as f32 + 0.5) / N as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / N as f32) / size as f32;
                    if let Some(c) = sample_pixel(x, y) {
                        r += c[0] as u32;
                        g += c[1] as u32;
                        b += c[2] as u32;
                        a += 1;
                    }
                }
            }
            let o = ((py * size + px) * 4) as usize;
            if a > 0 {
                out[o] = (r / a) as u8;
                out[o + 1] = (g / a) as u8;
                out[o + 2] = (b / a) as u8;
                out[o + 3] = (a * 255 / (N * N)) as u8;
            }
        }
    }
    out
}

/// The browser client's icon, drawn only by the tests (they write web/static/icons).
#[cfg(test)]
mod client {
    use super::*;

    /// Even-odd point-in-polygon test.
    fn polygon(x: f32, y: f32, p: &[(f32, f32)]) -> bool {
        let mut inside = false;
        let mut j = p.len() - 1;
        for i in 0..p.len() {
            let ((xi, yi), (xj, yj)) = (p[i], p[j]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    /// A mouse pointer, tip at the top left, as on every desktop; in the glyph's box.
    const POINTER: [(f32, f32); 7] = [(0.343, 0.265), (0.343, 0.735), (0.461, 0.633), (0.539, 0.804), (0.622, 0.767), (0.543, 0.598), (0.696, 0.59)];

    /// The glyph in its own box, whose sides are the window's outer edges: the window's frame
    /// and the pointer in it.
    fn glyph(x: f32, y: f32) -> bool {
        let frame = rounded_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.265) && !rounded_rect(x, y, 0.098, 0.098, 0.902, 0.902, 0.167);
        frame || polygon(x, y, &POINTER)
    }

    /// Whether the glyph, centred, its box `scale` of the square's side, covers the point.
    fn placed(x: f32, y: f32, scale: f32) -> bool {
        glyph((x - 0.5) / scale + 0.5, (y - 0.5) / scale + 0.5)
    }

    /// For the browser's own uses (its install dialog, the start screen while the app opens, a
    /// desktop's taskbar): the glyph in the accent on nothing.
    pub fn plain_rgba(size: u32) -> Vec<u8> {
        draw(size, |x, y| placed(x, y, 0.8).then_some(TILE))
    }

    /// For systems that cut their own shape out of the icon: the accent fills the square, the
    /// white glyph `scale` of it. Android may cut a circle, so its glyph stays inside the
    /// circle every mask keeps (66% of the side); the iPhone rounds the corners only.
    pub const IN_ANY_MASK: f32 = 0.47;
    pub const ON_TILE: f32 = 0.62;
    pub fn full_bleed_rgba(size: u32, scale: f32) -> Vec<u8> {
        draw(size, |x, y| Some(if placed(x, y, scale) { WHITE } else { TILE }))
    }
}

#[cfg(test)]
mod tests {
    use super::client::*;

    fn png(rgba: &[u8], size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, size, size);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(rgba).unwrap();
        out
    }

    /// The web client's icons (web/static/icons) are these drawings, never edited by hand. After
    /// changing the icon, run this test with DESKWAY_WRITE_ICONS=1 to write them again.
    #[test]
    fn the_web_icons_are_these_drawings() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/static/icons");
        let write = std::env::var_os("DESKWAY_WRITE_ICONS").is_some();
        for (name, size, pixels) in [
            ("icon-192.png", 192, plain_rgba(192)),
            ("icon-512.png", 512, plain_rgba(512)),
            ("maskable-512.png", 512, full_bleed_rgba(512, IN_ANY_MASK)),
            ("apple-touch-icon.png", 180, full_bleed_rgba(180, ON_TILE)),
        ] {
            let bytes = png(&pixels, size);
            let path = dir.join(name);
            if write {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, &bytes).unwrap();
            }
            assert!(std::fs::read(&path).ok().as_deref() == Some(&bytes[..]), "{name} is not icon.rs's drawing: run this test with DESKWAY_WRITE_ICONS=1");
        }
    }
}
