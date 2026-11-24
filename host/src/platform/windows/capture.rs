use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use super::displays;
use crate::display::{self, DisplayInfo};
use crate::platform::Desk;
use crate::video::{fit, CaptureChanges, Capturer, CursorShape, Done, NoDisplay, Still, CURSOR_MAX};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

pub struct Duplication {
    frame: Vec<u8>,
    nv12: Vec<u8>,
    scaled: Vec<u8>,
    info: DisplayInfo,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    w: usize,
    h: usize,
    /// A display turned to portrait (or upside down) is duplicated as the panel scans it, not
    /// as the desktop stands: the staging texture has the panel's size and the image is turned
    /// upright when read.
    rotation: DXGI_MODE_ROTATION,
    tw: usize,
    th: usize,
    /// The staging texture holds a real desktop image.
    has_frame: bool,
    /// Duplication delivers nothing usable until the screen changes, so a GDI copy
    /// stands in for a static screen until then.
    seed: Option<Vec<u8>>,
}

/// Copies one display with GDI into tightly packed BGRA. The screen DC spans the whole
/// virtual desktop with the primary display at its origin.
fn gdi_capture(x: i32, y: i32, w: usize, h: usize) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let dc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
        let old = SelectObject(dc, bmp.into());
        let copied = BitBlt(dc, 0, 0, w as i32, h as i32, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w as i32;
        info.bmiHeader.biHeight = -(h as i32); // negative: rows run top to bottom
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut out = vec![0u8; w * h * 4];
        let rows = GetDIBits(dc, bmp, 0, h as u32, Some(out.as_mut_ptr() as _), &mut info, DIB_RGB_COLORS);
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        (copied && rows == h as i32).then_some(out)
    }
}

#[derive(Default)]
pub struct Pointer {
    visible: bool,
    x: i32,
    y: i32,
    shape: Vec<u8>,
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
}

/// Opens the wanted display, or the primary one when it is not attached. No attached output
/// at all is [`NoDisplay`].
fn open_display(want: Option<&str>) -> anyhow::Result<Duplication> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let ids = displays::stable_ids();
        let mut found = Vec::new();
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            for j in 0.. {
                let Ok(output) = adapter.EnumOutputs(j) else { break };
                let desc = output.GetDesc()?;
                if let Some(info) = displays::from_desc(&desc, &ids) {
                    found.push((info, adapter.clone(), output));
                }
            }
        }
        let infos: Vec<DisplayInfo> = found.iter().map(|f| f.0.clone()).collect();
        let Some(target) = display::choose(&infos, want).cloned() else {
            return Err(NoDisplay.into());
        };
        let (_, adapter, output) = found.into_iter().find(|f| f.0 == target).unwrap();
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(&adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))?;
        let (device, ctx): (ID3D11Device, _) = (device.unwrap(), ctx.unwrap());
        let out1: IDXGIOutput1 = output.cast()?;
        let dup = out1.DuplicateOutput(&device)?;
        let (w, h) = (target.w as usize, target.h as usize);
        let dd = dup.GetDesc();
        let rotation = dd.Rotation;
        let (tw, th) = match rotation {
            DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270 => (h, w),
            _ => (w, h),
        };
        let td = D3D11_TEXTURE2D_DESC {
            Width: tw as u32,
            Height: th as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        device.CreateTexture2D(&td, None, Some(&mut staging))?;
        let seed = gdi_capture(target.x, target.y, w, h);
        Ok(Duplication { frame: vec![], nv12: vec![], scaled: vec![], info: target, ctx, dup, staging: staging.unwrap(), w, h, rotation, tw, th, has_frame: false, seed })
    }
}

/// Waits for the next desktop update and reports what changed.
fn acquire(d: &mut Duplication, ptr: &mut Pointer, wait_ms: u32) -> windows::core::Result<CaptureChanges> {
    let mut changes = CaptureChanges::default();
    unsafe {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res = None;
        match d.dup.AcquireNextFrame(wait_ms, &mut info, &mut res) {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(changes),
            Err(e) => return Err(e),
        }
        if info.LastPresentTime != 0 {
            let tex: ID3D11Texture2D = res.unwrap().cast()?;
            d.ctx.CopyResource(&d.staging, &tex);
            d.has_frame = true;
            d.seed = None;
            changes.image = true;
        }
        if info.LastMouseUpdateTime != 0 {
            ptr.visible = info.PointerPosition.Visible.as_bool();
            ptr.x = info.PointerPosition.Position.x;
            ptr.y = info.PointerPosition.Position.y;
            changes.pointer = true;
        }
        if info.PointerShapeBufferSize > 0 {
            ptr.shape.resize(info.PointerShapeBufferSize as usize, 0);
            let mut used = 0;
            d.dup.GetFramePointerShape(info.PointerShapeBufferSize, ptr.shape.as_mut_ptr() as _, &mut used, &mut ptr.info)?;
            changes.shape = true;
            changes.pointer = true;
        }
        d.dup.ReleaseFrame()?;
    }
    Ok(changes)
}

/// Copies the latest image out of the staging texture with tightly packed rows.
fn read_frame(d: &mut Duplication) -> windows::core::Result<()> {
    let frame = &mut d.frame;
    if let Some(seed) = &d.seed {
        frame.clear();
        frame.extend_from_slice(seed);
        return Ok(());
    }
    unsafe {
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        d.ctx.Map(&d.staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        let row = d.w * 4;
        frame.resize(row * d.h, 0);
        let turn = match d.rotation {
            DXGI_MODE_ROTATION_ROTATE90 => Turn::Quarter,
            DXGI_MODE_ROTATION_ROTATE180 => Turn::Half,
            DXGI_MODE_ROTATION_ROTATE270 => Turn::ThreeQuarters,
            _ => Turn::None,
        };
        if turn == Turn::None {
            for y in 0..d.h {
                let src = (m.pData as *const u8).add(y * m.RowPitch as usize);
                std::ptr::copy_nonoverlapping(src, frame.as_mut_ptr().add(y * row), row);
            }
        } else {
            let src = std::slice::from_raw_parts(m.pData as *const u8, m.RowPitch as usize * (d.th - 1) + d.tw * 4);
            turn_upright(src, m.RowPitch as usize, d.tw, d.th, turn, frame, d.w, d.h);
        }
        d.ctx.Unmap(&d.staging, 0);
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Turn {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

/// Turns a panel-oriented image (`tw`×`th`, rows `pitch` bytes apart) into the desktop's
/// orientation (`w`×`h`, tightly packed), the way the display shows it.
fn turn_upright(src: &[u8], pitch: usize, tw: usize, th: usize, turn: Turn, dst: &mut [u8], w: usize, h: usize) {
    debug_assert!(match turn { Turn::Quarter | Turn::ThreeQuarters => tw == h && th == w, _ => tw == w && th == h });
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = match turn {
                Turn::None => (x, y),
                Turn::Quarter => (y, w - 1 - x),
                Turn::Half => (w - 1 - x, h - 1 - y),
                Turn::ThreeQuarters => (h - 1 - y, x),
            };
            let s = sy * pitch + sx * 4;
            let o = (y * w + x) * 4;
            dst[o..o + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
}

/// DXGI captures exclude the hardware cursor, so it is drawn into the frame here.
fn draw_pointer(frame: &mut [u8], w: usize, h: usize, p: &Pointer) {
    if !p.visible || p.shape.is_empty() {
        return;
    }
    let (sw, pitch) = (p.info.Width as usize, p.info.Pitch as usize);
    let typ = p.info.Type as i32;
    let mono = typ == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0;
    let sh = if mono { p.info.Height as usize / 2 } else { p.info.Height as usize };
    for sy in 0..sh {
        let dy = p.y + sy as i32;
        if dy < 0 || dy as usize >= h {
            continue;
        }
        for sx in 0..sw {
            let dx = p.x + sx as i32;
            if dx < 0 || dx as usize >= w {
                continue;
            }
            let o = (dy as usize * w + dx as usize) * 4;
            let Some(px) = frame.get_mut(o..o + 4) else { continue };
            if mono {
                let bit = 0x80u8 >> (sx % 8);
                let Some(&and) = p.shape.get(sy * pitch + sx / 8) else { continue };
                let Some(&xor) = p.shape.get((sy + sh) * pitch + sx / 8) else { continue };
                for c in &mut px[..3] {
                    if and & bit == 0 {
                        *c = 0;
                    }
                    if xor & bit != 0 {
                        *c ^= 0xff;
                    }
                }
            } else {
                let Some(s) = p.shape.get(sy * pitch + sx * 4..sy * pitch + sx * 4 + 4) else { continue };
                if typ == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 {
                    let a = s[3] as u32;
                    for c in 0..3 {
                        px[c] = ((s[c] as u32 * a + px[c] as u32 * (255 - a)) / 255) as u8;
                    }
                } else if s[3] == 0 {
                    px[..3].copy_from_slice(&s[..3]);
                } else {
                    for c in 0..3 {
                        px[c] ^= s[c];
                    }
                }
            }
        }
    }
}

/// The pointer shape as straight RGBA for a browser to draw, with its hotspot, scaled
/// down to at most `CURSOR_MAX` pixels a side. Pixels that invert the screen have no
/// RGBA equivalent and become black; the browser outlines the pointer so it stays visible.
fn pointer_rgba(p: &Pointer) -> Option<(usize, usize, i32, i32, Vec<u8>)> {
    if p.shape.is_empty() || p.info.Width == 0 {
        return None;
    }
    let (w, pitch) = (p.info.Width as usize, p.info.Pitch as usize);
    let typ = p.info.Type as i32;
    let mono = typ == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0;
    let h = if mono { p.info.Height as usize / 2 } else { p.info.Height as usize };
    if h == 0 {
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let px: [u8; 4] = if mono {
                let bit = 0x80u8 >> (x % 8);
                let and = p.shape.get(y * pitch + x / 8).copied().unwrap_or(0xff) & bit != 0;
                let xor = p.shape.get((y + h) * pitch + x / 8).copied().unwrap_or(0) & bit != 0;
                match (and, xor) {
                    (false, false) => [0, 0, 0, 255],
                    (false, true) => [255, 255, 255, 255],
                    (true, false) => [0, 0, 0, 0],
                    (true, true) => [0, 0, 0, 255],
                }
            } else {
                let o = y * pitch + x * 4;
                let Some(s) = p.shape.get(o..o + 4) else { continue };
                if typ == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 {
                    [s[2], s[1], s[0], s[3]]
                } else if s[3] == 0 {
                    [s[2], s[1], s[0], 255]
                } else if s[..3] == [0, 0, 0] {
                    [0, 0, 0, 0]
                } else {
                    [0, 0, 0, 255]
                }
            };
            rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&px);
        }
    }
    let (hx, hy) = (p.info.HotSpot.x, p.info.HotSpot.y);
    let f = w.max(h).div_ceil(CURSOR_MAX);
    if f <= 1 {
        return Some((w, h, hx, hy, rgba));
    }
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut small = vec![0u8; sw * sh * 4];
    for y in 0..sh {
        for x in 0..sw {
            let o = ((y * f).min(h - 1) * w + (x * f).min(w - 1)) * 4;
            small[(y * sw + x) * 4..(y * sw + x) * 4 + 4].copy_from_slice(&rgba[o..o + 4]);
        }
    }
    Some((sw, sh, hx / f as i32, hy / f as i32, small))
}

/// BGRA to NV12, BT.709 limited range.
fn bgra_to_nv12(src: &[u8], src_w: usize, w: usize, h: usize, out: &mut [u8]) {
    let stride = src_w * 4;
    let (yp, uvp) = out.split_at_mut(w * h);
    for row in 0..h {
        let s = &src[row * stride..row * stride + w * 4];
        let d = &mut yp[row * w..row * w + w];
        for (px, y) in s.chunks_exact(4).zip(d.iter_mut()) {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            *y = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
        }
    }
    for row in 0..h / 2 {
        let s0 = &src[row * 2 * stride..];
        let s1 = &src[(row * 2 + 1) * stride..];
        let d = &mut uvp[row * w..row * w + w];
        for col in 0..w / 2 {
            let (mut b, mut g, mut r) = (0i32, 0i32, 0i32);
            for s in [&s0[col * 8..col * 8 + 8], &s1[col * 8..col * 8 + 8]] {
                b += s[0] as i32 + s[4] as i32;
                g += s[1] as i32 + s[5] as i32;
                r += s[2] as i32 + s[6] as i32;
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            d[col * 2] = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128) as u8;
            d[col * 2 + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// Bilinear downscale across a few threads, since it runs on every frame.
fn scale_bgra(src: &[u8], sw: usize, sh: usize, out: &mut Vec<u8>, dw: usize, dh: usize) {
    out.resize(dw * dh * 4, 255);
    let xs: Vec<(usize, u32)> = (0..dw)
        .map(|x| {
            let pos = x * (sw - 1) * 256 / (dw - 1).max(1);
            ((pos >> 8).min(sw - 2), (pos - (pos >> 8).min(sw - 2) * 256) as u32)
        })
        .collect();
    let stride = sw * 4;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
    let rows_per_part = dh.div_ceil(threads);
    std::thread::scope(|scope| {
        for (part, chunk) in out.chunks_mut(rows_per_part * dw * 4).enumerate() {
            let xs = &xs;
            scope.spawn(move || {
                for (i, row) in chunk.chunks_exact_mut(dw * 4).enumerate() {
                    let pos = (part * rows_per_part + i) * (sh - 1) * 256 / (dh - 1).max(1);
                    let y0 = (pos >> 8).min(sh - 2);
                    let fy = (pos - y0 * 256) as u32;
                    let (top, bottom) = (&src[y0 * stride..(y0 + 1) * stride], &src[(y0 + 1) * stride..(y0 + 2) * stride]);
                    for (px, &(x0, fx)) in row.chunks_exact_mut(4).zip(xs) {
                        let o = x0 * 4;
                        for c in 0..3 {
                            let a = top[o + c] as u32 * (256 - fx) + top[o + 4 + c] as u32 * fx;
                            let b = bottom[o + c] as u32 * (256 - fx) + bottom[o + 4 + c] as u32 * fx;
                            px[c] = ((a * (256 - fy) + b * fy + 32768) >> 16) as u8;
                        }
                    }
                }
            });
        }
    });
}

/// A BGRA frame `w`×`h`, box-filtered down to at most `max` wide, as RGB.
fn still_of(frame: &[u8], w: usize, h: usize, max: usize) -> Option<Still> {
    let (tw, th) = fit(w, h, max)?;
    let mut rgb = Vec::with_capacity(tw * th * 3);
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let (mut b, mut g, mut r) = (0u32, 0u32, 0u32);
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let o = (y * w + x) * 4;
                    b += frame[o] as u32;
                    g += frame[o + 1] as u32;
                    r += frame[o + 2] as u32;
                    n += 1;
                }
            }
            rgb.extend_from_slice(&[(r / n) as u8, (g / n) as u8, (b / n) as u8]);
        }
    }
    Some(Still { rgb, w: tw, h: th })
}

impl Capturer for Duplication {
    type Frame = [u8];
    type Context = Pointer;

    fn open(want: Option<&str>, _: bool) -> anyhow::Result<Self> {
        open_display(want)
    }

    fn display(&self) -> &DisplayInfo {
        &self.info
    }

    fn displays() -> Vec<DisplayInfo> {
        displays::list()
    }

    fn input_desktop() -> Desk {
        super::desktop::input()
    }

    fn follow_input_desktop() {
        super::desktop::follow();
    }

    fn pixel_size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn wait(&mut self, pointer: &mut Pointer, timeout: Duration) -> anyhow::Result<CaptureChanges> {
        Ok(acquire(self, pointer, timeout.as_millis() as u32)?)
    }

    fn has_frame(&self) -> bool {
        self.has_frame || self.seed.is_some()
    }

    /// The duplicated display from its latest frame (the lock screen and UAC prompts too);
    /// another through GDI, which needs no second duplication and gives nothing back on a
    /// secure desktop.
    fn snapshot(&mut self, d: &DisplayInfo, max: usize, done: Done) {
        let still = if d.id == self.info.id {
            read_frame(self).ok().and_then(|_| still_of(&self.frame, self.w, self.h, max))
        } else {
            gdi_capture(d.x, d.y, d.w as usize, d.h as usize).and_then(|frame| still_of(&frame, d.w as usize, d.h as usize, max))
        };
        done(still);
    }

    fn frame(&mut self, pointer: &Pointer, draw_cursor: bool, (w, h): (usize, usize)) -> anyhow::Result<&Self::Frame> {
        read_frame(self)?;
        if draw_cursor {
            draw_pointer(&mut self.frame, self.w, self.h, pointer);
        }
        self.nv12.resize(w * h * 3 / 2, 0);
        if w == self.w & !1 && h == self.h & !1 {
            bgra_to_nv12(&self.frame, self.w, w, h, &mut self.nv12);
        } else {
            scale_bgra(&self.frame, self.w, self.h, &mut self.scaled, w, h);
            bgra_to_nv12(&self.scaled, w, w, h, &mut self.nv12);
        }
        Ok(&self.nv12)
    }

    fn cursor_shape(&self, pointer: &Pointer) -> Option<CursorShape> {
        let (w, h, hx, hy, rgba) = pointer_rgba(pointer)?;
        Some(CursorShape { w, h, hx, hy, rgba })
    }

    fn cursor_position(&self, pointer: &Pointer) -> (f64, f64, bool) {
        let x = (pointer.x + pointer.info.HotSpot.x) as f64 / self.w.max(1) as f64;
        let y = (pointer.y + pointer.info.HotSpot.y) as f64 / self.h.max(1) as f64;
        let visible = pointer.visible && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y);
        (x, y, visible)
    }
}

/// Counts border-coloured pixels along the top row, once per stage, for `border-test`.
pub fn border_pixel_test(stages: Receiver<u8>, done: Sender<()>) -> String {
    let count = |d: &mut Duplication| -> usize {
        let mut pointer = Pointer::default();
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until {
            let _ = acquire(d, &mut pointer, 50);
        }
        if read_frame(d).is_err() {
            return 0;
        }
        let n = d.frame[..d.w * 4].chunks_exact(4).filter(|p| p[0] == 0x2A && p[1] == 0x61 && p[2] == 0xA8).count();
        d.frame.iter_mut().for_each(|b| *b = 0);
        n
    };
    let Ok(mut d) = open_display(None) else { return "capture unavailable".into() };
    let mut results = Vec::new();
    while let Ok(stage) = stages.recv_timeout(Duration::from_secs(10)) {
        results.push(count(&mut d));
        let _ = done.send(());
        if stage == 1 {
            break;
        }
    }
    let visible = results.first().copied().unwrap_or(0);
    let excluded = results.get(1).copied().unwrap_or(0);
    format!("top row width {}: border pixels captured without exclusion = {visible}, with exclusion = {excluded}", d.w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_keeps_corners_and_interpolates() {
        // A horizontal ramp 0, 100, 200 on two rows.
        let row: Vec<u8> = [0u8, 100, 200].iter().flat_map(|v| [*v, *v, *v, 255]).collect();
        let src = [row.clone(), row].concat();
        let mut out = Vec::new();
        scale_bgra(&src, 3, 2, &mut out, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!((out[0], out[4]), (0, 200));
        scale_bgra(&src, 3, 2, &mut out, 3, 1);
        assert_eq!((out[0], out[4], out[8]), (0, 100, 200));
    }

    #[test]
    fn portrait_panels_are_turned_upright() {
        // Desktop 2 wide, 3 tall; pixel value = 10*y + x (first byte).
        let want: Vec<u8> = (0..3).flat_map(|y| (0..2).map(move |x| (10 * y + x) as u8)).collect();
        let px = |v: &[u8]| v.chunks(4).map(|c| c[0]).collect::<Vec<_>>();
        let mk = |tw: usize, th: usize, f: &dyn Fn(usize, usize) -> usize| -> Vec<u8> {
            (0..th).flat_map(|ty| (0..tw).flat_map(move |tx| [f(tx, ty) as u8, 0, 0, 0])).collect::<Vec<_>>()
        };
        // Texture pixel (tx, ty) holds the desktop pixel that lands there on the panel.
        let q = mk(3, 2, &|tx, ty| 10 * tx + (1 - ty));
        let three = mk(3, 2, &|tx, ty| 10 * (2 - tx) + ty);
        let half = mk(2, 3, &|tx, ty| 10 * (2 - ty) + (1 - tx));
        for (src, tw, th, turn) in [(&q, 3, 2, Turn::Quarter), (&three, 3, 2, Turn::ThreeQuarters), (&half, 2, 3, Turn::Half)] {
            let mut out = vec![0u8; 2 * 3 * 4];
            turn_upright(src, tw * 4, tw, th, turn, &mut out, 2, 3);
            assert_eq!(px(&out), want, "{turn:?}");
        }
    }

    #[test]
    fn nv12_black_and_white() {
        let (w, h) = (4, 2);
        let mut src = vec![255u8; w * h * 4];
        src[..4].copy_from_slice(&[0, 0, 0, 255]);
        let mut out = vec![0u8; w * h * 3 / 2];
        bgra_to_nv12(&src, w, w, h, &mut out);
        assert_eq!(out[0], 16, "black maps to limited-range Y 16");
        assert_eq!(out[1], 235, "white maps to limited-range Y 235");
        assert_eq!(out[w * h + 2], 128, "white has neutral chroma");
    }

    #[test]
    fn colour_pointer_is_blended_and_clipped() {
        let mut p = Pointer { visible: true, x: 1, y: 0, shape: vec![10, 20, 30, 255, 0, 0, 0, 0], info: DXGI_OUTDUPL_POINTER_SHAPE_INFO::default() };
        p.info.Type = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32;
        p.info.Width = 2;
        p.info.Height = 1;
        p.info.Pitch = 8;
        let mut frame = vec![100u8; 2 * 4];
        draw_pointer(&mut frame, 2, 1, &p);
        assert_eq!(&frame[..4], &[100, 100, 100, 100], "pixel left of the pointer untouched");
        assert_eq!(&frame[4..7], &[10, 20, 30], "opaque pointer pixel replaces the image");
    }

    #[test]
    fn a_still_is_box_filtered_down_to_its_width_as_rgb() {
        // Left half blue, right half red (BGRA); halved, each side keeps its colour.
        let mut frame = vec![0u8; 4 * 2 * 4];
        for (i, px) in frame.chunks_mut(4).enumerate() {
            px.copy_from_slice(if i % 4 < 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
        }
        let s = still_of(&frame, 4, 2, 2).unwrap();
        assert_eq!((s.w, s.h), (2, 1));
        assert_eq!(s.rgb, vec![0, 0, 255, 255, 0, 0]);
    }
    #[test]
    fn mono_pointer_becomes_rgba_with_hotspot() {
        // 8x1 monochrome: AND row then XOR row. Pixels: black, white, transparent, inverted.
        let mut p = Pointer { visible: true, x: 0, y: 0, shape: vec![0b0011_1111, 0b0101_0000], info: DXGI_OUTDUPL_POINTER_SHAPE_INFO::default() };
        p.info.Type = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
        p.info.Width = 8;
        p.info.Height = 2;
        p.info.Pitch = 1;
        p.info.HotSpot.x = 3;
        let (w, h, hx, hy, rgba) = pointer_rgba(&p).unwrap();
        assert_eq!((w, h, hx, hy), (8, 1, 3, 0));
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255], "AND 0 XOR 0 is black");
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255], "AND 0 XOR 1 is white");
        assert_eq!(rgba[11], 0, "AND 1 XOR 0 is transparent");
        assert_eq!(&rgba[12..16], &[0, 0, 0, 255], "AND 1 XOR 1 inverts, shown as black");
    }

    #[test]
    fn big_colour_pointer_is_scaled_down() {
        let mut p = Pointer { visible: true, x: 0, y: 0, shape: vec![0; 256 * 256 * 4], info: DXGI_OUTDUPL_POINTER_SHAPE_INFO::default() };
        p.info.Type = DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32;
        p.info.Width = 256;
        p.info.Height = 256;
        p.info.Pitch = 256 * 4;
        p.info.HotSpot.x = 100;
        p.info.HotSpot.y = 40;
        // Colour shapes are BGRA; the browser wants RGBA.
        p.shape[..4].copy_from_slice(&[10, 20, 30, 200]);
        let (w, h, hx, hy, rgba) = pointer_rgba(&p).unwrap();
        assert_eq!((w, h, hx, hy), (64, 64, 25, 10));
        assert_eq!(&rgba[..4], &[30, 20, 10, 200]);
    }
}
