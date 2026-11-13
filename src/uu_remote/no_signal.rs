//! Placeholder video while the HDMI source has no signal. Official viewers keep
//! loading until the first decodable frame, and may not enable input before it,
//! so a sleeping target could not be woken from the viewer.
use std::time::Duration;

use crate::video::codec::VideoCodecType;

/// Video silence before, and between, placeholder frames. Each one is a
/// standalone IDR, so a lost frame or a reconnecting decoder recovers.
pub(super) const INTERVAL: Duration = Duration::from_secs(1);

/// 1280x720 SDR access units from no_signal/generate.py. Only the bitstream has
/// this size; the screen and input geometry reported to the viewer stay unchanged.
pub(super) fn frame(codec: VideoCodecType) -> &'static [u8] {
    match codec {
        VideoCodecType::H265 => include_bytes!("no_signal/no_signal.h265"),
        _ => include_bytes!("no_signal/no_signal.h264"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::codec::{h264_bitstream, h265_bitstream};

    fn probe(data: &[u8]) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,profile,width,height,pix_fmt,color_transfer,nb_read_frames",
                "-of",
                "csv=p=0",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Install ffmpeg to decode the placeholder bitstreams");
        child.stdin.take().unwrap().write_all(data).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn placeholders_are_standalone_sdr_keyframes_for_every_negotiated_profile() {
        let h264 = frame(VideoCodecType::H264);
        assert!(h264_bitstream::has_sps_pps(h264) && h264_bitstream::is_keyframe(h264));
        assert_eq!(
            probe(h264),
            "h264,Constrained Baseline,1280,720,yuv420p,bt709,1"
        );

        let h265 = frame(VideoCodecType::H265);
        assert!(h265_bitstream::has_vps_sps_pps(h265) && h265_bitstream::is_keyframe(h265));
        // x265 signals all-intra encodes as Main Intra, which Main receivers reject.
        assert_eq!(h265_bitstream::conforms_to_profile(h265, 1), Some(true));
        assert_eq!(h265_bitstream::conforms_to_profile(h265, 2), Some(true));
        assert_eq!(probe(h265), "hevc,Main,1280,720,yuv420p,bt709,1");
    }
}
