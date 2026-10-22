//! Opus outbound track. Video RTP lives in [`crate::webrtc::video_track`].

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
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

fn opus_restart_gap(
    frame: &OpusFrame,
    previous_sequence: Option<u64>,
    previous: Option<(Instant, Duration)>,
) -> Duration {
    if previous_sequence.is_some() {
        return Duration::ZERO;
    }
    previous
        .map(|(time, duration)| {
            frame
                .encoded_at
                .saturating_duration_since(time)
                .saturating_sub(duration)
        })
        .unwrap_or(Duration::ZERO)
}

pub struct OpusAudioTrack {
    track: Arc<TrackLocalStaticSample>,
    previous_encoded: Mutex<Option<(Instant, Duration)>>,
    stats: Option<Mutex<crate::audio::diagnostics::SendStats>>,
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

        Self {
            track,
            previous_encoded: Mutex::new(None),
            stats: crate::audio::diagnostics::SendStats::new().map(Mutex::new),
        }
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

        let mut previous = self.previous_encoded.lock().await;
        let mut gap = opus_restart_gap(frame, previous_sequence, *previous);
        // The track/SSRC survives recovery. An empty Opus sample advances the
        // packetizer clock without emitting a packet or consuming a sequence
        // number. Keep the actual outage in media time so resumed audio is not
        // discarded as late. Chunk long outages below the u32 sample limit.
        while !gap.is_zero() {
            let duration = gap.min(Duration::from_secs(3600));
            self.track
                .write_sample(&Sample {
                    duration,
                    ..Default::default()
                })
                .await
                .map_err(|error| AppError::WebRtcError(error.to_string()))?;
            gap -= duration;
        }
        let sample = opus_sample(frame, previous_sequence);
        *previous = Some((frame.encoded_at, sample.duration));

        let started = Instant::now();
        let result = self.track.write_sample(&sample).await;
        if let Some(stats) = &self.stats {
            stats.lock().await.observe(
                frame.encoded_at,
                started,
                sample.prev_dropped_packets,
                result.is_err(),
            );
        }
        result.map_err(|e| {
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

    #[test]
    fn capture_recovery_preserves_elapsed_rtp_time_on_the_existing_track() {
        let frame = OpusFrame {
            encoded_at: Instant::now(),
            data: Bytes::from_static(&[0xf8, 0xff, 0xfe]),
            duration_ms: 20,
            sequence: 0,
        };
        use webrtc::rtp::codecs::opus::OpusPayloader;
        use webrtc::rtp::packetizer::{new_packetizer, Packetizer};
        use webrtc::rtp::sequence::new_fixed_sequencer;
        let previous = Some((
            frame.encoded_at - Duration::from_secs(3),
            Duration::from_millis(20),
        ));
        let gap = opus_restart_gap(&frame, None, previous);
        assert_eq!(gap, Duration::from_millis(2980));
        assert_eq!(opus_restart_gap(&frame, Some(10), previous), Duration::ZERO);
        let mut packetizer = new_packetizer(
            1200,
            111,
            1,
            Box::new(OpusPayloader),
            Box::new(new_fixed_sequencer(7)),
            48000,
        );
        let first = packetizer.packetize(&frame.data, 960).unwrap().remove(0);
        assert!(packetizer
            .packetize(&Bytes::new(), (gap.as_secs_f64() * 48000.0) as u32)
            .unwrap()
            .is_empty());
        let next = packetizer.packetize(&frame.data, 960).unwrap().remove(0);
        assert_eq!(
            next.header.sequence_number,
            first.header.sequence_number.wrapping_add(1)
        );
        assert_eq!(
            next.header.timestamp.wrapping_sub(first.header.timestamp),
            144000
        );
    }
}
