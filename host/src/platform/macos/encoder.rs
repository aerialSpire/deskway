use std::ptr::{null_mut, NonNull};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use block2::RcBlock;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMVideoCodecType_H264, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, CVPixelBuffer,
};
use objc2_video_toolbox::*;

use crate::video::Encoder as VideoEncoder;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The ways to ask VideoToolbox for an H.264 encoder, best first, as the Windows encoder
/// falls back from hardware to software: some Macs refuse low-latency rate control (at some
/// sizes, or with the media engine busy), and a Mac without a usable hardware encoder still
/// has the software one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Hardware, low-latency rate control.
    LowLatency,
    /// Hardware, standard rate control.
    Hardware,
    /// The software encoder.
    Software,
}

const MODES: [Mode; 3] = [Mode::LowLatency, Mode::Hardware, Mode::Software];


pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    fps: i32,
    start: Instant,
    last_pts: i64,
    force_keyframe: std::cell::Cell<bool>,
}

impl Encoder {
    /// `width`×`height` is the real (even) size: VideoToolbox pads to whole macroblocks and
    /// crops in the SPS. `level` is an H.264 level_idc. Every mode gets the same size and level.
    fn new(mode: Mode, width: u32, height: u32, fps: u32, bitrate: u32, level: u8) -> Result<Self> {
        // The specification and each property value use the CF types declared by VideoToolbox.
        unsafe {
            let require_hardware = kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder;
            let yes = CFBoolean::new(true) as &CFType;
            let no = CFBoolean::new(false) as &CFType;
            let spec = match mode {
                Mode::LowLatency => CFDictionary::from_slices(
                    &[require_hardware as &CFType, kVTVideoEncoderSpecification_EnableLowLatencyRateControl],
                    &[yes, yes],
                ),
                Mode::Hardware => CFDictionary::from_slices(&[require_hardware as &CFType], &[yes]),
                Mode::Software => CFDictionary::from_slices(
                    &[kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as &CFType],
                    &[no],
                ),
            };
            let mut session = null_mut();
            check(
                VTCompressionSession::create(
                    None,
                    width as i32,
                    height as i32,
                    kCMVideoCodecType_H264,
                    Some(spec.as_opaque()),
                    None,
                    None,
                    None,
                    null_mut(),
                    NonNull::from(&mut session),
                ),
                "create H.264 session",
            )?;
            let session = CFRetained::from_raw(NonNull::new(session).context("no H.264 session")?);
            let encoder = Self {
                session,
                fps: fps as i32,
                start: Instant::now(),
                last_pts: -1,
                force_keyframe: std::cell::Cell::new(true),
            };
            encoder.property(kVTCompressionPropertyKey_RealTime, CFBoolean::new(true))?;
            encoder.property(
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            )?;
            encoder.property(kVTCompressionPropertyKey_ProfileLevel, high_profile_level(level))?;
            encoder.property(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i32((fps * 3600) as i32),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_i32(fps as i32),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ColorPrimaries,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            )?;
            encoder.property(
                kVTCompressionPropertyKey_TransferFunction,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            )?;
            encoder.property(
                kVTCompressionPropertyKey_YCbCrMatrix,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            )?;
            encoder.update_bitrate(bitrate)?;
            check(
                encoder.session.prepare_to_encode_frames(),
                "prepare H.264 session",
            )?;
            Ok(encoder)
        }
    }

    fn property(&self, key: &CFString, value: &CFType) -> Result<()> {
        check(
            unsafe { VTSessionSetProperty(&self.session, key, Some(value)) },
            "set encoder property",
        )
        .with_context(|| key.to_string())
    }

    fn update_bitrate(&self, bitrate: u32) -> Result<()> {
        // Allow short bursts above the average so keyframes and static-screen refinements fit.
        let bytes = CFNumber::new_i64(i64::from(bitrate) / 4);
        let second = CFNumber::new_i32(1);
        let limit = CFArray::from_objects(&[&*bytes, &*second]);
        unsafe {
            self.property(
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i64(i64::from(bitrate)),
            )?;
            self.property(kVTCompressionPropertyKey_DataRateLimits, &limit)
        }
    }
}

/// VideoToolbox's High profile levels run from 3.0 to 5.2. A stream that needs less is
/// marked 3.0, which every viewer that negotiated High at 3.1 (browsers' default) decodes.
fn high_profile_level(level: u8) -> &'static CFString {
    unsafe {
        match level {
            ..=30 => kVTProfileLevel_H264_High_3_0,
            31 => kVTProfileLevel_H264_High_3_1,
            32 => kVTProfileLevel_H264_High_3_2,
            40 => kVTProfileLevel_H264_High_4_0,
            41 => kVTProfileLevel_H264_High_4_1,
            42 => kVTProfileLevel_H264_High_4_2,
            50 => kVTProfileLevel_H264_High_5_0,
            51 => kVTProfileLevel_H264_High_5_1,
            _ => kVTProfileLevel_H264_High_5_2,
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

impl VideoEncoder for Encoder {
    type Frame = CVPixelBuffer;
    /// The modes in `MODES`, best first.
    type Catalog = ();
    /// kVTProfileLevel_H264_High_5_2 is VideoToolbox's highest.
    const MAX_LEVEL: u8 = 52;

    fn catalog() -> Result<()> {
        Ok(())
    }

    fn count(_: &()) -> usize {
        MODES.len()
    }

    fn open(_: &mut (), index: usize, width: u32, height: u32, fps: u32, bitrate: u32, level: u8) -> Result<Self> {
        let mode = MODES[index];
        let encoder = Self::new(mode, width, height, fps, bitrate, level).with_context(|| format!("VideoToolbox {mode:?}"))?;
        eprintln!("encoder: VideoToolbox H.264 {mode:?} {width}x{height} level {level}");
        Ok(encoder)
    }

    fn set_bitrate(&self, bitrate: u32) {
        if let Err(err) = self.update_bitrate(bitrate) {
            eprintln!("cannot update VideoToolbox bitrate: {err:#}");
        }
    }

    fn force_keyframe(&self) {
        self.force_keyframe.set(true);
    }

    fn encode(&mut self, frame: &CVPixelBuffer) -> Result<Vec<Vec<u8>>> {
        let result = Arc::new(Mutex::new(None));
        let output = Arc::clone(&result);
        let callback = RcBlock::new(
            move |status, flags: VTEncodeInfoFlags, sample: *mut CMSampleBuffer| {
                let decoded = check(status, "encode H.264 frame").and_then(|()| {
                    if flags.contains(VTEncodeInfoFlags::FrameDropped) {
                        return Ok(None);
                    }
                    let sample =
                        unsafe { sample.as_ref() }.context("encoder returned no sample")?;
                    access_unit(sample).map(Some)
                });
                *output.lock().unwrap() = Some(decoded);
            },
        );
        let force_key = self.force_keyframe.get();
        unsafe {
            let options = force_key.then(|| {
                CFDictionary::from_slices(
                    &[kVTEncodeFrameOptionKey_ForceKeyFrame as &CFType],
                    &[CFBoolean::new(true) as &CFType],
                )
            });
            self.last_pts = (self.start.elapsed().as_micros() as i64).max(self.last_pts + 1);
            let pts = CMTime::new(self.last_pts, 1_000_000);
            let duration = CMTime::new(1, self.fps);
            check(
                self.session.encode_frame_with_output_handler(
                    frame,
                    pts,
                    duration,
                    options.as_deref().map(CFDictionary::as_opaque),
                    null_mut(),
                    RcBlock::as_ptr(&callback),
                ),
                "submit H.264 frame",
            )?;
            // A static desktop may not produce another input. Drain through this frame now so
            // the first keyframe and the last refinement reach the browser even on an idle screen.
            check(self.session.complete_frames(pts), "complete H.264 frame")?;
        }
        let data = result.lock().unwrap().take().transpose()?.flatten();
        if data.is_some() {
            self.force_keyframe.set(false);
        }
        Ok(data.into_iter().collect())
    }
}

pub(super) fn check(status: i32, operation: &str) -> Result<()> {
    ensure!(status == 0, "{operation}: OSStatus {status}");
    Ok(())
}

fn access_unit(sample: &CMSampleBuffer) -> Result<Vec<u8>> {
    let format = unsafe { sample.format_description() }.context("H.264 sample missing format")?;
    let block = unsafe { sample.data_buffer() }.context("H.264 sample missing data")?;
    let mut length_size = 0;
    let mut set_count = 0;
    let mut data = vec![0; unsafe { block.data_length() }];
    unsafe {
        check(
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &format,
                0,
                null_mut(),
                null_mut(),
                &mut set_count,
                &mut length_size,
            ),
            "read H.264 format",
        )?;
        check(
            block.copy_data_bytes(
                0,
                data.len(),
                NonNull::new(data.as_mut_ptr().cast()).unwrap(),
            ),
            "copy H.264 sample",
        )?;
    }
    annex_b(&data, length_size as usize, || {
        parameter_sets(&format, set_count)
    })
}

fn parameter_sets(format: &CMFormatDescription, count: usize) -> Result<Vec<u8>> {
    let mut sets = Vec::new();
    let (mut has_sps, mut has_pps) = (false, false);
    for index in 0..count {
        let mut bytes = std::ptr::null();
        let mut len = 0;
        unsafe {
            check(
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format,
                    index,
                    &mut bytes,
                    &mut len,
                    null_mut(),
                    null_mut(),
                ),
                "read H.264 parameter set",
            )?;
            ensure!(!bytes.is_null() && len > 0, "empty H.264 parameter set");
            // The parameter set data is valid while the format description is retained.
            let set = std::slice::from_raw_parts(bytes, len);
            match set[0] & 0x1f {
                7 => has_sps = true,
                8 => has_pps = true,
                _ => continue,
            }
            sets.extend_from_slice(&START_CODE);
            sets.extend_from_slice(set);
        }
    }
    ensure!(has_sps && has_pps, "H.264 keyframe missing SPS or PPS");
    Ok(sets)
}

fn annex_b(
    mut avcc: &[u8],
    length_size: usize,
    parameter_sets: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    ensure!(
        matches!(length_size, 1 | 2 | 4),
        "invalid AVCC length width {length_size}"
    );
    ensure!(!avcc.is_empty(), "empty AVCC sample");
    let mut out = Vec::with_capacity(avcc.len());
    let mut idr = false;
    while !avcc.is_empty() {
        let (header, rest) = avcc
            .split_at_checked(length_size)
            .context("truncated AVCC NAL length")?;
        let len = header
            .iter()
            .fold(0usize, |len, &byte| (len << 8) | usize::from(byte));
        ensure!(len != 0, "empty AVCC NAL");
        let (nal, rest) = rest.split_at_checked(len).context("truncated AVCC NAL")?;
        idr |= nal[0] & 0x1f == 5;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        avcc = rest;
    }
    if idr {
        let mut key = parameter_sets()?;
        key.extend_from_slice(&out);
        return Ok(key);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_each_nal_and_prefixes_idr_with_parameter_sets() {
        let parameter_sets = [0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xaa];
        let mut expected = parameter_sets.to_vec();
        expected.extend_from_slice(&[
            0, 0, 0, 1, 0x06, 0x99, 0, 0, 0, 1, 0x65, 0xff, 0, 0, 0, 1, 0x65, 0x10,
        ]);
        for width in [1, 2, 4] {
            let mut avcc = Vec::new();
            for nal in [[0x06, 0x99], [0x65, 0xff], [0x65, 0x10]] {
                avcc.extend_from_slice(&2u32.to_be_bytes()[4 - width..]);
                avcc.extend_from_slice(&nal);
            }
            assert_eq!(
                annex_b(&avcc, width, || Ok(parameter_sets.to_vec())).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn skips_parameter_sets_on_inter_frames() {
        assert_eq!(
            annex_b(&[0, 2, 0x41, 0xaa], 2, || panic!("inter frame")).unwrap(),
            [0, 0, 0, 1, 0x41, 0xaa]
        );
    }

    #[test]
    fn rejects_invalid_or_truncated_lengths() {
        for (avcc, width) in [
            (&[][..], 4),
            (&[0, 0][..], 4),
            (&[0, 3, 0x41][..], 2),
            (&[0, 0][..], 2),
            (&[0, 0, 1, 0x41][..], 3),
        ] {
            assert!(annex_b(avcc, width, || Ok(Vec::new())).is_err());
        }
    }

    #[test]
    fn video_toolbox_outputs_high_profile_idrs_at_non_macroblock_sizes() {
        use objc2_core_media::{
            CMVideoFormatDescriptionCreateFromH264ParameterSets,
            CMVideoFormatDescriptionGetDimensions,
        };
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeightOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
            CVPixelBufferUnlockBaseAddress,
        };

        use crate::video::size::{choose, Constraints, Level};

        let view = Constraints {
            w: 1279,
            h: 719,
            max_w: 1920,
            max_h: 1080,
            fps: 30,
            ..Default::default()
        };
        let size = choose(
            (1920, 1080),
            &view,
            Level::from_idc(Encoder::MAX_LEVEL).unwrap(),
        );
        let (width, height, level) = (size.w as u32, size.h as u32, size.level.idc);
        assert_eq!((width, height, level), (1278, 718, 31));
        let frame = super::super::capture::new_nv12(size.w, size.h).unwrap();
        unsafe {
            let flags = CVPixelBufferLockFlags::empty();
            check(
                CVPixelBufferLockBaseAddress(&frame, flags),
                "lock test image",
            )
            .unwrap();
            for (plane, value) in [(0, 128), (1, 128)] {
                let ptr = CVPixelBufferGetBaseAddressOfPlane(&frame, plane).cast::<u8>();
                let len = CVPixelBufferGetBytesPerRowOfPlane(&frame, plane)
                    * CVPixelBufferGetHeightOfPlane(&frame, plane);
                std::ptr::write_bytes(ptr, value, len);
            }
            check(
                CVPixelBufferUnlockBaseAddress(&frame, flags),
                "unlock test image",
            )
            .unwrap();
        }

        let check_idr = |access_units: Vec<Vec<u8>>, expected_level| {
            assert_eq!(access_units.len(), 1);
            let frame = &access_units[0];
            let start = frame
                .windows(4)
                .enumerate()
                .filter_map(|(i, bytes)| (bytes == START_CODE).then_some(i + 4))
                .collect::<Vec<_>>();
            let types = start.iter().map(|&i| frame[i] & 0x1f).collect::<Vec<_>>();
            assert!(
                types.starts_with(&[7, 8]) && types.contains(&5),
                "{types:?}"
            );
            assert_eq!(frame[start[0] + 1], 0x64, "High profile");
            assert_eq!(frame[start[0] + 3], expected_level, "the H.264 level asked for");
            // Parse the SPS crop rectangle and verify the exact frame the browser can decode.
            let sps = &frame[start[0]..start[1] - START_CODE.len()];
            let pps = &frame[start[1]..start[2] - START_CODE.len()];
            let mut sets = [
                NonNull::from(&sps[0]).cast::<u8>(),
                NonNull::from(&pps[0]).cast::<u8>(),
            ];
            let mut sizes = [sps.len(), pps.len()];
            let mut format = std::ptr::null();
            unsafe {
                check(
                    CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        None,
                        sets.len(),
                        NonNull::new(sets.as_mut_ptr()).unwrap(),
                        NonNull::new(sizes.as_mut_ptr()).unwrap(),
                        4,
                        NonNull::from(&mut format),
                    ),
                    "parse H.264 SPS and PPS",
                )
                .unwrap();
                let format = CFRetained::from_raw(NonNull::new(format.cast_mut()).unwrap());
                let dimensions = CMVideoFormatDescriptionGetDimensions(&format);
                assert_eq!(
                    (dimensions.width, dimensions.height),
                    (width as i32, height as i32)
                );
            }
        };
        let all = std::env::var_os("DESKWAY_TEST_VT_MODES").is_some();
        let modes: &[_] = if all { &MODES } else { &[Mode::LowLatency] };
        let levels: &[_] = if all { &[level, 40] } else { &[level] };
        for &mode in modes {
            for &level in levels {
                eprintln!("testing VideoToolbox {mode:?} {width}x{height} level {level}");
                let mut encoder = Encoder::new(mode, width, height, 30, 4_000_000, level).unwrap();
                check_idr(encoder.encode(&frame).unwrap(), level);
                encoder.update_bitrate(2_000_000).unwrap();
                encoder.force_keyframe();
                check_idr(encoder.encode(&frame).unwrap(), level);
            }
        }
    }
}
