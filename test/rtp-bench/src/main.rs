//! Replay saved encoded AUs through production packetization and real SRTP.
//! No capture, codec, gadget or application service is opened.
#![allow(dead_code)]

use anyhow::{bail, ensure, Context as _, Result};
use bytes::Bytes;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use webrtc::api::{
    interceptor_registry::register_default_interceptors, media_engine::MediaEngine,
    setting_engine::SettingEngine, APIBuilder,
};
use webrtc::dtls::extension::extension_use_srtp::SrtpProtectionProfile;
use webrtc::ice::network_type::NetworkType;
use webrtc::ice::udp_mux::{UDPMux, UDPMuxDefault, UDPMuxParams};
use webrtc::ice::udp_network::UDPNetwork;
use webrtc::interceptor::{
    self, registry::Registry, Interceptor, InterceptorBuilder, RTPWriter, RTPWriterFn,
};
use webrtc::peer_connection::{
    configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
    RTCPeerConnection,
};
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability, RTPCodecType,
};
use webrtc::srtp::{context::Context, protection_profile::ProtectionProfile};
use webrtc::util::{Marshal, MarshalSize};

// These three modules are the production implementations, not benchmark copies.
#[path = "../../../src/video/codec/h264_bitstream.rs"]
pub mod h264_bitstream;
#[path = "../../../src/webrtc/h265_payloader.rs"]
pub mod h265_payloader;
#[path = "../../../src/webrtc/video_track.rs"]
pub mod video_track;

// Only non-I/O application types are adapted; no timing operation lives here.
pub mod error {
    #[derive(Debug)]
    pub enum AppError {
        WebRtcError(String),
    }
    impl std::fmt::Display for AppError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for AppError {}
    pub type Result<T> = std::result::Result<T, AppError>;
}
pub mod video {
    pub mod codec {
        pub use crate::h264_bitstream;
    }
    pub mod types {
        #[derive(Clone, Copy, Debug)]
        pub struct Resolution {
            pub width: u32,
            pub height: u32,
        }
        impl Resolution {
            pub const HD720: Self = Self {
                width: 1280,
                height: 720,
            };
            pub const HD1080: Self = Self {
                width: 1920,
                height: 1080,
            };
        }
    }
}
use video_track::{UniversalVideoTrack, UniversalVideoTrackConfig, VideoCodec};

#[derive(Clone)]
struct Fixture {
    codec: VideoCodec,
    index: usize,
    pts: u64,
    data: Bytes,
    packets: usize,
    hash: String,
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

// Linux counters are scoped to this process's network namespace, not this
// socket. Run alone; deltas identify kernel drops without changing socket sizes.
fn udp_counters() -> Option<BTreeMap<String, u64>> {
    let contents = std::fs::read_to_string("/proc/net/snmp").ok()?;
    let mut udp = contents
        .lines()
        .filter_map(|line| line.strip_prefix("Udp: "));
    let names = udp.next()?.split_whitespace();
    let values = udp.next()?.split_whitespace();
    names
        .zip(values)
        .map(|(name, value)| Some((name.to_owned(), value.parse().ok()?)))
        .collect()
}

fn udp_delta(
    before: &Option<BTreeMap<String, u64>>,
    after: &Option<BTreeMap<String, u64>>,
) -> Value {
    match (before, after) {
        (Some(before), Some(after)) => json!(after
            .iter()
            .filter_map(|(key, value)| Some((key, value.checked_sub(*before.get(key)?)?)))
            .collect::<BTreeMap<_, _>>()),
        _ => Value::Null,
    }
}

fn payloads(codec: VideoCodec, data: &Bytes) -> Result<Vec<Bytes>> {
    use rtp::packetizer::Payloader;
    Ok(match codec {
        VideoCodec::H264 => rtp::codecs::h264::H264Payloader::default()
            .payload(1188, &h264_bitstream::normalize_annex_b(data.clone()))?,
        VideoCodec::H265 => h265_payloader::H265Payloader::new().payload(1200, data),
        _ => unreachable!(),
    })
}

fn load_fixtures(path: &Path, codec: VideoCodec) -> Result<(Vec<Fixture>, Value)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let targets = [38usize, 48, 180, 220];
    let mut selected: Vec<Option<Fixture>> = vec![None; targets.len()];
    let mut offset = 0;
    let mut aus = 0;
    loop {
        let mut header = [0; 16];
        let n = file.read(&mut header)?;
        if n == 0 {
            break;
        }
        ensure!(n == 16, "truncated AU header");
        let pts = u64::from_le_bytes(header[..8].try_into()?);
        let size = u64::from_le_bytes(header[8..].try_into()?);
        ensure!((1..=16 * 1024 * 1024).contains(&size), "invalid AU size");
        let mut data = vec![0; size as usize];
        file.read_exact(&mut data)?;
        hasher.update(header);
        hasher.update(&data);
        let data = Bytes::from(data);
        // Use actual payloaders during the existing input pass. AU size alone
        // misses parameter-set packets and multi-NAL fragmentation boundaries.
        let packets = payloads(codec, &data)?.len();
        for (target, best) in targets.iter().zip(&mut selected) {
            if best
                .as_ref()
                .is_none_or(|best| packets.abs_diff(*target) < best.packets.abs_diff(*target))
            {
                *best = Some(Fixture {
                    codec,
                    index: aus,
                    pts,
                    hash: digest(&data),
                    data: data.clone(),
                    packets,
                });
            }
        }
        aus += 1;
        offset += size + 16;
    }
    ensure!(aus != 0, "empty fixture file");
    let mut seen = HashSet::new();
    let selected = selected
        .into_iter()
        .flatten()
        .filter(|fixture| seen.insert(fixture.index))
        .collect();
    Ok((
        selected,
        json!({"path":path,"sha256":hex(&hasher.finalize()),"bytes":offset,"aus":aus}),
    ))
}

fn packetize(f: &Fixture, sequence: &mut u16, timestamp: u32) -> Result<Vec<rtp::packet::Packet>> {
    let payloads = payloads(f.codec, &f.data)?;
    let count = payloads.len();
    Ok(payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let packet = rtp::packet::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: if f.codec == VideoCodec::H264 { 108 } else { 49 },
                    sequence_number: *sequence,
                    timestamp,
                    ssrc: 17,
                    marker: i + 1 == count,
                    ..Default::default()
                },
                payload,
            };
            *sequence = sequence.wrapping_add(1);
            packet
        })
        .collect())
}

fn distribution(raw: &[f64]) -> Value {
    let mut v = raw.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    json!({"n":n,"mean_us":v.iter().sum::<f64>()/n as f64,"p50_us":v[n/2],"p95_us":v[(n as f64*0.95).ceil() as usize-1],"p99_us":v[(n as f64*0.99).ceil() as usize-1],"max_us":v[n-1],"raw_us":raw})
}

fn profile_name(p: ProtectionProfile) -> &'static str {
    match p {
        ProtectionProfile::Aes128CmHmacSha1_80 => "AES_CM_128_HMAC_SHA1_80",
        ProtectionProfile::AeadAes128Gcm => "AEAD_AES_128_GCM",
        _ => unreachable!(),
    }
}

#[derive(Default, Debug)]
struct Counts {
    packets: AtomicU64,
    bytes: AtomicU64,
    markers: AtomicU64,
    duplicates: AtomicU64,
    seen: Mutex<HashSet<(u32, u16)>>,
}
impl Counts {
    fn observe(&self, p: &rtp::packet::Packet) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(p.marshal_size() as u64, Ordering::Relaxed);
        if p.header.marker {
            self.markers.fetch_add(1, Ordering::Relaxed);
        }
        if !self
            .seen
            .lock()
            .unwrap()
            .insert((p.header.timestamp, p.header.sequence_number))
        {
            self.duplicates.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> Value {
        json!({"packets":self.packets.load(Ordering::Relaxed),"markers":self.markers.load(Ordering::Relaxed),"duplicates":self.duplicates.load(Ordering::Relaxed)})
    }

    fn reset(&self) {
        self.packets.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        self.markers.store(0, Ordering::Relaxed);
        self.duplicates.store(0, Ordering::Relaxed);
        self.seen.lock().unwrap().clear();
    }
}
struct SinkBuilder(Arc<Counts>);
impl InterceptorBuilder for SinkBuilder {
    fn build(
        &self,
        _: &str,
    ) -> std::result::Result<Arc<dyn Interceptor + Send + Sync>, interceptor::Error> {
        let counts = self.0.clone();
        Ok(Arc::new(
            interceptor::mock::mock_interceptor::MockInterceptor {
                bind_local_stream_fn: Some(Box::new(move |_, _| {
                    let counts = counts.clone();
                    Box::pin(async move {
                        Arc::new(RTPWriterFn(Box::new(move |packet, _| {
                            counts.observe(packet);
                            let n = packet.marshal_size();
                            Box::pin(async move { Ok(n) })
                        }))) as Arc<dyn RTPWriter + Send + Sync>
                    })
                })),
                ..Default::default()
            },
        ))
    }
}

async fn make_peer(
    codec: VideoCodec,
    profile: ProtectionProfile,
    sink: Option<Arc<Counts>>,
    defaults: bool,
) -> Result<RTCPeerConnection> {
    make_peer_options(codec, vec![profile], sink, defaults, false, true, None).await
}

async fn make_peer_options(
    codec: VideoCodec,
    profiles: Vec<ProtectionProfile>,
    sink: Option<Arc<Counts>>,
    defaults: bool,
    server_role: bool,
    loopback_only: bool,
    udp_network: Option<UDPNetwork>,
) -> Result<RTCPeerConnection> {
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    if codec == VideoCodec::H265 {
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: codec.mime_type().into(),
                    clock_rate: 90000,
                    sdp_fmtp_line: codec.sdp_fmtp(),
                    rtcp_feedback: vec![
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "goog-remb".into(),
                            parameter: "".into(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "ccm".into(),
                            parameter: "fir".into(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "nack".into(),
                            parameter: "".into(),
                        },
                        webrtc::rtp_transceiver::RTCPFeedback {
                            typ: "nack".into(),
                            parameter: "pli".into(),
                        },
                    ],
                    ..Default::default()
                },
                payload_type: 49,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
    }
    media.register_header_extension(
        RTCRtpHeaderExtensionCapability {
            uri: "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay".into(),
        },
        RTPCodecType::Video,
        None,
    )?;
    let mut registry = Registry::new();
    // Registered first: the default writers wrap this terminal counting sink.
    if let Some(c) = sink {
        registry.add(Box::new(SinkBuilder(c)));
    }
    if defaults {
        registry = register_default_interceptors(registry, &mut media)?;
    }
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    settings.set_ip_filter(Box::new(move |ip| {
        ip.is_loopback()
            || (!loopback_only && matches!(ip,std::net::IpAddr::V4(v4) if v4.is_private()))
    }));
    settings.set_network_types(vec![NetworkType::Udp4]);
    if let Some(network) = udp_network {
        settings.set_udp_network(network);
    }
    if !profiles.is_empty() {
        settings.set_srtp_protection_profiles(
            profiles
                .iter()
                .map(|profile| match profile {
                    ProtectionProfile::Aes128CmHmacSha1_80 => {
                        SrtpProtectionProfile::Srtp_Aes128_Cm_Hmac_Sha1_80
                    }
                    ProtectionProfile::AeadAes128Gcm => {
                        SrtpProtectionProfile::Srtp_Aead_Aes_128_Gcm
                    }
                    _ => unreachable!(),
                })
                .collect(),
        );
    }
    if server_role {
        settings.set_answering_dtls_role(webrtc::dtls_transport::dtls_role::DTLSRole::Server)?;
    }
    Ok(APIBuilder::new()
        .with_media_engine(media)
        .with_setting_engine(settings)
        .with_interceptor_registry(registry)
        .build()
        .new_peer_connection(RTCConfiguration::default())
        .await?)
}

async fn connect(sender: &RTCPeerConnection, receiver: &RTCPeerConnection) -> Result<()> {
    let mut gathered = sender.gathering_complete_promise().await;
    sender
        .set_local_description(sender.create_offer(None).await?)
        .await?;
    gathered.recv().await;
    receiver
        .set_remote_description(sender.local_description().await.context("missing offer")?)
        .await?;
    let mut gathered = receiver.gathering_complete_promise().await;
    receiver
        .set_local_description(receiver.create_answer(None).await?)
        .await?;
    gathered.recv().await;
    sender
        .set_remote_description(
            receiver
                .local_description()
                .await
                .context("missing answer")?,
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        while sender.connection_state() != RTCPeerConnectionState::Connected
            || receiver.connection_state() != RTCPeerConnectionState::Connected
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("loopback ICE/DTLS timeout")?;
    Ok(())
}

async fn full_stack(
    fixtures: &[Fixture],
    profile: ProtectionProfile,
    mode: &str,
    rounds: usize,
    iterations: usize,
) -> Result<Value> {
    eprintln!(
        "{} {} {mode}",
        fixtures[0].codec.mime_type(),
        profile_name(profile)
    );
    let counts = Arc::new(Counts::default());
    let udp_before = udp_counters();
    let mock = matches!(mode, "track_mock" | "interceptors_mock");
    let defaults = mode != "track_mock";
    let sender = make_peer(
        fixtures[0].codec,
        profile,
        mock.then(|| counts.clone()),
        defaults,
    )
    .await?;
    let (receiver_mux, receive_buffer) = if mode == "peer_udp_buffered" {
        let (socket, actual_buffer) = buffered_udp_receiver()?;
        (
            Some(UDPMuxDefault::new(UDPMuxParams::new(socket))),
            Some(actual_buffer),
        )
    } else {
        (None, None)
    };
    let receiver = make_peer_options(
        fixtures[0].codec,
        vec![profile],
        None,
        defaults,
        false,
        true,
        receiver_mux
            .as_ref()
            .map(|mux| UDPNetwork::Muxed(mux.clone())),
    )
    .await?;
    let observed = counts.clone();
    receiver.on_track(Box::new(move |track, _, _| {
        let counts = observed.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                while let Ok((p, _)) = track.read_rtp().await {
                    counts.observe(&p);
                }
            });
        })
    }));
    let track = Arc::new(UniversalVideoTrack::new(UniversalVideoTrackConfig {
        codec: fixtures[0].codec,
        fps: 60,
        ..Default::default()
    }));
    let rtp_sender = sender.add_track(track.as_track_local()).await?;
    tokio::spawn(async move {
        let mut b = [0; 1500];
        while rtp_sender.read(&mut b).await.is_ok() {}
    });
    let result = async {
        connect(&sender, &receiver).await?;
        // Connected is a DTLS state, not an acknowledgement that the remote track
        // reader has consumed media. Observe a complete setup frame before timing.
        let mut setup_frames = 0i64;
        for _ in 0..30 {
            setup_frames += 1;
            track
                .write_frame_bytes_at(fixtures[0].data.clone(), false, Some(setup_frames * 17))
                .await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
            if counts.markers.load(Ordering::Relaxed) > 0 {
                break;
            }
        }
        ensure!(
            counts.markers.load(Ordering::Relaxed) > 0,
            "no setup frame delivered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let setup = json!({
            "sent_frames": setup_frames,
            "sent_packets": setup_frames as usize * fixtures[0].packets,
            "observed": counts.snapshot()
        });
        counts.reset();
        let stats = serde_json::to_value(sender.get_stats().await)?;
        let mut cases = Vec::new();
        let mut frame_index = setup_frames;
        let mut expected_packets = 0u64;
        let mut expected_frames = 0u64;
        for round in 0..rounds {
            for f in fixtures {
                let mut raw = Vec::new();
                for iteration in 0..iterations + 10 {
                    frame_index += 1;
                    let started = Instant::now();
                    track
                        .write_frame_bytes_at(f.data.clone(), false, Some(frame_index * 17))
                        .await?;
                    let elapsed = started.elapsed().as_secs_f64() * 1e6;
                    expected_packets += f.packets as u64;
                    expected_frames += 1;
                    if iteration >= 10 {
                        raw.push(elapsed);
                    }
                    // Outside timing: bound bursts and make every round's loss observable.
                    if !mock {
                        tokio::time::sleep(Duration::from_micros(16667)).await;
                    }
                }
                let mut result = distribution(&raw);
                result["round"] = json!(round);
                result["fixture_index"] = json!(f.index);
                result["packets_per_frame"] = json!(f.packets);
                result["au_bytes"] = json!(f.data.len());
                cases.push(result);
            }
        }
        let drained = tokio::time::timeout(Duration::from_secs(5), async {
            while counts.markers.load(Ordering::Relaxed) < expected_frames {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let actual_packets = counts.packets.load(Ordering::Relaxed);
        let duplicate_packets = counts.duplicates.load(Ordering::Relaxed);
        let valid = drained.is_ok() && actual_packets == expected_packets && duplicate_packets == 0;
        let udp_after = udp_counters();
        // Keep raw samples and receiver evidence when delivery fails. Lost
        // packets must not turn sender timings into a valid baseline.
        Ok(json!({
            "stage": mode,
            "profile": profile_name(profile),
            "codec": fixtures[0].codec.mime_type(),
            "cases": cases,
            "expected_packets": expected_packets,
            "actual_packets": actual_packets,
            "expected_frames": expected_frames,
            "actual_frames": counts.markers.load(Ordering::Relaxed),
            "duplicate_packets": duplicate_packets,
            "setup": setup,
            "failed": !valid,
            "frame_drain_timed_out": drained.is_err(),
            "receiver_buffer_bytes": receive_buffer,
            "receiver_udp_network": if receiver_mux.is_some() { "owned-mux" } else { "default-ephemeral" },
            "udp_namespace_before": udp_before,
            "udp_namespace_after": udp_after,
            "udp_namespace_delta": udp_delta(&udp_before, &udp_after),
            "stats_before": stats,
            "stats_after": serde_json::to_value(sender.get_stats().await)?,
            "receiver_stats": serde_json::to_value(receiver.get_stats().await)?
        }))
    }
    .await;
    sender.close().await?;
    receiver.close().await?;
    if let Some(mux) = receiver_mux {
        mux.close().await?;
    }
    result
}

struct ReceiverTask(tokio::task::JoinHandle<()>);
impl Drop for ReceiverTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn buffered_udp_receiver() -> Result<(tokio::net::UdpSocket, usize)> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_recv_buffer_size(4 * 1024 * 1024)?;
    let actual_buffer = socket.recv_buffer_size()?;
    socket.bind(&"127.0.0.1:0".parse::<std::net::SocketAddr>()?.into())?;
    socket.set_nonblocking(true)?;
    Ok((
        tokio::net::UdpSocket::from_std(socket.into())?,
        actual_buffer,
    ))
}

async fn srtp_stage(
    fixtures: &[Fixture],
    profile: ProtectionProfile,
    udp: bool,
    rounds: usize,
    iterations: usize,
) -> Result<Value> {
    let udp_before = udp_counters();
    let key = vec![0x42; profile.key_len()];
    let salt = vec![0x24; profile.salt_len()];
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
    let (receiver_socket, receive_buffer) = buffered_udp_receiver()?;
    let recv = Arc::new(receiver_socket);
    socket.connect(recv.local_addr()?).await?;
    let received = Arc::new(AtomicU64::new(0));
    let count = received.clone();
    let receive_socket = recv.clone();
    let _task = ReceiverTask(tokio::spawn(async move {
        let mut buf = [0u8; 2000];
        while receive_socket.recv(&mut buf).await.is_ok() {
            count.fetch_add(1, Ordering::Relaxed);
        }
    }));
    let mut sender = Context::new(&key, &salt, profile, None, None)?;
    let mut verifier = Context::new(&key, &salt, profile, None, None)?;
    let mut sequence = 0;
    let mut expected = 0;
    let mut timestamp = 0u32;
    let mut cases = Vec::new();
    let mut transcript = Sha256::new();
    let mut failure = None;
    'cases: for round in 0..rounds {
        for f in fixtures {
            let mut raw = Vec::new();
            for iteration in 0..iterations + 10 {
                timestamp = timestamp.wrapping_add(3000);
                let packets = packetize(f, &mut sequence, timestamp)?;
                let plaintext: Vec<_> = packets
                    .iter()
                    .map(|p| p.marshal())
                    .collect::<std::result::Result<_, _>>()?;
                let started = Instant::now();
                let mut encrypted = Vec::with_capacity(packets.len());
                for p in &plaintext {
                    let protected = sender.encrypt_rtp(p)?;
                    if udp {
                        ensure!(
                            socket.send(&protected).await? == protected.len(),
                            "short UDP send"
                        );
                    }
                    black_box(&protected);
                    encrypted.push(protected);
                }
                let elapsed = started.elapsed().as_secs_f64() * 1e6;
                // Outside timing: independently decrypt every packet, including sequence rollover.
                for (plain, cipher) in plaintext.iter().zip(encrypted.iter()) {
                    transcript.update((cipher.len() as u64).to_le_bytes());
                    transcript.update(cipher);
                    ensure!(
                        verifier.decrypt_rtp(cipher)?.as_ref() == plain.as_ref(),
                        "decryption mismatch"
                    );
                }
                if iteration >= 10 {
                    raw.push(elapsed);
                }
                expected += packets.len() as u64;
                if udp {
                    let drained = tokio::time::timeout(Duration::from_secs(1), async {
                        while received.load(Ordering::Relaxed) < expected {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await;
                    if drained.is_err() {
                        failure = Some(json!({
                            "error": "raw UDP capture loss",
                            "round": round,
                            "fixture_index": f.index,
                            "iteration_including_warmup": iteration,
                            "warmup": iteration < 10,
                            "sender_elapsed_us": elapsed,
                            "expected_packets": expected,
                            "udp_received": received.load(Ordering::Relaxed)
                        }));
                        break;
                    }
                }
            }
            // A timeout during warmup has no measured samples. Keep that case
            // without asking distribution() to index an empty sample set.
            let mut result = if raw.is_empty() {
                json!({"n": 0, "raw_us": raw})
            } else {
                distribution(&raw)
            };
            result["round"] = json!(round);
            result["fixture_index"] = json!(f.index);
            result["packets_per_frame"] = json!(f.packets);
            result["complete"] = json!(failure.is_none());
            cases.push(result);
            if failure.is_some() {
                break 'cases;
            }
        }
    }
    let udp_after = udp_counters();
    Ok(json!({
        "stage": if udp { "srtp_udp" } else { "srtp_context" },
        "profile": profile_name(profile),
        "codec": fixtures[0].codec.mime_type(),
        "cases": cases,
        "failed": failure.is_some(),
        "failure": failure,
        "expected_packets": expected,
        "packets_decrypt_verified": expected,
        "encrypted_transcript_sha256": hex(&transcript.finalize()),
        "transcript_format": "u64 little-endian packet length followed by SRTP bytes; includes warmup, fixed keys/salt/SSRC/initial sequence/timestamps",
        "udp_received": received.load(Ordering::Relaxed),
        "receive_buffer_bytes": receive_buffer,
        "udp_namespace_before": udp_before,
        "udp_namespace_after": udp_after,
        "udp_namespace_delta": udp_delta(&udp_before, &udp_after)
    }))
}

fn metadata() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let command = |args: &[&str]| {
        let mut command = std::process::Command::new(args[0]);
        command.args(&args[1..]);
        if root.exists() {
            command.current_dir(&root);
        }
        command
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let mut files = serde_json::Map::new();
    // Embed the source snapshot so a copied executable reports the same build
    // even if the checkout changes, or is absent on a CPU-only replay host.
    for (name, bytes) in [
        ("Cargo.lock", &include_bytes!("../../../Cargo.lock")[..]),
        (
            ".cargo/config.toml",
            &include_bytes!("../../../.cargo/config.toml")[..],
        ),
        (
            "src/webrtc/video_track.rs",
            &include_bytes!("../../../src/webrtc/video_track.rs")[..],
        ),
        (
            "src/webrtc/h265_payloader.rs",
            &include_bytes!("../../../src/webrtc/h265_payloader.rs")[..],
        ),
        (
            "src/video/codec/h264_bitstream.rs",
            &include_bytes!("../../../src/video/codec/h264_bitstream.rs")[..],
        ),
        (
            "libs/dtls/src/flight/flight0.rs",
            &include_bytes!("../../../libs/dtls/src/flight/flight0.rs")[..],
        ),
        (
            "libs/srtp/Cargo.toml",
            &include_bytes!("../../../libs/srtp/Cargo.toml")[..],
        ),
        (
            "libs/srtp/src/cipher/cipher_aes_cm_hmac_sha1/mod.rs",
            &include_bytes!("../../../libs/srtp/src/cipher/cipher_aes_cm_hmac_sha1/mod.rs")[..],
        ),
        (
            "test/rtp-bench/Cargo.lock",
            &include_bytes!("../Cargo.lock")[..],
        ),
        (
            "test/rtp-bench/Cargo.toml",
            &include_bytes!("../Cargo.toml")[..],
        ),
        ("test/rtp-bench/src/main.rs", &include_bytes!("main.rs")[..]),
    ] {
        files.insert(name.into(), json!(digest(bytes)));
    }
    json!({"checkout_revision_at_runtime":command(&["git","rev-parse","HEAD"]),"checkout_status_at_runtime":command(&["git","status","--porcelain"]),"uname":command(&["uname","-a"]),"rustc":command(&["rustc","-Vv"]),"arch":std::env::consts::ARCH,"target_features":crypto_features(),"openssl_hmac_feature":cfg!(feature="openssl-hmac"),"source_sha256":files,"build_manifest_sha256":digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/Cargo.toml"))),"build_lock_sha256":digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/Cargo.lock"))),"executable_sha256":std::fs::read("/proc/self/exe").map(|b|digest(&b)).ok(),"build":"release opt3 lto codegen-units1; webrtc/vendored-openssl; see cargo-features.txt for actual crypto backend"})
}

fn crypto_features() -> Value {
    #[cfg(target_arch = "aarch64")]
    {
        json!({"aes":std::arch::is_aarch64_feature_detected!("aes"),"pmull":std::arch::is_aarch64_feature_detected!("pmull"),"sha2":std::arch::is_aarch64_feature_detected!("sha2")})
    }
    #[cfg(target_arch = "x86_64")]
    {
        json!({"aes":std::arch::is_x86_feature_detected!("aes"),"pclmulqdq":std::arch::is_x86_feature_detected!("pclmulqdq"),"sha":std::arch::is_x86_feature_detected!("sha")})
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        json!({})
    }
}

async fn chrome(args: &[String]) -> Result<()> {
    ensure!(args.len()==8,"usage: --chrome OFFER_JSON NEW_ANSWER_JSON h264|h265 FRAMES_BIN SECONDS auto|server default|gcm|fallback NEW_REPORT_JSON");
    let codec = match args[2].as_str() {
        "h264" => VideoCodec::H264,
        "h265" => VideoCodec::H265,
        _ => bail!("invalid codec"),
    };
    let seconds: u64 = args[4].parse()?;
    let server_role = match args[5].as_str() {
        "auto" => false,
        "server" => true,
        _ => bail!("invalid DTLS role"),
    };
    let profiles = match args[6].as_str() {
        "default" => vec![],
        "gcm" => vec![ProtectionProfile::AeadAes128Gcm],
        "fallback" => vec![
            ProtectionProfile::AeadAes128Gcm,
            ProtectionProfile::Aes128CmHmacSha1_80,
        ],
        _ => bail!("invalid profile list"),
    };
    ensure!(
        !Path::new(&args[1]).exists() && !Path::new(&args[7]).exists(),
        "answer or report already exists"
    );
    let mut input = std::fs::File::open(&args[3])?;
    let mut clip = Vec::new();
    // Start with the saved SPS/PPS(+VPS)/IDR. Loop the full reference chain,
    // rather than repeating a single P frame that a real decoder cannot use.
    for _ in 0..600 {
        let mut header = [0u8; 16];
        if input.read(&mut header)? == 0 {
            break;
        }
        let length = u64::from_le_bytes(header[8..].try_into()?) as usize;
        ensure!(
            (1..=16 * 1024 * 1024).contains(&length),
            "invalid AU length"
        );
        let mut data = vec![0u8; length];
        input.read_exact(&mut data)?;
        clip.push(Bytes::from(data));
    }
    ensure!(!clip.is_empty(), "empty clip");
    let peer = make_peer_options(codec, profiles, None, true, server_role, false, None).await?;
    let track = Arc::new(UniversalVideoTrack::new(UniversalVideoTrackConfig {
        codec,
        fps: 60,
        ..Default::default()
    }));
    let sender = peer.add_track(track.as_track_local()).await?;
    tokio::spawn(async move {
        let mut buffer = [0u8; 1500];
        while sender.read(&mut buffer).await.is_ok() {}
    });
    let offer = serde_json::from_reader(std::fs::File::open(&args[0])?)?;
    peer.set_remote_description(offer).await?;
    let mut gathered = peer.gathering_complete_promise().await;
    peer.set_local_description(peer.create_answer(None).await?)
        .await?;
    gathered.recv().await;
    serde_json::to_writer(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[1])?,
        &peer.local_description().await,
    )?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while peer.connection_state() != RTCPeerConnectionState::Connected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Chrome ICE/DTLS timeout")?;
    let started = Instant::now();
    let mut frames = 0usize;
    let mut total_bytes = 0usize;
    let mut sends = Vec::new();
    let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while started.elapsed() < Duration::from_secs(seconds) {
        interval.tick().await;
        let data = clip[frames % clip.len()].clone();
        total_bytes += data.len();
        let send = Instant::now();
        track
            .write_frame_bytes_at(data, false, Some(frames as i64 * 1000 / 60))
            .await?;
        sends.push(send.elapsed().as_secs_f64() * 1e6);
        frames += 1;
    }
    let stats = serde_json::to_value(peer.get_stats().await)?;
    peer.close().await?;
    let report = json!({"environment":metadata(),"codec":codec.mime_type(),"dtls_role":args[5],"profile_setting":args[6],"frames":frames,"bytes":total_bytes,"send":distribution(&sends),"stats":stats,"clip_aus":clip.len()});
    serde_json::to_writer_pretty(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args[7])?,
        &report,
    )?;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--chrome") {
        return chrome(&args[2..]).await;
    }
    if !(6..=7).contains(&args.len()) {
        bail!("usage: onekvm-rtp-bench H264.frames.bin H265.frames.bin ROUNDS ITERATIONS NEW_OUTPUT.json [packetize,track_mock,interceptors_mock,srtp_context,srtp_udp,peer_udp,peer_udp_buffered]");
    }
    let rounds: usize = args[3].parse()?;
    let iterations: usize = args[4].parse()?;
    ensure!(
        rounds >= 1 && iterations >= 5,
        "need positive rounds and >=5 iterations"
    );
    let output = PathBuf::from(&args[5]);
    ensure!(!output.exists(), "output already exists");
    let stages: HashSet<&str> = args
        .get(6)
        .map_or(
            "packetize,track_mock,interceptors_mock,srtp_context,srtp_udp,peer_udp,peer_udp_buffered",
            String::as_str,
        )
        .split(',')
        .collect();
    ensure!(
        stages.iter().all(|s| [
            "packetize",
            "track_mock",
            "interceptors_mock",
            "srtp_context",
            "srtp_udp",
            "peer_udp",
            "peer_udp_buffered"
        ]
        .contains(s)),
        "unknown benchmark stage"
    );
    let (h264, a) = load_fixtures(Path::new(&args[1]), VideoCodec::H264)?;
    let (h265, b) = load_fixtures(Path::new(&args[2]), VideoCodec::H265)?;
    let mut fixture_metadata = Vec::new();
    for fixtures in [&h264, &h265] {
        for f in fixtures {
            fixture_metadata.push(json!({"codec":f.codec.mime_type(),"index":f.index,"pts":f.pts,"bytes":f.data.len(),"packets":f.packets,"sha256":f.hash}));
        }
    }
    let mut report = json!({"schema":1,"complete":false,"valid":false,"environment":metadata(),"inputs":[a,b],"fixtures":fixture_metadata,"rounds":rounds,"iterations_per_case":iterations,"timing":"per-frame sender wall duration; fixture reads, packet decryption, UDP drain and pacing outside timed interval; mock includes packet-count validation inside sink; peer UDP uses >=16.67ms between frame sends","results":[]});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    checkpoint(&mut file, &report)?;
    let mut failures = 0;
    for fixtures in [&h264, &h265] {
        if stages.contains("packetize") {
            for round in 0..rounds {
                for f in fixtures {
                    let mut raw = Vec::new();
                    for i in 0..iterations + 10 {
                        let start = Instant::now();
                        black_box(payloads(f.codec, &f.data)?);
                        if i >= 10 {
                            raw.push(start.elapsed().as_secs_f64() * 1e6);
                        }
                    }
                    report["results"].as_array_mut().unwrap().push(json!({"stage":"packetize","round":round,"codec":f.codec.mime_type(),"fixture_index":f.index,"packets_per_frame":f.packets,"timing":distribution(&raw)}));
                }
            }
            checkpoint(&mut file, &report)?;
        }
        for stage in ["track_mock", "interceptors_mock"] {
            if stages.contains(stage) {
                let result = full_stack(
                    fixtures,
                    ProtectionProfile::Aes128CmHmacSha1_80,
                    stage,
                    rounds,
                    iterations,
                )
                .await;
                failures += record(
                    &mut report,
                    stage,
                    fixtures[0].codec,
                    ProtectionProfile::Aes128CmHmacSha1_80,
                    result,
                );
                checkpoint(&mut file, &report)?;
            }
        }
        for p in [
            ProtectionProfile::Aes128CmHmacSha1_80,
            ProtectionProfile::AeadAes128Gcm,
        ] {
            for (stage, udp) in [("srtp_context", false), ("srtp_udp", true)] {
                if stages.contains(stage) {
                    let result = srtp_stage(fixtures, p, udp, rounds, iterations).await;
                    failures += record(&mut report, stage, fixtures[0].codec, p, result);
                    checkpoint(&mut file, &report)?;
                }
            }
            for stage in ["peer_udp", "peer_udp_buffered"] {
                if stages.contains(stage) {
                    let result = full_stack(fixtures, p, stage, rounds, iterations).await;
                    failures += record(&mut report, stage, fixtures[0].codec, p, result);
                    checkpoint(&mut file, &report)?;
                }
            }
        }
    }
    report["complete"] = json!(true);
    report["valid"] = json!(failures == 0);
    report["failed_stages"] = json!(failures);
    checkpoint(&mut file, &report)?;
    ensure!(
        failures == 0,
        "{failures} stages failed validation; partial results preserved"
    );
    println!(
        "completed {} stages",
        report["results"].as_array().unwrap().len()
    );
    Ok(())
}

fn checkpoint(file: &mut std::fs::File, report: &Value) -> Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer_pretty(&mut *file, report)?;
    file.sync_data()?;
    Ok(())
}

fn record(
    report: &mut Value,
    stage: &str,
    codec: VideoCodec,
    profile: ProtectionProfile,
    result: Result<Value>,
) -> usize {
    let (entry, failed) = match result {
        Ok(value) => {
            let failed = usize::from(value["failed"].as_bool().unwrap_or(false));
            if failed != 0 {
                eprintln!("stage {stage} failed packet delivery validation; raw samples and counters retained");
            }
            (value, failed)
        }
        Err(error) => {
            eprintln!("stage {stage} failed: {error:#}");
            (
                json!({"stage":stage,"codec":codec.mime_type(),"profile":profile_name(profile),"failed":true,"error":format!("{error:#}")}),
                1,
            )
        }
    };
    report["results"].as_array_mut().unwrap().push(entry);
    failed
}
