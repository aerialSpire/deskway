//! Generates the AppKit iconset from the same artwork as the Windows application icon.

#[path = "../src/icon.rs"]
mod icon;

use std::fs::File;
use std::path::PathBuf;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let dir = std::env::args_os().nth(1).map(PathBuf::from).context("specify the iconset directory")?;
    for size in [16, 32, 128, 256, 512] {
        for (scale, suffix) in [(1, ""), (2, "@2x")] {
            let pixels = size * scale;
            let file = File::create(dir.join(format!("icon_{size}x{size}{suffix}.png")))?;
            let mut encoder = png::Encoder::new(file, pixels, pixels);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&icon::rgba(pixels, false))?;
        }
    }
    Ok(())
}
