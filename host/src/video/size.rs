//! The encoded size: the viewer's constraints, within the H.264 level the stream carries.
//!
//! Pure logic, no platform code. The viewer says how much it shows (its viewport in device
//! pixels) and how much it decodes smoothly (MediaCapabilities); the SDP's `profile-level-id`
//! says which H.264 level it accepts before it has said more. The result keeps the source's
//! aspect ratio, has even dimensions (4:2:0 chroma), and fits the H.264 level it is encoded at.

/// The most frames a second the capture loop sends. A viewer asks for fewer to get each one
/// sharper (the browser's Sharper picture asks for 2).
pub const FPS: u32 = 24;

/// One row of H.264 Table A-1 (ITU-T H.264, Annex A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// `level_idc` as written in the SPS; level 1b is 9 (its High-profile coding).
    pub idc: u8,
    /// Macroblocks per second.
    max_mbps: u64,
    /// Macroblocks per frame.
    max_fs: u64,
    /// Video bit rate, in units of 1000 bits/s (Baseline/Main; High is 1.25 times).
    max_br: u64,
}

const fn level(idc: u8, max_mbps: u64, max_fs: u64, max_br: u64) -> Level {
    Level { idc, max_mbps, max_fs, max_br }
}

/// Table A-1, from the lowest level to the highest.
pub const LEVELS: [Level; 20] = [
    level(10, 1_485, 99, 64),
    level(9, 1_485, 99, 128),
    level(11, 3_000, 396, 192),
    level(12, 6_000, 396, 384),
    level(13, 11_880, 396, 768),
    level(20, 11_880, 396, 2_000),
    level(21, 19_800, 792, 4_000),
    level(22, 20_250, 1_620, 4_000),
    level(30, 40_500, 1_620, 10_000),
    level(31, 108_000, 3_600, 14_000),
    level(32, 216_000, 5_120, 20_000),
    level(40, 245_760, 8_192, 20_000),
    level(41, 245_760, 8_192, 50_000),
    level(42, 522_240, 8_704, 50_000),
    level(50, 589_824, 22_080, 135_000),
    level(51, 983_040, 36_864, 240_000),
    level(52, 2_073_600, 36_864, 240_000),
    level(60, 4_177_920, 139_264, 240_000),
    level(61, 8_355_840, 139_264, 480_000),
    level(62, 16_711_680, 139_264, 800_000),
];

fn rank(l: Level) -> usize {
    LEVELS.iter().position(|x| x.idc == l.idc).unwrap_or(0)
}

fn macroblocks(px: usize) -> u64 {
    px.div_ceil(16) as u64
}

impl Level {
    /// The level with this `level_idc`.
    pub fn from_idc(idc: u8) -> Option<Level> {
        LEVELS.iter().copied().find(|l| l.idc == idc)
    }

    /// Whether a `w`×`h` stream at `fps` conforms to this level: frame size, macroblock rate,
    /// and each side at most sqrt(8 × MaxFS) macroblocks (A.3.1 f, g; A.3.2 a).
    pub fn holds(self, w: usize, h: usize, fps: u32) -> bool {
        let (mw, mh) = (macroblocks(w), macroblocks(h));
        let side = ((8 * self.max_fs) as f64).sqrt() as u64;
        mw * mh <= self.max_fs && mw * mh * u64::from(fps) <= self.max_mbps && mw <= side && mh <= side
    }

    /// The lowest level that holds a `w`×`h` stream at `fps`.
    pub fn lowest_for(w: usize, h: usize, fps: u32) -> Option<Level> {
        LEVELS.iter().copied().find(|l| l.holds(w, h, fps))
    }

    /// The most a High-profile stream at this level may carry, in bits per second
    /// (cpbBrVclFactor 1250, Table A-2).
    pub fn max_bitrate(self) -> u64 {
        self.max_br * 1250
    }

    fn min(self, other: Level) -> Level {
        if rank(self) <= rank(other) { self } else { other }
    }
}

/// `profile-level-id` (RFC 6184 8.1): profile_idc, the constraint flags, level_idc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileLevelId {
    pub profile_idc: u8,
    pub constraints: u8,
    pub level: Level,
}

impl ProfileLevelId {
    /// Parses the six hex digits. Level 1b is level_idc 11 with constraint_set3 in Baseline,
    /// Main and Extended, and level_idc 9 elsewhere.
    pub fn parse(hex: &str) -> Option<ProfileLevelId> {
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let (profile_idc, constraints, idc) = (byte(0)?, byte(2)?, byte(4)?);
        let one_b = idc == 11 && matches!(profile_idc, 66 | 77 | 88) && constraints & 0x10 != 0;
        let level = Level::from_idc(if one_b { 9 } else { idc })?;
        Some(ProfileLevelId { profile_idc, constraints, level })
    }

    /// The `profile-level-id` of payload type `pt` in an SDP. Absent, RFC 6184 says 42000A
    /// (Baseline, level 1.0) is meant.
    pub fn from_sdp(sdp: &str, pt: u8) -> Option<ProfileLevelId> {
        let prefix = format!("a=fmtp:{pt} ");
        let line = sdp.lines().find_map(|l| l.trim_end().strip_prefix(&prefix));
        let Some(params) = line else { return ProfileLevelId::parse("42000a") };
        let value = params.split(';').map(str::trim).find_map(|p| {
            let (k, v) = p.split_once('=')?;
            k.trim().eq_ignore_ascii_case("profile-level-id").then(|| v.trim())
        });
        ProfileLevelId::parse(value.unwrap_or("42000a"))
    }
}

/// What the viewer accepts. Zero means "not known".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Constraints {
    /// Viewport, device pixels.
    pub w: u32,
    pub h: u32,
    /// Largest size the viewer decodes smoothly (MediaCapabilities), as a box.
    pub max_w: u32,
    pub max_h: u32,
    /// Frames a second the viewer takes: what it decodes smoothly at that size, or fewer when
    /// the person prefers a sharper picture to a smoother one.
    pub fps: u32,
    /// `level_idc` from the negotiated `profile-level-id`.
    pub level: u8,
}

/// How a source is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub w: usize,
    pub h: usize,
    pub fps: u32,
    /// The level written into the stream: the lowest that holds it.
    pub level: Level,
    /// What this size can use; the network estimate lowers it from here.
    pub max_bitrate: u32,
}

/// The smallest short side encoded when the source is larger: every H.264 encoder takes it.
pub const MIN_SHORT: usize = 240;

/// Floors to an even number, at least 2: 4:2:0 chroma needs even dimensions. The encoder pads
/// to whole macroblocks itself and crops them away in the SPS.
fn even(x: f64) -> usize {
    // The small margin keeps 1079.9999 from becoming 1078.
    (((x + 1e-6) as usize) & !1).max(2)
}

fn scaled((sw, sh): (usize, usize), s: f64) -> (usize, usize) {
    (even(sw as f64 * s).min(sw & !1).max(2), even(sh as f64 * s).min(sh & !1).max(2))
}

/// The Kush gauge (pixels × fps × motion 1 × 0.07): the bit rate a picture of this size needs
/// at low motion, which is what a desktop is.
fn kush_gauge(w: usize, h: usize, fps: u32) -> u64 {
    (w * h) as u64 * u64::from(fps) * 7 / 100
}

/// The most bits one frame takes: 0.4 a pixel keeps a screen of text sharp in a single frame
/// (a 3840×2160 frame up to about 410 KB).
fn sharp_frame_bits(w: usize, h: usize) -> u64 {
    (w * h) as u64 * 2 / 5
}

/// What a stream of this size can use. The ceiling is the Kush gauge at the full frame rate,
/// whatever rate the viewer takes: a viewer taking fewer frames gets each one larger, up to
/// `sharp_frame_bits`. At 24 frames a second that is the gauge; at 2 each frame is sharp.
fn stream_bitrate(w: usize, h: usize, fps: u32, level: Level) -> u32 {
    kush_gauge(w, h, FPS).min(sharp_frame_bits(w, h) * u64::from(fps)).min(level.max_bitrate()).min(u64::from(u32::MAX)) as u32
}

/// The encoded size for a `source` (pixels) under `c`, at a level no higher than the encoder's
/// `encoder_max`.
///
/// - The viewport: the source is fitted into it (no larger than it is shown).
/// - The decode box: fitted into it as well, turned like the source (decoder limits are by
///   area and side, not orientation). The viewer has then vouched for the level that box needs,
///   so the SDP's level no longer limits (browsers offer a fixed level there: Chrome 3.1).
/// - Without a decode box, the SDP's level is the limit.
/// - Never above the source: nothing is scaled up.
pub fn choose(source: (usize, usize), c: &Constraints, encoder_max: Level) -> Encoding {
    let (sw, sh) = (source.0.max(2), source.1.max(2));
    let fps = if c.fps > 0 { c.fps.min(FPS) } else { FPS };
    // The scale that fits the source into a box (0 = not known: no limit).
    let fit = |bw: u32, bh: u32| if bw > 0 && bh > 0 { (bw as f64 / sw as f64).min(bh as f64 / sh as f64) } else { f64::INFINITY };
    // The viewer's limits: the viewport, and the decode box turned like the source.
    let mut s = fit(c.w, c.h);
    let cap = if c.max_w > 0 && c.max_h > 0 {
        let (long, short) = (c.max_w.max(c.max_h), c.max_w.min(c.max_h));
        s = s.min(if sw >= sh { fit(long, short) } else { fit(short, long) });
        encoder_max
    } else {
        Level::from_idc(c.level).map_or(encoder_max, |l| l.min(encoder_max))
    };
    // Whatever the viewer reports, never below MIN_SHORT on the short side (as far as the source
    // goes) and never above the source: a viewer reporting almost nothing (a page being hidden,
    // an older page that copies a tiny viewport into its decode box) still gets a picture every
    // encoder takes.
    let floor = (MIN_SHORT as f64 / sw.min(sh) as f64).min(1.0);
    let mut s = s.max(floor).min(1.0);
    // First guess from the frame-size limit, then down two pixels at a time to the exact fit.
    let (w, h) = scaled((sw, sh), s);
    if !cap.holds(w, h, fps) {
        let mbs = cap.max_fs.min(cap.max_mbps / u64::from(fps)) as f64;
        s = s.min((mbs * 256.0 / (w * h) as f64).sqrt() * s);
    }
    let (mut w, mut h) = scaled((sw, sh), s);
    while !cap.holds(w, h, fps) && w > 2 {
        s = (w - 2) as f64 / sw as f64;
        (w, h) = scaled((sw, sh), s);
    }
    let level = Level::lowest_for(w, h, fps).unwrap_or(cap);
    Encoding { w, h, fps, level, max_bitrate: stream_bitrate(w, h, fps, level) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L52: Level = LEVELS[16];

    fn lvl(idc: u8) -> Level {
        Level::from_idc(idc).unwrap()
    }

    fn view(w: u32, h: u32, max_w: u32, max_h: u32) -> Constraints {
        Constraints { w, h, max_w, max_h, fps: 30, level: 31 }
    }

    fn aspect_kept(src: (usize, usize), e: &Encoding) {
        let (a, b) = (src.0 as f64 / src.1 as f64, e.w as f64 / e.h as f64);
        assert!((a / b - 1.0).abs() < 0.01, "{src:?} gave {}x{}", e.w, e.h);
        assert!(e.w % 2 == 0 && e.h % 2 == 0, "{}x{} is not even", e.w, e.h);
    }


    /// Fewer frames, each sharper: a 4K screen at 24 frames a second has the Kush gauge; at 2,
    /// each frame takes 0.4 bits a pixel, several times what one of the 24 gets.
    #[test]
    fn a_viewer_taking_fewer_frames_gets_each_one_sharper() {
        let unlimited = Constraints { w: 3840, h: 2160, max_w: 3840, max_h: 2160, fps: FPS, level: 0 };
        let smooth = choose((3840, 2160), &unlimited, Level::from_idc(52).unwrap());
        let sharp = choose((3840, 2160), &Constraints { fps: 2, ..unlimited }, Level::from_idc(52).unwrap());
        assert_eq!((smooth.w, smooth.h, smooth.fps, sharp.w, sharp.h, sharp.fps), (3840, 2160, 24, 3840, 2160, 2));
        let per_frame = |e: &Encoding| u64::from(e.max_bitrate) / u64::from(e.fps);
        assert_eq!(per_frame(&sharp), 3840 * 2160 * 2 / 5);
        assert!(per_frame(&sharp) > 5 * per_frame(&smooth), "{} vs {}", per_frame(&sharp), per_frame(&smooth));
        assert!(sharp.max_bitrate < smooth.max_bitrate, "fewer frames never use more than the full rate");
    }
    #[test]
    fn a_viewer_reporting_almost_nothing_still_gets_a_usable_picture() {
        // A tiny viewport, and a tiny viewport copied into the decode box too.
        for c in [view(1, 1, 3840, 2160), view(1, 1, 1, 1)] {
            let e = choose((3840, 2160), &c, L52);
            assert_eq!((e.w, e.h), (426, 240), "{c:?}");
            aspect_kept((3840, 2160), &e);
            // Upright sources keep the floor on their short side; smaller sources stay as they are.
            assert_eq!(choose((1080, 1920), &c, L52).w, 240, "{c:?}");
            let small = choose((200, 100), &c, L52);
            assert_eq!((small.w, small.h), (200, 100), "{c:?}");
        }
    }

    #[test]
    fn table_a1_limits_for_common_levels() {
        // 3.1: 720p30; 4.0: 1080p30 (8160 MBs incl. the padded row); 4.2: 1080p60;
        // 5.1: 4K30 and 4096x2304; 5.2: 4K60.
        assert!(lvl(31).holds(1280, 720, 30) && !lvl(31).holds(1920, 1080, 30) && !lvl(31).holds(1280, 720, 31));
        assert!(lvl(40).holds(1920, 1080, 30) && !lvl(40).holds(1920, 1080, 31) && !lvl(40).holds(2560, 1440, 30));
        assert!(lvl(42).holds(1920, 1080, 60) && !lvl(42).holds(2560, 1440, 30));
        assert!(lvl(51).holds(3840, 2160, 30) && lvl(51).holds(4096, 2304, 26) && !lvl(51).holds(3840, 2160, 31));
        assert!(lvl(52).holds(3840, 2160, 60) && !lvl(52).holds(5120, 2880, 30));
        // A long, thin frame is limited by its side, not only its area.
        assert!(!lvl(31).holds(16 * 170, 16, 30), "170 MBs wide exceeds sqrt(8*3600)");
        assert_eq!(Level::lowest_for(1280, 720, 30).unwrap().idc, 31);
        assert_eq!(Level::lowest_for(1920, 1080, 30).unwrap().idc, 40);
        assert_eq!(Level::lowest_for(1920, 1080, 60).unwrap().idc, 42);
        assert_eq!(Level::lowest_for(3840, 2160, 30).unwrap().idc, 51);
        assert_eq!(Level::lowest_for(3840, 2160, 60).unwrap().idc, 52);
        assert_eq!(lvl(40).max_bitrate(), 25_000_000);
    }

    /// The ladder web/static/js/session/constraints.js probes names these levels per rung.
    #[test]
    fn the_browser_ladder_levels_agree() {
        for (w, h, idc) in [(1280, 720, 31), (1920, 1080, 40), (2560, 1440, 50), (3840, 2160, 51), (5120, 2880, 60), (7680, 4320, 60)] {
            assert_eq!(Level::lowest_for(w, h, 30).unwrap().idc, idc, "{w}x{h}");
        }
    }

    #[test]
    fn profile_level_id_from_the_sdp() {
        let p = ProfileLevelId::parse("64001f").unwrap();
        assert_eq!((p.profile_idc, p.constraints, p.level.idc), (0x64, 0, 31));
        assert_eq!(ProfileLevelId::parse("42e01f").unwrap().level.idc, 31);
        assert_eq!(ProfileLevelId::parse("640C34").unwrap().level.idc, 52);
        assert_eq!(ProfileLevelId::parse("42f00b").unwrap().level.idc, 9, "Baseline 11 + constraint_set3 is 1b");
        assert_eq!(ProfileLevelId::parse("64000b").unwrap().level.idc, 11, "High 11 is 1.1");
        assert!(ProfileLevelId::parse("64001").is_none() && ProfileLevelId::parse("6400zz").is_none() && ProfileLevelId::parse("640017").is_none());
        let sdp = "v=0\r\na=rtpmap:102 H264/90000\r\na=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n\
                   a=rtpmap:118 H264/90000\r\na=fmtp:118 level-asymmetry-allowed=1; packetization-mode=1; Profile-Level-Id=640033\r\na=fmtp:119 apt=118\r\n";
        assert_eq!(ProfileLevelId::from_sdp(sdp, 118).unwrap().level.idc, 51);
        assert_eq!(ProfileLevelId::from_sdp(sdp, 102).unwrap().profile_idc, 0x42);
        assert_eq!(ProfileLevelId::from_sdp(sdp, 119).unwrap().level.idc, 10, "no profile-level-id means 42000A");
        assert_eq!(ProfileLevelId::from_sdp(sdp, 120).unwrap().level.idc, 10);
    }

    #[test]
    fn the_viewport_bounds_the_size_and_the_aspect_is_kept() {
        // A phone in portrait looking at a 2560x1440 desktop: as wide as the phone.
        let e = choose((2560, 1440), &view(1170, 2532, 3840, 2160), L52);
        assert_eq!((e.w, e.h), (1170, 658));
        aspect_kept((2560, 1440), &e);
        assert_eq!(e.level.idc, 31);
        // A window larger than the display: the display's own size, never scaled up.
        let e = choose((1920, 1080), &view(2880, 1800, 3840, 2160), L52);
        assert_eq!((e.w, e.h), (1920, 1080));
        // Odd source sizes become even.
        let e = choose((1921, 1081), &view(4000, 4000, 3840, 2160), L52);
        assert_eq!((e.w, e.h), (1920, 1080));
    }

    #[test]
    fn the_decode_box_bounds_the_size_turned_like_the_source() {
        let e = choose((3840, 2160), &view(3840, 2160, 1920, 1080), L52);
        assert_eq!((e.w, e.h), (1920, 1080));
        // A portrait display under a landscape box: the box is turned.
        let e = choose((2160, 3840), &view(4000, 4000, 1920, 1080), L52);
        assert_eq!((e.w, e.h), (1080, 1920));
        aspect_kept((2160, 3840), &e);
    }

    #[test]
    fn the_level_bounds_the_size() {
        // Without a decode box the SDP's level holds: 3.1 is 720p at 30 fps.
        let only_sdp = |idc| Constraints { level: idc, ..Default::default() };
        for (idc, src) in [(31, (1920, 1080)), (40, (2560, 1440)), (42, (3840, 2160)), (51, (5120, 2880)), (52, (7680, 4320))] {
            let e = choose(src, &only_sdp(idc), L52);
            assert!(lvl(idc).holds(e.w, e.h, e.fps), "level {idc}: {src:?} gave {}x{}", e.w, e.h);
            assert!(!lvl(idc).holds(e.w + 16, e.h + 16, e.fps), "level {idc}: {src:?} gave {}x{}, much smaller than needed", e.w, e.h);
            aspect_kept(src, &e);
            assert!(rank(e.level) <= rank(lvl(idc)));
        }
        assert_eq!({ let e = choose((1920, 1080), &only_sdp(31), L52); (e.w, e.h) }, (1280, 720));
        // A decode box beyond what the encoder can signal: the encoder's highest level holds.
        let e = choose((5120, 2880), &view(5120, 2880, 7680, 4320), L52);
        assert!(L52.holds(e.w, e.h, e.fps) && e.w >= 4000, "{}x{}", e.w, e.h);
        aspect_kept((5120, 2880), &e);
        assert_eq!((e.fps, e.level.idc), (24, 51), "4096x2304 at 24 fps fits 5.1's frame size and macroblock rate");
        // A lower frame rate lets more macroblocks through per frame only where MaxMBPS binds.
        let e = choose((3840, 2160), &Constraints { level: 42, fps: 15, ..Default::default() }, L52);
        assert!(lvl(42).holds(e.w, e.h, 15) && e.fps == 15);
    }

    #[test]
    fn rotation_and_constraint_updates_change_the_size() {
        let mut c = view(844 * 3, 390 * 3, 3840, 2160);
        let landscape = choose((2560, 1600), &c, L52);
        (c.w, c.h) = (c.h, c.w);
        let portrait = choose((2560, 1600), &c, L52);
        assert_eq!((landscape.w, landscape.h), (1872, 1170));
        assert_eq!((portrait.w, portrait.h), (1170, 730));
        // The display itself turned to portrait.
        let turned = choose((1600, 2560), &c, L52);
        assert_eq!((turned.w, turned.h), (1170, 1872));
        // The window grew: a larger stream, up to the display.
        c = view(1440, 900, 3840, 2160);
        assert_eq!({ let e = choose((2560, 1600), &c, L52); (e.w, e.h) }, (1440, 900));
        c = view(2880, 1800, 3840, 2160);
        assert_eq!({ let e = choose((2560, 1600), &c, L52); (e.w, e.h) }, (2560, 1600));
    }

    #[test]
    fn the_bitrate_ceiling_follows_the_size() {
        let small = choose((1920, 1080), &view(1170, 2532, 3840, 2160), L52);
        let full = choose((1920, 1080), &view(1920, 1080, 3840, 2160), L52);
        let four_k = choose((3840, 2160), &view(3840, 2160, 3840, 2160), L52);
        assert!(small.max_bitrate < full.max_bitrate && full.max_bitrate < four_k.max_bitrate);
        assert_eq!(full.max_bitrate, 3_483_648, "1920 x 1080 x 24 x 0.07");
        // Never more than the level allows: 1.1 carries 240 kbit/s at High profile.
        let tiny = choose((352, 288), &Constraints { level: 11, fps: 30, ..Default::default() }, L52);
        assert!(u64::from(tiny.max_bitrate) <= tiny.level.max_bitrate());
    }
}
