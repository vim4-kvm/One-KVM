//! Aggregate diagnostics only; no changes to pacing, retransmission or recovery.
use std::any::Any;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::status::{UuVideoReceiver, UuVideoSender, UuVideoStats};
use tracing::info;
use webrtc::rtcp::{
    payload_feedbacks::{
        full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
    },
    receiver_report::ReceiverReport,
    reception_report::ReceptionReport,
    transport_feedbacks::transport_layer_nack::TransportLayerNack,
};

#[derive(Debug)]
struct ReceiverSample {
    received_at: Instant,
    ssrc: u32,
    last_sequence: u32,
    lost: i32,
    loss_percent: f64,
    jitter_ms: f64,
    rtt_ms: Option<f64>,
}

impl ReceiverSample {
    fn new(report: &ReceptionReport, now: SystemTime, received_at: Instant) -> Self {
        Self {
            received_at,
            ssrc: report.ssrc,
            last_sequence: report.last_sequence_number,
            // rtcp 0.17 exposes this signed 24-bit wire field as u32.
            lost: ((report.total_lost << 8) as i32) >> 8,
            loss_percent: f64::from(report.fraction_lost) * 100.0 / 256.0,
            jitter_ms: f64::from(report.jitter) / 90.0,
            rtt_ms: report_rtt_ms(report, now),
        }
    }
}

fn report_rtt_ms(report: &ReceptionReport, now: SystemTime) -> Option<f64> {
    if report.last_sender_report == 0 {
        return None;
    }
    let unix = now.duration_since(UNIX_EPOCH).ok()?;
    // RFC 3550 §6.4.1: A - LSR - DLSR in compact NTP (16.16) units.
    // Modular subtraction handles the 65536-second wrap. Reject implausible
    // results (including negative differences after a wall-clock adjustment).
    // This is an estimate: small clock steps cannot be detected from RR alone.
    let seconds = unix.as_secs().wrapping_add(2_208_988_800);
    let compact =
        ((seconds as u32) << 16) | (u64::from(unix.subsec_nanos()) * 65536 / 1_000_000_000) as u32;
    let elapsed = compact
        .wrapping_sub(report.last_sender_report)
        .wrapping_sub(report.delay);
    (elapsed < 30 * 65536).then(|| f64::from(elapsed) * 1000.0 / 65536.0)
}

#[derive(Default)]
pub(super) struct VideoFeedback {
    nack_requests: u64,
    pli: u64,
    fir: u64,
    receiver_reports: u64,
    latest: Option<ReceiverSample>,
    last_log: Option<Instant>,
}

/// Owned by one peer; snapshots keep independent receiver/sender sample ages.
#[derive(Default)]
pub(super) struct VideoDiagnostics {
    pub feedback: VideoFeedback,
    pub sender: Option<(Instant, UuVideoSender)>,
}

impl VideoDiagnostics {
    pub fn snapshot(&self, now: Instant) -> UuVideoStats {
        UuVideoStats {
            nack_packet_requests: self.feedback.nack_requests as f64,
            pli: self.feedback.pli as f64,
            fir: self.feedback.fir as f64,
            receiver: self.feedback.latest.as_ref().map(|report| UuVideoReceiver {
                age_ms: now.duration_since(report.received_at).as_secs_f64() * 1000.0,
                loss_percent: report.loss_percent,
                total_lost: report.lost,
                jitter_ms: report.jitter_ms,
                rtt_ms: report.rtt_ms,
            }),
            sender: self.sender.as_ref().map(|(sampled_at, sample)| {
                let mut sample = sample.clone();
                sample.age_ms = now.duration_since(*sampled_at).as_secs_f64() * 1000.0;
                sample
            }),
        }
    }
}

impl VideoFeedback {
    pub fn observe(&mut self, packet: &dyn Any, now: SystemTime, received_at: Instant) {
        if let Some(nack) = packet.downcast_ref::<TransportLayerNack>() {
            // Count requested packet IDs, including repeat requests. This is
            // neither unique packet loss nor confirmed successful retransmission.
            self.nack_requests += nack
                .nacks
                .iter()
                .map(|pair| 1 + u64::from(pair.lost_packets.count_ones()))
                .sum::<u64>();
        } else if packet.is::<PictureLossIndication>() {
            self.pli += 1;
        } else if packet.is::<FullIntraRequest>() {
            self.fir += 1;
        } else if let Some(report) = packet.downcast_ref::<ReceiverReport>() {
            self.receiver_reports += 1;
            // This sender owns one video track. Keep the latest report block;
            // include its SSRC so a source change is never presented as a delta.
            for block in &report.reports {
                self.latest = Some(ReceiverSample::new(block, now, received_at));
            }
        }
    }

    pub fn log_if_due(&mut self, now: Instant) {
        if self
            .last_log
            .is_some_and(|last| now.duration_since(last).as_secs() < 5)
        {
            return;
        }
        self.last_log = Some(now);
        info!(
            nack_packet_requests = self.nack_requests,
            pli = self.pli,
            fir = self.fir,
            receiver_reports = self.receiver_reports,
            "UU video feedback totals"
        );
        if let Some(report) = &self.latest {
            info!(ssrc = report.ssrc, last_sequence = report.last_sequence,
                total_lost = report.lost, fraction_lost_percent = report.loss_percent,
                jitter_ms = report.jitter_ms, rtcp_rtt_ms = ?report.rtt_ms,
                report_age_ms = now.duration_since(report.received_at).as_millis() as u64,
                "UU video receiver report");
        }
    }
}

#[derive(Default)]
pub(super) struct FrameAgeStats {
    pub frames: u64,
    pub total_queue_us: u64,
    pub max_queue_us: u64,
    pub max_encoded_to_send_us: u64,
    pub cached_frames: u64,
}

impl FrameAgeStats {
    pub fn observe(
        &mut self,
        encoded_at: Instant,
        sending_at: Instant,
        sent_at: Instant,
        subscribed_at: Instant,
    ) {
        let queue_us = sending_at.duration_since(encoded_at).as_micros() as u64;
        self.frames += 1;
        self.total_queue_us += queue_us;
        self.max_queue_us = self.max_queue_us.max(queue_us);
        self.max_encoded_to_send_us = self
            .max_encoded_to_send_us
            .max(sent_at.duration_since(encoded_at).as_micros() as u64);
        self.cached_frames += u64::from(encoded_at < subscribed_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use webrtc::rtcp::transport_feedbacks::transport_layer_nack::NackPair;

    #[test]
    fn session_snapshot_preserves_unknown_values_and_independent_sample_ages() {
        let now = Instant::now();
        let mut diagnostics = VideoDiagnostics::default();
        let empty = serde_json::to_value(diagnostics.snapshot(now)).unwrap();
        assert!(empty["receiver"].is_null());
        assert!(empty["sender"].is_null());
        diagnostics.feedback.observe(
            &ReceiverReport {
                reports: vec![ReceptionReport {
                    fraction_lost: 32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            UNIX_EPOCH,
            now,
        );
        diagnostics.sender = Some((
            now + Duration::from_secs(2),
            UuVideoSender {
                age_ms: 0.0,
                window_ms: 5000.0,
                fps: 60.0,
                mean_queue_ms: 0.25,
                max_queue_ms: 0.75,
                max_encoded_to_send_ms: 5.0,
                cached_frames: 0.0,
            },
        ));
        let snapshot = diagnostics.snapshot(now + Duration::from_secs(12));
        let receiver = snapshot.receiver.unwrap();
        assert_eq!(receiver.age_ms, 12000.0);
        assert_eq!(receiver.loss_percent, 12.5);
        assert!(receiver.rtt_ms.is_none());
        assert_eq!(snapshot.sender.unwrap().age_ms, 10000.0);
        // A new peer never inherits the previous peer's measurements.
        assert!(VideoDiagnostics::default().snapshot(now).receiver.is_none());
    }

    #[test]
    fn receiver_units_and_negative_loss_are_not_misreported() {
        let report = ReceptionReport {
            total_lost: 0x00ff_fffe,
            fraction_lost: 64,
            jitter: 900,
            ..Default::default()
        };
        let sample = ReceiverSample::new(&report, UNIX_EPOCH, Instant::now());
        assert_eq!(sample.lost, -2);
        assert_eq!(sample.loss_percent, 25.0);
        assert_eq!(sample.jitter_ms, 10.0);
        assert_eq!(sample.rtt_ms, None);
    }

    #[test]
    fn rtt_handles_compact_ntp_wrap_and_rejects_invalid_elapsed_time() {
        // NTP time 65536 seconds: the compact timestamp just wrapped to zero.
        let now = UNIX_EPOCH + Duration::from_secs(65536 - (2_208_988_800u64 % 65536));
        let mut report = ReceptionReport {
            last_sender_report: 0xffff_0000,
            delay: 32768,
            ..Default::default()
        };
        assert_eq!(report_rtt_ms(&report, now), Some(500.0));
        report.delay = 2 * 65536;
        assert_eq!(report_rtt_ms(&report, now), None);
        report.delay = 0;
        report.last_sender_report = 0u32.wrapping_sub(31 * 65536);
        assert_eq!(report_rtt_ms(&report, now), None);
        assert_eq!(
            report_rtt_ms(&report, UNIX_EPOCH - Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn feedback_counts_repeated_packet_requests_without_claiming_unique_loss() {
        let nack = TransportLayerNack {
            nacks: vec![NackPair {
                packet_id: u16::MAX,
                lost_packets: 0b101,
            }],
            ..Default::default()
        };
        let mut stats = VideoFeedback::default();
        let now = Instant::now();
        for _ in 0..2 {
            stats.observe(&nack, UNIX_EPOCH, now);
        }
        stats.observe(&PictureLossIndication::default(), UNIX_EPOCH, now);
        stats.observe(&FullIntraRequest::default(), UNIX_EPOCH, now);
        assert_eq!((stats.nack_requests, stats.pli, stats.fir), (6, 1, 1));
        assert!(stats.latest.is_none());
    }

    #[test]
    fn frame_age_includes_queue_and_send_but_retains_cached_frame_age() {
        let start = Instant::now();
        let mut stats = FrameAgeStats::default();
        stats.observe(
            start,
            start + Duration::from_millis(10),
            start + Duration::from_millis(15),
            start,
        );
        stats.observe(
            start,
            start + Duration::from_millis(100),
            start + Duration::from_millis(120),
            start + Duration::from_millis(50),
        );
        assert_eq!((stats.frames, stats.cached_frames), (2, 1));
        assert_eq!(stats.total_queue_us / stats.frames, 55_000);
        assert_eq!(stats.max_queue_us, 100_000);
        assert_eq!(stats.max_encoded_to_send_us, 120_000);
    }
}
