use super::*;
use webrtc::rtcp::transport_feedbacks::transport_layer_cc::{RecvDelta, RunLengthChunk};
use webrtc::util::{Marshal, Unmarshal};

fn feedback(base: u16, count: u16, reference: u32, delta: i64) -> TransportLayerCc {
    TransportLayerCc {
        base_sequence_number: base,
        packet_status_count: count,
        reference_time: reference,
        packet_chunks: vec![PacketStatusChunk::RunLengthChunk(RunLengthChunk {
            packet_status_symbol: Symbol::PacketReceivedLargeDelta,
            run_length: count,
            ..Default::default()
        })],
        recv_deltas: (0..count)
            .map(|_| RecvDelta {
                type_tcc_packet: Symbol::PacketReceivedLargeDelta,
                delta,
            })
            .collect(),
        ..Default::default()
    }
}

fn batch(s: &mut Estimator, start: Instant, receive_delta: i64) {
    let base = s.next as u16;
    for i in 0..100 {
        s.sent(start + Duration::from_millis(i * 5), 1200);
    }
    s.receive(
        &feedback(base, 100, (s.next / 100 * 16) as u32, receive_delta),
        start + Duration::from_millis(600),
    );
}

#[test]
fn queue_growth_reduces_rate_before_loss() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 2000,
        ..Default::default()
    };
    batch(&mut s, start, 6000);
    assert!(s.delay_slope > 100.0);
    assert!(s.target < 2000);
    assert_eq!(s.loss_percent, 0.0);
}

#[test]
fn healthy_full_link_recovers_but_idle_does_not_probe() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 2000,
        ..Default::default()
    };
    batch(&mut s, start, 5000);
    assert_eq!(s.target, 2100);
    let mut idle = Estimator {
        target: 8000,
        ..Default::default()
    };
    batch(&mut idle, start, 5000);
    assert_eq!(idle.target, 8000);
    assert!(idle.acknowledged_kbps < 2000.0);
}

#[test]
fn duplicate_and_reordered_feedback_cannot_reduce_twice() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 8000,
        ..Default::default()
    };
    batch(&mut s, start, 6000);
    let target = s.target;
    let reports = s.reports;
    s.receive(&feedback(0, 100, 16, 6000), start + Duration::from_secs(1));
    s.receive(&feedback(0, 50, 16, 6000), start + Duration::from_secs(2));
    assert_eq!((s.target, s.reports, s.matched), (target, reports, 100));
}

#[test]
fn sequence_and_reference_wrap_and_negative_deltas() {
    let start = Instant::now();
    let mut s = Estimator {
        next: 65_530,
        reference: Some((1 << 24) - 1),
        target: 8000,
        ..Default::default()
    };
    for i in 0..30 {
        s.sent(start + Duration::from_millis(i * 5), 1200);
    }
    let mut report = feedback(65_530, 30, 0, 5000);
    report.recv_deltas[0].delta = -250;
    s.receive(&report, start + Duration::from_secs(1));
    assert_eq!(s.matched, 30);
    assert_eq!(s.reference, Some(1 << 24));
    assert_eq!(s.last_sequence, Some(65_559));
}

#[test]
fn malformed_or_future_report_does_not_consume_history() {
    let start = Instant::now();
    let mut s = Estimator::default();
    for _ in 0..30 {
        s.sent(start, 1200);
    }
    let mut malformed = feedback(0, 30, 0, 5000);
    malformed.recv_deltas.pop();
    s.receive(&malformed, start);
    s.receive(&feedback(20, 30, 0, 5000), start);
    assert_eq!(s.matched, 0);
    assert!(s.history.values().all(|p| !p.reported));
    s.receive(&feedback(0, 30, 0, 5000), start);
    assert_eq!(s.matched, 30);
}

#[test]
fn loss_only_feedback_and_sparse_reports_are_usable() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 8000,
        ..Default::default()
    };
    for _ in 0..20 {
        s.sent(start, 1200);
    }
    for base in [0, 10] {
        let mut report = feedback(base, 10, 0, 0);
        report.recv_deltas.clear();
        if let PacketStatusChunk::RunLengthChunk(c) = &mut report.packet_chunks[0] {
            c.packet_status_symbol = Symbol::PacketNotReceived;
        }
        s.receive(&report, start);
    }
    assert_eq!(s.target, 6800);
    assert_eq!(s.loss_percent, 100.0);
}

#[test]
fn expiry_idle_and_path_reset_keep_sequence_but_clear_capacity() {
    let start = Instant::now();
    let transport = Transport::default();
    assert_eq!(transport.budget(start, 8000, 8000), None);
    batch(&mut transport.0.lock().unwrap(), start, 6000);
    assert!(
        transport
            .budget(start + Duration::from_secs(1), 8000, 8000)
            .unwrap()
            < 8000
    );
    assert_eq!(
        transport.budget(start + Duration::from_secs(4), 8000, 8000),
        None
    );
    let mut s = transport.0.lock().unwrap();
    let next = s.next;
    s.sent(start + Duration::from_secs(5), 100);
    assert_eq!(s.next, next + 1);
    assert!(s.updated.is_none());
    s.reset_measurements();
    assert_eq!(s.next, next + 1);
    assert!(s.history.is_empty());
}

#[test]
fn history_is_bounded_and_feedback_compression_cannot_inflate_rate() {
    let start = Instant::now();
    let mut s = Estimator::default();
    for _ in 0..HISTORY + 10 {
        s.sent(start, 1200);
    }
    assert_eq!(s.history.len(), HISTORY);
    let mut s = Estimator {
        target: 8000,
        ..Default::default()
    };
    batch(&mut s, start, 2500);
    assert!((s.acknowledged_kbps - 1920.0).abs() < 0.01);
    assert_eq!(s.target, 8000);
}

#[derive(Default)]
struct Sink(Mutex<Vec<webrtc::rtp::packet::Packet>>);
#[async_trait]
impl RTPWriter for Sink {
    async fn write(&self, packet: &webrtc::rtp::packet::Packet, _: &Attributes) -> Result<usize> {
        self.0.lock().unwrap().push(packet.clone());
        Ok(packet.marshal_size())
    }
}

#[tokio::test]
async fn audio_video_and_retransmission_share_sequence_without_changing_rtp() {
    let transport = Transport::default();
    let sink = Arc::new(Sink::default());
    let mut info = StreamInfo::default();
    let bare = transport.bind_local_stream(&info, sink.clone()).await;
    let mut packet = webrtc::rtp::packet::Packet::default();
    packet.header.sequence_number = 42;
    packet
        .header
        .set_extension(3, Bytes::from_static(&[7]))
        .unwrap();
    bare.write(&packet, &Attributes::new()).await.unwrap();
    info.rtp_header_extensions
        .push(webrtc::interceptor::stream_info::RTPHeaderExtension {
            uri: URI.into(),
            id: 5,
        });
    let video = transport.bind_local_stream(&info, sink.clone()).await;
    info.ssrc = 2;
    let audio = transport.bind_local_stream(&info, sink.clone()).await;
    video.write(&packet, &Attributes::new()).await.unwrap();
    audio.write(&packet, &Attributes::new()).await.unwrap();
    video.write(&packet, &Attributes::new()).await.unwrap();
    let packets = sink.0.lock().unwrap();
    assert!(packets[0].header.get_extension(5).is_none());
    for (i, packet) in packets[1..].iter().enumerate() {
        assert_eq!(
            packet.header.get_extension(5).unwrap().as_ref(),
            &(i as u16).to_be_bytes()
        );
        assert_eq!(packet.header.sequence_number, 42);
        assert_eq!(packet.header.get_extension(3).unwrap().as_ref(), &[7]);
    }
}

#[test]
fn wire_round_trip_handles_vector_padding_loss_and_untimed_delivery() {
    use webrtc::rtcp::transport_feedbacks::transport_layer_cc::{
        StatusChunkTypeTcc, StatusVectorChunk, SymbolSizeTypeTcc,
    };
    let report = TransportLayerCc {
        base_sequence_number: 65535,
        packet_status_count: 4,
        reference_time: 123,
        packet_chunks: vec![PacketStatusChunk::StatusVectorChunk(StatusVectorChunk {
            type_tcc: StatusChunkTypeTcc::StatusVectorChunk,
            symbol_size: SymbolSizeTypeTcc::TwoBit,
            symbol_list: vec![
                Symbol::PacketReceivedSmallDelta,
                Symbol::PacketNotReceived,
                Symbol::PacketReceivedLargeDelta,
                Symbol::PacketReceivedWithoutDelta,
            ],
        })],
        recv_deltas: vec![
            RecvDelta {
                type_tcc_packet: Symbol::PacketReceivedSmallDelta,
                delta: 500,
            },
            RecvDelta {
                type_tcc_packet: Symbol::PacketReceivedLargeDelta,
                delta: -250,
            },
        ],
        ..Default::default()
    };
    let mut bytes = report.marshal().unwrap();
    let parsed = TransportLayerCc::unmarshal(&mut bytes).unwrap();
    assert_eq!(
        decode(&parsed, 123).unwrap(),
        vec![
            Some(123 * 64000 + 500),
            None,
            Some(123 * 64000 + 250),
            Some(i64::MIN)
        ]
    );
}

struct FailingWriter;
#[async_trait]
impl RTPWriter for FailingWriter {
    async fn write(&self, _: &webrtc::rtp::packet::Packet, _: &Attributes) -> Result<usize> {
        Err(webrtc::interceptor::Error::ErrIoEOF)
    }
}

#[tokio::test]
async fn failed_send_is_not_network_loss() {
    let transport = Transport::default();
    let writer = PacketWriter {
        transport: transport.clone(),
        writer: Arc::new(FailingWriter),
        extension: 5,
    };
    assert!(writer
        .write(&Default::default(), &Attributes::new())
        .await
        .is_err());
    let state = transport.0.lock().unwrap();
    assert_eq!(state.next, 1);
    assert!(state.history.is_empty());
}

#[test]
fn overlapping_feedback_counts_each_transport_packet_once() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 8000,
        ..Default::default()
    };
    for i in 0..60 {
        s.sent(start + Duration::from_millis(i * 5), 1200);
    }
    s.receive(&feedback(0, 40, 0, 5000), start + Duration::from_secs(1));
    s.receive(&feedback(20, 40, 1, 5000), start + Duration::from_secs(1));
    assert_eq!(s.matched, 60);
    assert_eq!(s.reports, 2);
    assert_eq!(s.loss_percent, 0.0);
}

#[test]
fn capacity_recovers_after_a_congested_period() {
    let start = Instant::now();
    let mut s = Estimator {
        target: 2000,
        ..Default::default()
    };
    let mut receive_us = 0_i64;
    let mut send_us = 0_u64;
    let mut lowest = s.target;
    for batch_index in 0..20 {
        let base = s.next as u16;
        let delta = if batch_index < 3 { 6000 } else { 5000 };
        let mut report = feedback(base, 100, (receive_us / 64000) as u32, delta);
        report.recv_deltas[0].delta += receive_us % 64000;
        for _ in 0..100 {
            s.sent(start + Duration::from_micros(send_us), 1200);
            send_us += 5000;
        }
        receive_us += delta * 100;
        s.receive(&report, start + Duration::from_micros(send_us + 100_000));
        lowest = lowest.min(s.target);
    }
    assert!(lowest < 1800);
    assert!(s.target > lowest + 300);
    assert!(s.delay_slope.abs() < 1.0);
}

#[test]
fn first_twcc_budget_tracks_rr_fallback_and_remb_caps_recovery() {
    let start = Instant::now();
    let transport = Transport::default();
    assert_eq!(transport.budget(start, 8000, 8000), None);
    assert_eq!(transport.budget(start, 4000, 8000), None);
    batch(&mut transport.0.lock().unwrap(), start, 5000);
    let now = start + Duration::from_secs(1);
    assert!(transport.budget(now, 4000, 8000).unwrap() <= 4000);
    assert!(transport.budget(now, 4000, 2000).unwrap() <= 2000);
    assert!(transport.budget(now, 2000, 8000).unwrap() <= 2000);
}

#[tokio::test]
async fn nack_responder_retransmits_through_transport_numbering() {
    use webrtc::interceptor::stream_info::{RTCPFeedback, RTPHeaderExtension};
    use webrtc::interceptor::{
        mock::mock_stream::MockStream, nack::responder::Responder, registry::Registry,
    };
    use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack};
    let transport = Transport::default();
    let mut registry = Registry::new();
    registry.add(Box::new(transport.clone()));
    registry.add(Box::new(Responder::builder()));
    let stream = MockStream::new(
        &StreamInfo {
            ssrc: 1,
            rtcp_feedback: vec![RTCPFeedback {
                typ: "nack".into(),
                ..Default::default()
            }],
            rtp_header_extensions: vec![RTPHeaderExtension {
                uri: URI.into(),
                id: 5,
            }],
            ..Default::default()
        },
        registry.build("test").unwrap(),
    )
    .await;
    let mut packet = webrtc::rtp::packet::Packet::default();
    packet.header.sequence_number = 42;
    stream.write_rtp(&packet).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(1), stream.written_rtp())
        .await
        .unwrap()
        .unwrap();
    stream
        .receive_rtcp(vec![Box::new(TransportLayerNack {
            media_ssrc: 1,
            sender_ssrc: 2,
            nacks: vec![NackPair {
                packet_id: 42,
                lost_packets: 0,
            }],
        })])
        .await;
    let retransmission = tokio::time::timeout(Duration::from_secs(1), stream.written_rtp())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first.header.sequence_number,
        retransmission.header.sequence_number
    );
    assert_ne!(
        first.header.get_extension(5),
        retransmission.header.get_extension(5)
    );
    assert_eq!(transport.0.lock().unwrap().history.len(), 2);
    stream.close().await.unwrap();
}
