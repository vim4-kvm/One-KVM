//! Opus outbound track. Video RTP lives in [`crate::webrtc::video_track`].

use std::sync::Arc;
use std::time::Duration;
use tracing::error;
use webrtc::media::Sample;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::audio::OpusFrame;
use crate::error::{AppError, Result};

pub const RTP_MTU: usize = 1200;

pub const H264_CLOCK_RATE: u32 = 90000;

fn opus_sample(frame: &OpusFrame, previous_sequence: Option<u64>) -> Sample {
    // Fan-out can drop packets for a slow subscriber. Keep those missing samples
    // in RTP time; compressing the timeline makes the receiver's A/V sync drift.
    let dropped = previous_sequence
        .map(|previous| frame.sequence.saturating_sub(previous).saturating_sub(1))
        .unwrap_or(0);
    Sample {
        data: frame.data.clone(),
        duration: Duration::from_millis(u64::from(frame.duration_ms)),
        prev_dropped_packets: dropped.min(u64::from(u16::MAX)) as u16,
        ..Default::default()
    }
}

pub struct OpusAudioTrack {
    track: Arc<TrackLocalStaticSample>,
}

impl OpusAudioTrack {
    pub fn new(track_id: &str, stream_id: &str) -> Self {
        let codec = RTCRtpCodecCapability {
            mime_type: "audio/opus".to_string(),
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
            rtcp_feedback: vec![],
        };

        let track = Arc::new(TrackLocalStaticSample::new(
            codec,
            track_id.to_string(),
            stream_id.to_string(),
        ));

        Self { track }
    }

    pub fn track(&self) -> Arc<TrackLocalStaticSample> {
        self.track.clone()
    }

    pub fn as_track_local(&self) -> Arc<dyn TrackLocal + Send + Sync> {
        self.track.clone()
    }

    pub async fn write_frame(
        &self,
        frame: &OpusFrame,
        previous_sequence: Option<u64>,
    ) -> Result<()> {
        if frame.is_empty() {
            return Ok(());
        }

        let sample = opus_sample(frame, previous_sequence);

        self.track.write_sample(&sample).await.map_err(|e| {
            error!("Failed to write Opus sample: {}", e);
            AppError::WebRtcError(format!("Failed to write audio sample: {}", e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn dropped_audio_keeps_its_place_in_media_time() {
        let frame = OpusFrame {
            encoded_at: std::time::Instant::now(),
            data: Bytes::from_static(&[0xf8, 0xff, 0xfe]),
            duration_ms: 20,
            sequence: 105,
        };
        let sample = opus_sample(&frame, Some(100));
        // Four omitted 20 ms packets must not turn a 100 ms interval into 20 ms.
        assert_eq!(
            sample.duration * (u32::from(sample.prev_dropped_packets) + 1),
            Duration::from_millis(100)
        );
        assert_eq!(sample.data, frame.data);
        assert_eq!(opus_sample(&frame, None).prev_dropped_packets, 0);
        // A restarted encoder has a new sequence origin, not an enormous gap.
        assert_eq!(opus_sample(&frame, Some(500)).prev_dropped_packets, 0);
    }
}
