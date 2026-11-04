//! Sender-side transport-wide congestion control (TWCC FMT=15).
//! All media and retransmissions share a sequence space. This passive delay/loss
//! AIMD estimator adds no pacing queue or probe traffic; idle traffic cannot
//! establish spare capacity. It is not a port of Google's GCC implementation.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use webrtc::interceptor::{
    stream_info::StreamInfo, Attributes, Interceptor, InterceptorBuilder, RTCPReader, RTCPWriter,
    RTPReader, RTPWriter,
};
use webrtc::rtcp::transport_feedbacks::transport_layer_cc::{
    PacketStatusChunk, SymbolTypeTcc as Symbol, TransportLayerCc,
};
use webrtc::util::MarshalSize;

const URI: &str = "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";
const HISTORY: usize = 32_000; // Less than half a transport sequence cycle.
const FRESH: Duration = Duration::from_secs(3);
const TIMING_TIMEOUT: Duration = Duration::from_secs(2);
const HISTORY_AGE: Duration = Duration::from_secs(10);
type Result<T> = std::result::Result<T, webrtc::interceptor::Error>;

#[cfg(test)]
#[path = "twcc_tests.rs"]
mod tests;

#[derive(Clone, Default)]
pub(super) struct Transport(pub Arc<Mutex<Estimator>>);

#[derive(Clone, Copy)]
struct Sent {
    at: Instant,
    bytes: usize,
    // None: unreported; Some(false): missing, still eligible for a late ACK.
    received: Option<bool>,
    loss_epoch: u64,
    video: bool,
}

#[derive(Clone, Copy)]
struct Arrival {
    sent: Instant,
    received: i64,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Estimator {
    next: i64,
    history: BTreeMap<i64, Sent>,
    reference: Option<i64>,
    arrivals: VecDeque<Arrival>,
    arrival_bytes: usize,
    // Regression uses send-time groups, so a large fragmented frame does not
    // overwhelm the delay detector with thousands of identical send times.
    groups: VecDeque<(f64, f64)>,
    origin: Option<Arrival>,
    last_group: Option<Arrival>,
    target: u32,
    updated: Option<Instant>,
    timing_updated: Option<Instant>,
    decision: Option<Instant>,
    acknowledged_kbps: f64,
    loss_percent: f64,
    delay_slope: f64,
    smoothed_delay: f64,
    reports: u64,
    matched: u64,
    pending_total: u64,
    pending_lost: u64,
    loss_epoch: u64,
    video_received: u64,
    audio_received: u64,
}

/// Unwrap a counter around a known extended value, including reference time's
/// 24-bit wrap (~12 days). Sender and receiver clocks need not be synchronized.
fn unwrap(value: i64, near: i64, modulus: i64) -> i64 {
    near + (value - near + modulus / 2).rem_euclid(modulus) - modulus / 2
}

/// Validate the entire report before consuming history. Delta units provided by
/// the RTCP crate are microseconds, not wire units of 250 microseconds.
fn decode(report: &TransportLayerCc, reference: i64) -> Option<Vec<Option<i64>>> {
    let count = usize::from(report.packet_status_count);
    if count == 0 || count > HISTORY {
        return None;
    }
    let mut symbols = Vec::with_capacity(count);
    for chunk in &report.packet_chunks {
        if symbols.len() == count {
            return None;
        }
        match chunk {
            PacketStatusChunk::RunLengthChunk(c) => {
                let n = usize::from(c.run_length);
                if n == 0 || n > count - symbols.len() {
                    return None;
                }
                symbols.extend(std::iter::repeat_n(c.packet_status_symbol, n));
            }
            PacketStatusChunk::StatusVectorChunk(c) => {
                symbols.extend(c.symbol_list.iter().copied().take(count - symbols.len()));
            }
        }
    }
    if symbols.len() != count {
        return None;
    }
    let mut deltas = report.recv_deltas.iter();
    let mut time = reference * 64_000;
    let mut received = Vec::with_capacity(count);
    for symbol in symbols {
        match symbol {
            Symbol::PacketNotReceived => received.push(None),
            // Without timing, delivery can be counted but cannot estimate rate.
            Symbol::PacketReceivedWithoutDelta => received.push(Some(i64::MIN)),
            _ => {
                let delta = deltas.next()?;
                if delta.type_tcc_packet != symbol {
                    return None;
                }
                time = time.checked_add(delta.delta)?;
                received.push(Some(time));
            }
        }
    }
    if deltas.next().is_some() {
        return None;
    }
    Some(received)
}

impl Estimator {
    fn sent(&mut self, now: Instant, bytes: usize, video: bool) -> i64 {
        if self
            .history
            .last_key_value()
            .is_some_and(|(_, p)| now.duration_since(p.at) > FRESH)
        {
            self.reset_measurements();
        }
        while self.history.len() >= HISTORY
            || self
                .history
                .first_key_value()
                .is_some_and(|(_, p)| now.duration_since(p.at) > HISTORY_AGE)
        {
            self.history.pop_first();
        }
        let seq = self.next;
        self.next += 1;
        self.history.insert(
            seq,
            Sent {
                at: now,
                bytes,
                received: None,
                loss_epoch: 0,
                video,
            },
        );
        seq
    }

    fn reset_measurements(&mut self) {
        // Preserve transport sequence continuity across path/quality changes.
        let next = self.next;
        *self = Self {
            next,
            ..Self::default()
        };
    }

    fn receive(&mut self, report: &TransportLayerCc, now: Instant) {
        let base = unwrap(
            i64::from(report.base_sequence_number),
            self.next - 1,
            65_536,
        );
        let reference = unwrap(
            i64::from(report.reference_time),
            self.reference.unwrap_or(i64::from(report.reference_time)),
            1 << 24,
        );
        let Some(times) = decode(report, reference) else {
            return;
        };
        if base < 0 || base + times.len() as i64 > self.next {
            return;
        }
        let mut received = Vec::new();
        let mut changed = false;
        for (offset, time) in times.into_iter().enumerate() {
            let sequence = base + offset as i64;
            let Some(packet) = self.history.get_mut(&sequence) else {
                continue;
            };
            if packet.received == Some(true)
                || packet.received == Some(false) && time.is_none()
                || now.duration_since(packet.at) > HISTORY_AGE
            {
                continue;
            }
            changed = true;
            if packet.received.is_none() {
                self.matched += 1;
                self.pending_total += 1;
                self.pending_lost += u64::from(time.is_none());
                packet.loss_epoch = self.loss_epoch;
            } else if packet.loss_epoch == self.loss_epoch {
                // Correct only the still-open loss window. A late ACK is not
                // another transmitted packet and cannot undo an old decision.
                self.pending_lost -= 1;
            }
            packet.received = Some(time.is_some());
            if time.is_some() {
                if packet.video {
                    self.video_received += 1;
                } else {
                    self.audio_received += 1;
                }
            }
            match time {
                Some(time) if time != i64::MIN && now.duration_since(packet.at) <= FRESH => {
                    received.push(Arrival {
                        sent: packet.at,
                        received: time,
                        bytes: packet.bytes,
                    })
                }
                _ => (),
            }
        }
        if !changed {
            return;
        }
        self.reference = Some(self.reference.map_or(reference, |last| last.max(reference)));
        self.reports += 1;
        self.expire_timing(now);
        let mut new_timing = false;
        for arrival in received {
            if self.arrival(arrival) {
                self.timing_updated = Some(now);
                new_timing = true;
            }
        }
        // A whole feedback batch may be lost, so a valid loss-only report still
        // drives a decrease. Sparse batches alone must not drive loss control.
        if self.pending_total < 20 {
            return;
        }
        self.loss_percent = self.pending_lost as f64 * 100.0 / self.pending_total as f64;
        self.updated = Some(now);
        if self.video_received == 0 {
            // Some clients report only audio, even across transport sequence
            // gaps occupied by video. Such gaps cannot establish video loss.
            self.finish_loss_window();
            return;
        }
        if self.target == 0
            || self
                .decision
                .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
        {
            return;
        }
        self.finish_loss_window();
        let congested = self.loss_percent >= 10.0
            || (new_timing && self.groups.len() >= 10 && self.delay_slope > 12.5);
        if congested {
            let mut next = self.target as f64 * 0.85;
            // Low delivered rate while application-limited is not a capacity
            // estimate. Only cap against throughput when we are filling the link.
            if new_timing && self.acknowledged_kbps >= self.target as f64 * 0.5 {
                next = next.min(self.acknowledged_kbps * 0.9);
            }
            self.target = (next as u32).max(1000);
            self.decision = Some(now);
        } else if self.loss_percent < 2.0
            && new_timing
            && self.groups.len() >= 10
            && self.delay_slope < 5.0
            && self.acknowledged_kbps >= self.target as f64 * 0.8
        {
            self.target = self.target.saturating_add((self.target / 20).max(50));
            self.decision = Some(now);
        }
    }

    fn finish_loss_window(&mut self) {
        self.pending_total = 0;
        self.pending_lost = 0;
        self.loss_epoch += 1;
    }

    fn expire_timing(&mut self, now: Instant) {
        if self
            .timing_updated
            .is_some_and(|at| now.duration_since(at) > TIMING_TIMEOUT)
        {
            self.arrivals.clear();
            self.arrival_bytes = 0;
            self.groups.clear();
            self.origin = None;
            self.last_group = None;
            self.acknowledged_kbps = 0.0;
            self.delay_slope = 0.0;
            self.smoothed_delay = 0.0;
            self.timing_updated = None;
        }
    }

    // Only new, forward-moving delay groups can refresh the delay detector.
    fn arrival(&mut self, sample: Arrival) -> bool {
        let last = self.arrivals.back().copied().unwrap_or(sample);
        let reordered = sample.received < last.received || sample.sent < last.sent;
        // TWCC quantizes arrivals to 250 us: many fragments of a video frame
        // legitimately share a timestamp. Count every delivered byte. Attribute
        // reordered arrivals to the current rate window without feeding reversed
        // time deltas into the delay regression.
        let rate_sample = Arrival {
            sent: sample.sent.max(last.sent),
            received: sample.received.max(last.received),
            bytes: sample.bytes,
        };
        self.arrivals.push_back(rate_sample);
        self.arrival_bytes += sample.bytes;
        while self.arrivals.len() > HISTORY
            || (self.arrivals.len() > 2
                && rate_sample.received - self.arrivals[1].received > 500_000)
        {
            self.arrival_bytes -= self.arrivals.pop_front().unwrap().bytes;
        }
        let first = self.arrivals.front().unwrap();
        let receive_us = rate_sample.received - first.received;
        let send_us = rate_sample.sent.duration_since(first.sent).as_micros() as i64;
        if receive_us >= 100_000 && send_us >= 100_000 {
            let bytes = self.arrival_bytes - first.bytes;
            // Taking the larger interval prevents compressed feedback arrivals
            // from inventing bandwidth. Receiver times remove RTCP batching.
            self.acknowledged_kbps = bytes as f64 * 8000.0 / receive_us.max(send_us) as f64;
        }
        if reordered {
            return false;
        }
        let origin = *self.origin.get_or_insert(sample);
        if self
            .last_group
            .is_some_and(|last| sample.sent.duration_since(last.sent) < Duration::from_millis(5))
        {
            return false;
        }
        self.last_group = Some(sample);
        let sent_ms = sample.sent.duration_since(origin.sent).as_secs_f64() * 1000.0;
        let delay_ms = (sample.received - origin.received) as f64 / 1000.0 - sent_ms;
        self.smoothed_delay = self.smoothed_delay * 0.9 + delay_ms * 0.1;
        self.groups
            .push_back((sent_ms / 1000.0, self.smoothed_delay));
        if self.groups.len() > 20 {
            self.groups.pop_front();
        }
        let n = self.groups.len() as f64;
        let x = self.groups.iter().map(|p| p.0).sum::<f64>() / n;
        let y = self.groups.iter().map(|p| p.1).sum::<f64>() / n;
        let variance = self.groups.iter().map(|p| (p.0 - x).powi(2)).sum::<f64>();
        if variance > 0.0 {
            self.delay_slope = self
                .groups
                .iter()
                .map(|p| (p.0 - x) * (p.1 - y))
                .sum::<f64>()
                / variance;
        }
        true
    }
}

impl Transport {
    pub fn reset(&self) {
        self.0.lock().unwrap().reset_measurements();
    }

    /// A fresh estimate is authoritative even when no bitrate change is needed;
    /// RR must not independently increase the rate while TWCC holds it steady.
    pub fn budget(&self, now: Instant, current: u32, ceiling: u32) -> Option<u32> {
        let mut state = self.0.lock().unwrap();
        state.expire_timing(now);
        if state
            .updated
            .is_some_and(|at| now.duration_since(at) > FRESH)
        {
            state.reset_measurements();
        }
        // Audio feedback can arrive before the desktop encoder is ready and
        // before budget() runs for the first time.
        if state.target == 0 || state.updated.is_none() || state.video_received == 0 {
            state.target = current.saturating_mul(100) / 85;
        }
        state.target = state.target.min(ceiling.saturating_mul(100) / 85);
        if state.video_received == 0 {
            return None;
        }
        state
            .updated
            .filter(|t| now.duration_since(*t) <= FRESH)
            .map(|_| {
                state
                    .target
                    .saturating_mul(85)
                    .div_ceil(100)
                    .clamp(1000, ceiling.max(1000))
            })
    }

    pub fn snapshot(&self, now: Instant) -> Option<super::status::UuTwccEstimate> {
        let mut s = self.0.lock().unwrap();
        s.expire_timing(now);
        if s.target == 0 {
            return None;
        }
        Some(super::status::UuTwccEstimate {
            age_ms: now.duration_since(s.updated?).as_secs_f64() * 1000.0,
            transport_kbps: f64::from(s.target),
            acknowledged_kbps: s.acknowledged_kbps,
            loss_percent: s.loss_percent,
            delay_slope_ms_per_second: s.delay_slope,
            feedback_reports: s.reports as f64,
            matched_packets: s.matched as f64,
            video_received_packets: s.video_received as f64,
            audio_received_packets: s.audio_received as f64,
            usable_for_video: s.video_received > 0 && now.duration_since(s.updated?) <= FRESH,
        })
    }
}

impl InterceptorBuilder for Transport {
    fn build(&self, _id: &str) -> Result<Arc<dyn Interceptor + Send + Sync>> {
        Ok(Arc::new(self.clone()))
    }
}

#[async_trait]
impl Interceptor for Transport {
    async fn bind_rtcp_reader(
        &self,
        reader: Arc<dyn RTCPReader + Send + Sync>,
    ) -> Arc<dyn RTCPReader + Send + Sync> {
        Arc::new(FeedbackReader {
            transport: self.clone(),
            reader,
        })
    }
    async fn bind_rtcp_writer(
        &self,
        writer: Arc<dyn RTCPWriter + Send + Sync>,
    ) -> Arc<dyn RTCPWriter + Send + Sync> {
        writer
    }
    async fn bind_local_stream(
        &self,
        info: &StreamInfo,
        writer: Arc<dyn RTPWriter + Send + Sync>,
    ) -> Arc<dyn RTPWriter + Send + Sync> {
        let feedback = info.rtcp_feedback.iter().any(|f| f.typ == "transport-cc");
        tracing::info!(mime = %info.mime_type, transport_cc_feedback = feedback,
            transport_cc_extension = info.rtp_header_extensions.iter().find(|e| e.uri == URI).map(|e| e.id),
            "UU negotiated transport feedback");
        if !feedback {
            return writer;
        }
        let Some(extension) = info
            .rtp_header_extensions
            .iter()
            .find(|e| e.uri == URI && (1..=255).contains(&e.id))
        else {
            return writer;
        };
        Arc::new(PacketWriter {
            transport: self.clone(),
            writer,
            extension: extension.id as u8,
            video: info.mime_type.to_ascii_lowercase().starts_with("video/"),
        })
    }
    async fn unbind_local_stream(&self, _info: &StreamInfo) {}
    async fn bind_remote_stream(
        &self,
        _info: &StreamInfo,
        reader: Arc<dyn RTPReader + Send + Sync>,
    ) -> Arc<dyn RTPReader + Send + Sync> {
        reader
    }
    async fn unbind_remote_stream(&self, _info: &StreamInfo) {}
    async fn close(&self) -> Result<()> {
        self.reset();
        Ok(())
    }
}

struct PacketWriter {
    transport: Transport,
    writer: Arc<dyn RTPWriter + Send + Sync>,
    extension: u8,
    video: bool,
}

#[async_trait]
impl RTPWriter for PacketWriter {
    async fn write(
        &self,
        packet: &webrtc::rtp::packet::Packet,
        attributes: &Attributes,
    ) -> Result<usize> {
        let mut packet = packet.clone();
        // Reserve the extension before recording size. No mutex is held over IO.
        packet
            .header
            .set_extension(self.extension, Bytes::from_static(&[0, 0]))?;
        let sequence = self.transport.0.lock().unwrap().sent(
            Instant::now(),
            packet.marshal_size(),
            self.video,
        );
        packet.header.set_extension(
            self.extension,
            Bytes::copy_from_slice(&(sequence as u16).to_be_bytes()),
        )?;
        let result = self.writer.write(&packet, attributes).await;
        if result.is_err() {
            self.transport.0.lock().unwrap().history.remove(&sequence);
        }
        result
    }
}

struct FeedbackReader {
    transport: Transport,
    reader: Arc<dyn RTCPReader + Send + Sync>,
}

#[async_trait]
impl RTCPReader for FeedbackReader {
    async fn read(
        &self,
        buf: &mut [u8],
        attributes: &Attributes,
    ) -> Result<(
        Vec<Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>>,
        Attributes,
    )> {
        let result = self.reader.read(buf, attributes).await?;
        let now = Instant::now();
        for packet in &result.0 {
            if let Some(report) = packet.as_any().downcast_ref::<TransportLayerCc>() {
                self.transport.0.lock().unwrap().receive(report, now);
            }
        }
        Ok(result)
    }
}
