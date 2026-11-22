//! The picture's track: its payload type, its mid, and which access unit can start it.

use str0m::format::Codec;
use str0m::media::{Mid, Pt};
use str0m::Rtc;

pub fn choose_payload(rtc: &mut Rtc, mid: Mid) -> Option<Pt> {
    let w = rtc.writer(mid)?;
    let h264: Vec<_> = w.payload_params().filter(|p| p.spec().codec == Codec::H264).cloned().collect();
    // The encoder outputs High profile, so prefer the matching signaling when offered.
    let high = h264.iter().find(|p| p.spec().format.profile_level_id.is_some_and(|id| id >> 16 == 0x64));
    high.or(h264.first()).map(|p| p.pt())
}

/// The encoders return Annex B access units. The three-byte start code also occurs at
/// the end of a four-byte start code, so this accepts both forms.
pub fn has_idr(access_unit: &[u8]) -> bool {
    access_unit.windows(4).any(|bytes| bytes[..3] == [0, 0, 1] && bytes[3] & 0x1f == 5)
}

/// The mid of the offer's video section. It is known as soon as the offer is accepted; str0m
/// reports the media only once DTLS is connected.
pub fn video_mid(sdp: &str) -> Option<Mid> {
    let mut video = false;
    for line in sdp.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("m=") {
            video = m.starts_with("video ");
        } else if let (true, Some(id)) = (video, line.strip_prefix("a=mid:")) {
            return Some(Mid::from(id));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_idr_can_start_the_stream() {
        assert!(has_idr(&[0, 0, 0, 1, 0x67, 0, 0, 1, 0x68, 0, 0, 1, 0x65, 0x80]));
        assert!(has_idr(&[0, 0, 0, 1, 0x65, 0x80]));
        assert!(!has_idr(&[0, 0, 1, 0x67, 0, 0, 1, 0x41, 0x65]));
        assert!(!has_idr(&[0, 0, 1]));
    }

    #[test]
    fn the_video_mid_comes_from_the_video_section() {
        let data = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\n";
        assert_eq!(video_mid(&format!("{data}m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\n")), Some(Mid::from("1")));
        assert_eq!(video_mid(data), None, "no video section, no mid");
    }
}
