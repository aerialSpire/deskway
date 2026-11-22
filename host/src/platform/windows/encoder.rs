//! Media Foundation H.264 encoding, hardware first with software fallback.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::*;

pub type Res<T> = windows::core::Result<T>;

pub struct EncoderInfo {
    pub name: String,
    activate: IMFActivate,
}

/// Lists H.264 encoders, hardware first. Availability varies by machine and driver,
/// so callers must be ready for any of them to fail when used.
pub fn list_encoders() -> Res<Vec<EncoderInfo>> {
    let mut out = vec![];
    for flag in [MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SYNCMFT] {
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut n = 0u32;
            MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flag | MFT_ENUM_FLAG_SORTANDFILTER, Some(&input), Some(&output), &mut arr, &mut n)?;
            for i in 0..n as usize {
                if let Some(a) = (*arr.add(i)).take() {
                    let mut p = windows::core::PWSTR::null();
                    let mut len = 0u32;
                    let name = if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len).is_ok() {
                        let s = p.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(p.0 as _));
                        s
                    } else {
                        "unknown".into()
                    };
                    out.push(EncoderInfo { name, activate: a });
                }
            }
            CoTaskMemFree(Some(arr as _));
        }
    }
    Ok(out)
}

fn set_ratio(t: &IMFMediaType, key: &GUID, a: u32, b: u32) -> Res<()> {
    unsafe { t.SetUINT64(key, ((a as u64) << 32) | b as u64) }
}

fn variant_u32(v: u32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    var
}

fn variant_true() -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_TRUE;
    }
    var
}

pub struct Encoder {
    activate: IMFActivate,
    mft: IMFTransform,
    api: Option<ICodecAPI>,
    events: Option<IMFMediaEventGenerator>,
    out_size: u32,
    provides_samples: bool,
    pts: i64,
    fps: u32,
    pending_inputs: u32,
}

impl Encoder {
    /// `w`×`h` is the real (even) size: the encoder pads to whole macroblocks and writes the
    /// cropping into the SPS. `level` is an H.264 level_idc, which eAVEncH264VLevel equals.
    pub fn new(info: &EncoderInfo, w: u32, h: u32, fps: u32, bitrate: u32, gop: u32, level: u8) -> Res<Encoder> {
        unsafe {
            let mft: IMFTransform = info.activate.ActivateObject()?;
            let attrs = mft.GetAttributes()?;
            let is_async = attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
            if is_async {
                attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            }
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let api = mft.cast::<ICodecAPI>().ok();
            if let Some(api) = &api {
                // Best effort: not every encoder accepts every property.
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &variant_true());
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(gop));
                let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
            }

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            set_ratio(&out, &MF_MT_FRAME_SIZE, w, h)?;
            set_ratio(&out, &MF_MT_FRAME_RATE, fps, 1)?;
            set_ratio(&out, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_LEVEL, u32::from(level))?;
            mft.SetOutputType(0, &out, 0)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            set_ratio(&inp, &MF_MT_FRAME_SIZE, w, h)?;
            set_ratio(&inp, &MF_MT_FRAME_RATE, fps, 1)?;
            set_ratio(&inp, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &inp, 0)?;

            let si = mft.GetOutputStreamInfo(0)?;
            let provides = si.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            let events = if is_async { Some(mft.cast::<IMFMediaEventGenerator>()?) } else { None };
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Encoder {
                activate: info.activate.clone(),
                mft,
                api,
                events,
                out_size: si.cbSize.max(w * h * 3 / 2 + 4096),
                provides_samples: provides,
                pts: 0,
                fps,
                pending_inputs: 0,
            })
        }
    }

    /// Makes the next encoded frame a keyframe. Encoders that ignore this
    /// still recover at the next scheduled keyframe.
    pub fn force_keyframe(&self) {
        if let Some(api) = &self.api {
            unsafe {
                let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
            }
        }
    }

    /// Changes the target bitrate of the running encoder.
    pub fn set_bitrate(&self, bitrate: u32) {
        if let Some(api) = &self.api {
            unsafe {
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
            }
        }
    }

    fn make_sample(&mut self, nv12: &[u8]) -> Res<IMFSample> {
        unsafe {
            let buf = MFCreateMemoryBuffer(nv12.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), p, nv12.len());
            buf.Unlock()?;
            buf.SetCurrentLength(nv12.len() as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            let dur = 10_000_000i64 / self.fps as i64;
            s.SetSampleTime(self.pts)?;
            s.SetSampleDuration(dur)?;
            self.pts += dur;
            Ok(s)
        }
    }

    fn pull(&mut self) -> Res<Option<Vec<u8>>> {
        unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut odb = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut odb, &mut status);
            let s = ManuallyDrop::take(&mut odb[0].pSample);
            let _ = ManuallyDrop::take(&mut odb[0].pEvents);
            match r {
                Ok(()) => {
                    let Some(s) = s else { return Ok(None) };
                    let b = s.ConvertToContiguousBuffer()?;
                    let mut p = std::ptr::null_mut();
                    let mut len = 0u32;
                    b.Lock(&mut p, None, Some(&mut len))?;
                    let data = std::slice::from_raw_parts(p, len as usize).to_vec();
                    b.Unlock()?;
                    Ok(Some(data))
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
                Err(e) => Err(e),
            }
        }
    }

    /// Handles async encoder events without blocking; true if any event was consumed.
    fn poll_event(&mut self, ev: &IMFMediaEventGenerator, out: &mut Vec<Vec<u8>>) -> Res<bool> {
        unsafe {
            let e = match ev.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
                Err(e) => return Err(e),
            };
            let kind = MF_EVENT_TYPE(e.GetType()? as i32);
            if kind == METransformNeedInput {
                self.pending_inputs += 1;
            } else if kind == METransformHaveOutput {
                if let Some(f) = self.pull()? {
                    out.push(f);
                }
            }
            Ok(true)
        }
    }

    /// Feeds one NV12 frame and returns whatever output is ready. Waits are bounded, so an
    /// encoder that buffers frames or stalls delays the output instead of hanging the capture.
    pub fn encode(&mut self, nv12: &[u8]) -> Res<Vec<Vec<u8>>> {
        let sample = self.make_sample(nv12)?;
        let mut out = vec![];
        unsafe {
            let Some(ev) = self.events.clone() else {
                self.mft.ProcessInput(0, &sample, 0)?;
                while let Some(f) = self.pull()? {
                    out.push(f);
                }
                return Ok(out);
            };
            let budget = Instant::now() + Duration::from_millis(100);
            while self.pending_inputs == 0 {
                if !self.poll_event(&ev, &mut out)? {
                    if Instant::now() > budget {
                        // Dropping this frame is safe: the next one replaces it.
                        return Ok(out);
                    }
                    std::thread::yield_now();
                }
            }
            self.mft.ProcessInput(0, &sample, 0)?;
            self.pending_inputs -= 1;
            let budget = Instant::now() + Duration::from_millis(40);
            while out.is_empty() && Instant::now() < budget {
                if !self.poll_event(&ev, &mut out)? {
                    std::thread::yield_now();
                }
            }
        }
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            // Lets the same IMFActivate build a fresh encoder next time.
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// Builds the first working encoder, starting at `from`, and records its index.

/// The H.264 encoders Media Foundation offers, hardware first.
pub struct Catalog {
    encoders: Vec<EncoderInfo>,
}

impl crate::video::Encoder for Encoder {
    type Frame = [u8];
    type Catalog = Catalog;
    /// eAVEncH264VLevel ends at 5.2.
    const MAX_LEVEL: u8 = eAVEncH264VLevel5_2.0 as u8;

    fn catalog() -> anyhow::Result<Catalog> {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        Ok(Catalog { encoders: list_encoders()? })
    }

    fn count(catalog: &Catalog) -> usize {
        catalog.encoders.len()
    }

    fn open(catalog: &mut Catalog, index: usize, w: u32, h: u32, fps: u32, bitrate: u32, level: u8) -> anyhow::Result<Self> {
        let info = &catalog.encoders[index];
        let e = Encoder::new(info, w, h, fps, bitrate, fps * 3600, level).map_err(|err| anyhow::anyhow!("{}: {err}", info.name))?;
        eprintln!("encoder: {} {w}x{h} level {level}", info.name);
        Ok(e)
    }

    fn set_bitrate(&self, bitrate: u32) {
        self.set_bitrate(bitrate);
    }

    fn force_keyframe(&self) {
        self.force_keyframe();
    }

    fn encode(&mut self, frame: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
        Ok(self.encode(frame)?)
    }
}

#[cfg(test)]
mod probe {
    use super::*;

    /// Reads an exp-Golomb coded SPS, as far as the size, the level and the cropping.
    struct Bits<'a>(&'a [u8], usize);

    impl Bits<'_> {
        fn bit(&mut self) -> u32 {
            let b = (self.0[self.1 / 8] >> (7 - self.1 % 8)) & 1;
            self.1 += 1;
            b as u32
        }
        fn bits(&mut self, n: u32) -> u32 {
            (0..n).fold(0, |v, _| (v << 1) | self.bit())
        }
        fn ue(&mut self) -> u32 {
            let mut zeros = 0;
            while self.bit() == 0 {
                zeros += 1;
            }
            (1 << zeros) - 1 + self.bits(zeros)
        }
    }

    /// (level_idc, width, height) as a decoder shows the picture: macroblocks minus cropping.
    fn sps_size(au: &[u8]) -> (u8, u32, u32) {
        let start = au.windows(4).position(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 7).expect("an SPS") + 4;
        let end = au[start..].windows(3).position(|w| w == [0, 0, 1]).map_or(au.len(), |e| start + e);
        // Emulation prevention bytes out.
        let mut rbsp = Vec::new();
        for &b in &au[start..end] {
            if b == 3 && rbsp.ends_with(&[0, 0]) {
                continue;
            }
            rbsp.push(b);
        }
        let mut r = Bits(&rbsp, 0);
        let profile = r.bits(8);
        r.bits(8);
        let level = r.bits(8) as u8;
        r.ue();
        if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128) {
            assert_eq!(r.ue(), 1, "4:2:0");
            r.ue();
            r.ue();
            r.bit();
            assert_eq!(r.bit(), 0, "no scaling matrices");
        }
        r.ue();
        match r.ue() {
            0 => {
                r.ue();
            }
            2 => {}
            t => panic!("pic_order_cnt_type {t}"),
        }
        r.ue();
        r.bit();
        let (mbs_w, map_h) = (r.ue() + 1, r.ue() + 1);
        let frame_only = r.bit();
        if frame_only == 0 {
            r.bit();
        }
        r.bit();
        let (mut w, mut h) = (mbs_w * 16, map_h * 16 * (2 - frame_only));
        if r.bit() == 1 {
            let (left, right, top, bottom) = (r.ue(), r.ue(), r.ue(), r.ue());
            w -= 2 * (left + right);
            h -= 2 * (2 - frame_only) * (top + bottom);
        }
        (level, w, h)
    }

    /// Run by hand on a machine with Media Foundation encoders (synthetic grey frames only):
    /// every encoder takes the real, non-16-aligned size in MF_MT_FRAME_SIZE, pads it to whole
    /// macroblocks itself, and its SPS crops back to exactly that size at the level asked for.
    #[test]
    #[ignore]
    fn encoders_pad_and_crop_real_sizes() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
        }
        let list = list_encoders().unwrap();
        assert!(!list.is_empty());
        for (w, h) in [(1170u32, 658u32), (1366, 768), (1082, 1922), (3838, 2158), (1920, 1080), (4096, 2304)] {
            let level = crate::video::size::Level::lowest_for(w as usize, h as usize, 30).unwrap().idc;
            for e in &list {
                let mut enc = match Encoder::new(e, w, h, 30, 8_000_000, 90, level) {
                    Ok(enc) => enc,
                    Err(err) => {
                        println!("{w}x{h}  {:<45} refused: {err}", e.name);
                        continue;
                    }
                };
                let frame = vec![128u8; (w * h * 3 / 2) as usize];
                let mut first = None;
                for _ in 0..10 {
                    if let Some(au) = enc.encode(&frame).unwrap().into_iter().next() {
                        first.get_or_insert(au);
                    }
                }
                let au = first.unwrap_or_else(|| panic!("{} gave no output for {w}x{h}", e.name));
                let (got_level, sw, sh) = sps_size(&au);
                println!("{w}x{h}  {:<45} SPS {sw}x{sh} level {got_level} (asked {level})", e.name);
                assert_eq!((sw, sh), (w, h), "{}: the SPS crops to the real size", e.name);
                assert!(got_level >= level, "{}: level {got_level} below {level}", e.name);
            }
        }
    }
}
