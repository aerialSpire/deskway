//! Lossless 64-pixel updates for a viewer with very little bandwidth.

use std::time::{Duration, Instant};

use anyhow::ensure;

use super::Still;

const SIDE: usize = 64;
const MAX_SIDE: usize = 8192;
const MAX_PIXELS: usize = 8192 * 4320;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Tiles {
    pub epoch: u32,
    revision: u32,
    sampled: Option<Instant>,
    size: (usize, usize),
    width: usize,
    latest: Vec<Option<Vec<u8>>>,
    sent: Vec<Option<Vec<u8>>>,
    next: usize,
    prefer_pointer: bool,
}

impl Tiles {
    pub fn new(epoch: u32) -> Self {
        Self {
            epoch, revision: 0, sampled: None, size: (0, 0), width: 0,
            latest: Vec::new(), sent: Vec::new(), next: 0, prefer_pointer: true,
        }
    }

    pub fn reset(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.sampled = None;
        self.size = (0, 0);
        self.latest.clear();
        self.sent.clear();
        self.next = 0;
        self.prefer_pointer = true;
    }

    pub fn can_sample(&self) -> bool {
        self.sampled.is_none_or(|t| t.elapsed() >= SAMPLE_INTERVAL)
    }

    pub fn sample(&mut self, image: Still) -> anyhow::Result<()> {
        let Still { rgb, w, h } = image;
        ensure!(w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE && w * h <= MAX_PIXELS, "unsupported screen size");
        ensure!(rgb.len() == w * h * 3, "invalid RGB frame");
        if self.size != (w, h) {
            self.reset();
            self.size = (w, h);
            self.width = w.div_ceil(SIDE);
            let count = self.width * h.div_ceil(SIDE);
            self.latest.resize_with(count, || None);
            self.sent.resize_with(count, || None);
        }

        for i in 0..self.sent.len() {
            let (x, y, tw, th) = self.rect(i);
            let rows = || {
                (y..y + th).map(|row| {
                    let first = (row * w + x) * 3;
                    &rgb[first..first + tw * 3]
                })
            };
            self.latest[i] = if self.sent[i].as_ref().is_some_and(|sent| sent.chunks_exact(tw * 3).eq(rows())) {
                None
            } else {
                let mut pixels = self.latest[i].take().unwrap_or_default();
                pixels.clear();
                pixels.extend(rows().flatten().copied());
                Some(pixels)
            };
        }
        self.sampled = Some(Instant::now());
        Ok(())
    }

    fn rect(&self, i: usize) -> (usize, usize, usize, usize) {
        let (x, y) = (i % self.width * SIDE, i / self.width * SIDE);
        (x, y, SIDE.min(self.size.0 - x), SIDE.min(self.size.1 - y))
    }

    pub fn next(&mut self, px: f64, py: f64) -> anyhow::Result<Option<Vec<u8>>> {
        let (w, h) = self.size;
        let pending = || {
            (self.next..self.latest.len()).chain(0..self.next).filter(|&i| self.latest[i].is_some())
        };
        // Send the area near the pointer promptly, with every other slot advancing the scan
        // across the entire display, including while the pointer area keeps changing.
        let i = if self.prefer_pointer {
            pending().min_by_key(|&i| {
                let (x, y, tw, th) = self.rect(i);
                (x + tw / 2).abs_diff((px.clamp(0.0, 1.0) * w as f64) as usize)
                    + (y + th / 2).abs_diff((py.clamp(0.0, 1.0) * h as f64) as usize)
            })
        } else {
            pending().next()
        };
        let Some(i) = i else { return Ok(None) };
        let (x, y, tw, th) = self.rect(i);
        let pixels = self.latest[i].take().expect("selected a changed tile");
        let mut packet = self.revision.to_le_bytes().to_vec();
        for field in [w, h, x, y] {
            packet.extend_from_slice(&(field as u16).to_le_bytes());
        }
        {
            let mut encoder = png::Encoder::new(&mut packet, tw as u32, th as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
            encoder.write_header()?.write_image_data(&pixels)?;
        }
        self.sent[i] = Some(pixels);
        if !self.prefer_pointer {
            self.next = (i + 1) % self.latest.len();
        }
        self.prefer_pointer = !self.prefer_pointer;
        Ok(Some(packet))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_original_pixels_and_only_changes_after_the_initial_screen() {
        let mut tiles = Tiles::new(1);
        let image = |rgb| Still { rgb, w: 66, h: 2 };
        let mut pixels = vec![255; 66 * 2 * 3];
        tiles.sample(image(pixels.clone())).unwrap();
        let right = tiles.next(1.0, 0.0).unwrap().unwrap();
        assert_eq!(&right[8..12], &[64, 0, 0, 0]);
        assert!(tiles.next(1.0, 0.0).unwrap().is_some());
        assert!(tiles.next(1.0, 0.0).unwrap().is_none());
        pixels[..3].copy_from_slice(&[23, 41, 97]);
        tiles.sample(image(pixels.clone())).unwrap();
        let left = tiles.next(1.0, 0.0).unwrap().unwrap();
        assert_eq!(&left[8..12], &[0; 4]);
        let mut reader = png::Decoder::new(&left[12..]).read_info().unwrap();
        let mut restored = vec![0; reader.output_buffer_size()];
        assert_eq!(reader.next_frame(&mut restored).unwrap().width, 64);
        assert_eq!(&restored[..3], &[23, 41, 97]);
        assert!(tiles.next(1.0, 0.0).unwrap().is_none());

        tiles.reset();
        tiles.sample(image(pixels)).unwrap();
        assert_ne!(&tiles.next(0.0, 0.0).unwrap().unwrap()[..4], &left[..4]);
    }
}
