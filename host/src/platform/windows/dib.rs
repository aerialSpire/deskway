//! The bitmap forms Windows programs read and write on the clipboard (`CF_DIB`, `CF_DIBV5`).

use crate::clipboard::image::PIXELS_MAX;

/// A 32-bit bottom-up DIB, as `CF_DIB` holds it.
pub fn rgba_to_dib(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(40 + rgba.len());
    for v in [40u32, w, h] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, w * h * 4, 0, 0, 0, 0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    for y in (0..h as usize).rev() {
        for p in rgba[y * w as usize * 4..(y + 1) * w as usize * 4].chunks(4) {
            d.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    d
}

/// Width and height of a `CF_DIB`/`CF_DIBV5` bitmap, from its header.
pub fn dib_size(dib: &[u8]) -> Option<(u32, u32)> {
    let w = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let h = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    (w > 0 && h != 0).then(|| (w as u32, h.unsigned_abs()))
}

/// A `CF_DIB` or `CF_DIBV5` bitmap (24 or 32 bits, plain or with bit fields) as RGBA, top row
/// first. Other depths (palettes) are not taken.
pub fn dib_to_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let header = u32_at(0)? as usize;
    let (w, h) = dib_size(dib)?;
    if header < 40 || (w as u64) * (h as u64) > PIXELS_MAX {
        return None;
    }
    let top_down = i32::from_le_bytes(dib[8..12].try_into().ok()?) < 0;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    // Channel masks: inside a V4/V5 header, else after a plain one.
    let (masks, offset) = match compression {
        BI_RGB => ([0x00ff_0000, 0x0000_ff00, 0x0000_00ff, if bits == 32 { 0xff00_0000 } else { 0 }], header),
        BI_BITFIELDS if bits == 32 && header >= 56 => ([u32_at(40)?, u32_at(44)?, u32_at(48)?, u32_at(52)?], header),
        BI_BITFIELDS if bits == 32 => ([u32_at(40)?, u32_at(44)?, u32_at(48)?, 0], header + 12),
        _ => return None,
    };
    if bits != 24 && bits != 32 {
        return None;
    }
    let stride = ((w as usize * bits as usize + 31) / 32) * 4;
    let data = dib.get(offset..offset + stride * h as usize)?;
    let channel = |px: u32, mask: u32| -> u8 { if mask == 0 { 0 } else { ((px & mask) >> mask.trailing_zeros()) as u8 } };
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let row = &data[(if top_down { y } else { h as usize - 1 - y }) * stride..][..stride];
        for x in 0..w as usize {
            let px = if bits == 32 {
                u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap())
            } else {
                u32::from_le_bytes([row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 0])
            };
            out.extend_from_slice(&[channel(px, masks[0]), channel(px, masks[1]), channel(px, masks[2]), if masks[3] == 0 { 255 } else { channel(px, masks[3]) }]);
        }
    }
    // Many programs leave alpha at zero in 32-bit bitmaps: such a picture is opaque.
    if masks[3] != 0 && out.chunks(4).all(|p| p[3] == 0) {
        out.chunks_mut(4).for_each(|p| p[3] = 255);
    }
    Some((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::image::{png_decode, png_encode, png_size};

    #[test]
    fn received_images_become_bottom_up_bitmaps() {
        let rgba: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 10, 100, 200, 255]).collect();
        let png = png_encode(3, 2, &rgba).unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        let (w, h, back) = png_decode(&png).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(back, rgba);
        let dib = rgba_to_dib(w, h, &back);
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        assert_eq!(u32::from_le_bytes(dib[4..8].try_into().unwrap()), 3);
        // The first row stored is the bottom one, as blue, green, red, alpha.
        assert_eq!(&dib[40..44], &[200, 100, 30, 255]);
        assert!(png_size(b"not a png").is_none());
    }

    #[test]
    fn bitmaps_copied_by_programs_become_rgba() {
        let rgba: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 10, 100, 200, 255]).collect();
        // What this host writes reads back the same.
        let dib = rgba_to_dib(3, 2, &rgba);
        assert_eq!(dib_size(&dib), Some((3, 2)));
        assert_eq!(dib_to_rgba(&dib), Some((3, 2, rgba.clone())));
        // 24 bits, top-down, rows padded to four bytes; and 32 bits with alpha left at zero.
        let mut d24 = Vec::new();
        for v in [40u32, 1] {
            d24.extend_from_slice(&v.to_le_bytes());
        }
        d24.extend_from_slice(&(-2i32).to_le_bytes());
        d24.extend_from_slice(&1u16.to_le_bytes());
        d24.extend_from_slice(&24u16.to_le_bytes());
        d24.extend_from_slice(&[0; 24]);
        d24.extend_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        assert_eq!(dib_to_rgba(&d24), Some((1, 2, vec![3, 2, 1, 255, 6, 5, 4, 255])));
        let mut zero_alpha = rgba_to_dib(1, 1, &[9, 8, 7, 0]);
        zero_alpha.truncate(44);
        assert_eq!(dib_to_rgba(&zero_alpha), Some((1, 1, vec![9, 8, 7, 255])));
        // Bit fields after a plain header (as BGRA masks).
        let mut bf = rgba_to_dib(1, 1, &[1, 2, 3, 4]);
        bf[16..20].copy_from_slice(&3u32.to_le_bytes());
        let px = bf.split_off(40);
        for m in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff] {
            bf.extend_from_slice(&m.to_le_bytes());
        }
        bf.extend_from_slice(&px);
        assert_eq!(dib_to_rgba(&bf), Some((1, 1, vec![1, 2, 3, 255])));
        assert_eq!(dib_to_rgba(&dib[..50]), None, "cut short");
    }
}
