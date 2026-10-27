//! One UU viewer desktop connection: WebRTC answer, shared H.264 video with the UU RTP
//! metadata, Opus audio and CONTROL-channel input/settings mapped to One-KVM HID.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use bytes::Bytes;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::dtls_transport::dtls_role::DTLSRole;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState as PcState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpHeaderExtensionCapability, RTPCodecType,
};
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use super::ice::SessionIce;
use super::input::{self, HidAction, SessionInput, SessionInputEvent};
use super::signaling::{SignalingClient, SoacMessage};
use super::status::UuVideoSender;
use super::video_metrics::{FrameAgeStats, VideoDiagnostics};
use super::wire::{self, Rect};
use crate::audio::AudioController;
use crate::hid::{CanonicalKey, HidController};
use crate::video::codec::registry::VideoEncoderType;
use crate::video::codec::{BitratePreset, VideoCodecType};
use crate::video::VideoStreamManager;

/// Capture-source metadata read by the official streaming core (big-endian u16 source index,
/// and a one-byte fresh-frame flag). This host has the single source 0.
const CAPTURE_INDEX_URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/video-capture-index";
const NEW_FRAME_URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/video-frame-is-new-frame";
const FORWARD_SETTING_EVENT: &str = "forward_setting";
const ICE_FINISHED_EVENT: &str = "ice_finished_log";
const SCREEN_NAME: &str = "HDMI";
const RTP_CLOCK: u64 = 90_000;
const TRANSPORT_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Bitrates accepted by the shared encoder's custom setting.
const CUSTOM_KBPS: std::ops::RangeInclusive<u32> = 1_000..=60_000;

#[cfg(test)]
#[path = "desktop_tests.rs"]
mod desktop_tests;

struct CaptureIndex(u16);
impl webrtc::util::MarshalSize for CaptureIndex {
    fn marshal_size(&self) -> usize {
        2
    }
}
impl webrtc::util::Marshal for CaptureIndex {
    fn marshal_to(&self, buffer: &mut [u8]) -> webrtc::util::Result<usize> {
        let target = buffer
            .get_mut(..2)
            .ok_or_else(|| webrtc::util::Error::Other("capture index buffer too short".into()))?;
        target.copy_from_slice(&self.0.to_be_bytes());
        Ok(2)
    }
}

struct NewFrame;
impl webrtc::util::MarshalSize for NewFrame {
    fn marshal_size(&self) -> usize {
        1
    }
}
impl webrtc::util::Marshal for NewFrame {
    fn marshal_to(&self, buffer: &mut [u8]) -> webrtc::util::Result<usize> {
        *buffer
            .first_mut()
            .ok_or_else(|| webrtc::util::Error::Other("new frame buffer too short".into()))? = 1;
        Ok(1)
    }
}

fn frame_extensions() -> [rtp::extension::HeaderExtension; 2] {
    use rtp::extension::HeaderExtension::Custom;
    [
        Custom {
            uri: CAPTURE_INDEX_URI.into(),
            extension: Box::new(CaptureIndex(0)),
        },
        Custom {
            uri: NEW_FRAME_URI.into(),
            extension: Box::new(NewFrame),
        },
    ]
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

enum VideoTrack {
    H264(Arc<TrackLocalStaticSample>),
    H265(Arc<TrackLocalStaticRTP>),
}

impl VideoTrack {
    fn new(codec: VideoCodecType, hevc_profile: u8) -> Self {
        let capability = RTCRtpCodecCapability {
            mime_type: if codec == VideoCodecType::H265 {
                "video/H265"
            } else {
                "video/H264"
            }
            .into(),
            clock_rate: 90_000,
            sdp_fmtp_line: if codec == VideoCodecType::H265 {
                format!("profile-id={hevc_profile};tx-mode=SRST")
            } else {
                String::new()
            },
            ..Default::default()
        };
        if codec == VideoCodecType::H265 {
            Self::H265(Arc::new(TrackLocalStaticRTP::new(
                capability,
                "video_0".into(),
                "video_0".into(),
            )))
        } else {
            Self::H264(Arc::new(TrackLocalStaticSample::new(
                capability,
                "video_0".into(),
                "video_0".into(),
            )))
        }
    }

    fn local(&self) -> Arc<dyn TrackLocal + Send + Sync> {
        match self {
            Self::H264(track) => track.clone(),
            Self::H265(track) => track.clone(),
        }
    }

    async fn write(
        &self,
        sample: &Sample,
        hevc: &mut HevcPackets,
        color_space: Option<[u8; 4]>,
    ) -> webrtc::error::Result<()> {
        let extensions = frame_extensions();
        match self {
            Self::H264(track) => {
                track
                    .write_sample_with_extensions(sample, &extensions)
                    .await
            }
            Self::H265(track) => {
                for packet in hevc.packetize(sample) {
                    let mut extensions = frame_extensions().into_iter().collect::<Vec<_>>();
                    if let Some(extension) =
                        crate::webrtc::color_space::extension(color_space, packet.header.marker)
                    {
                        extensions.push(extension);
                    }
                    track
                        .write_rtp_with_extensions(&packet, &extensions)
                        .await?;
                }
                Ok(())
            }
        }
    }
}

struct HevcPackets {
    payloader: crate::webrtc::h265_payloader::H265Payloader,
    sequence: u16,
    timestamp: u32,
}

impl HevcPackets {
    fn new() -> Self {
        Self {
            payloader: Default::default(),
            sequence: rand::random(),
            timestamp: rand::random(),
        }
    }

    fn packetize(&mut self, sample: &Sample) -> Vec<rtp::packet::Packet> {
        // The stack's default HEVC payloader drops CRA (type 21) and mishandles mixed
        // Annex B prefixes. Use the board-tested payloader shared with the web viewer.
        let payloads = self.payloader.payload(1100, &sample.data);
        let count = payloads.len();
        let packets = payloads
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                let packet = rtp::packet::Packet {
                    header: rtp::header::Header {
                        version: 2,
                        marker: index + 1 == count,
                        sequence_number: self.sequence,
                        timestamp: self.timestamp,
                        ..Default::default()
                    },
                    payload,
                };
                self.sequence = self.sequence.wrapping_add(1);
                packet
            })
            .collect();
        self.timestamp = self
            .timestamp
            .wrapping_add((sample.duration.as_secs_f64() * 90_000.0) as u32);
        packets
    }
}

/// Round up (+1 ns float guard) so webrtc's floor(duration * 90000) yields exactly `ticks`.
fn ticks_to_duration(ticks: u64) -> Duration {
    Duration::from_nanos((ticks * 1_000_000_000).div_ceil(RTP_CLOCK) + 1)
}

/// RTP timestamps follow when frames actually arrive, not the nominal rate. webrtc-rs stamps a
/// sample and then advances by its duration, so each duration predicts the gap to the next
/// frame; a stall costs one frame of timing error instead of accumulating.
struct RtpTimeline {
    origin: Option<Instant>,
    stamp: u64,
    fps: u64,
}

impl RtpTimeline {
    fn new(fps: u32) -> Self {
        Self {
            origin: None,
            stamp: 0,
            fps: u64::from(fps.max(1)),
        }
    }

    fn duration_after(&mut self, at: Instant) -> Duration {
        let origin = *self.origin.get_or_insert(at);
        let real = ((at.saturating_duration_since(origin).as_nanos() * u128::from(RTP_CLOCK)
            + 500_000_000)
            / 1_000_000_000) as u64;
        let advance = (real + RTP_CLOCK / self.fps)
            .saturating_sub(self.stamp)
            .max(1);
        self.stamp += advance;
        ticks_to_duration(advance)
    }
}

/// Virtual/overlay/USB-gadget interfaces never carry the viewer path.
fn ice_interface_allowed(name: &str) -> bool {
    ![
        "zt",
        "docker",
        "br-",
        "veth",
        "virbr",
        "lo",
        "usb",
        "tailscale",
        "tun",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

/// The first active BUNDLE section (mid, m-line index) carries every local candidate.
fn negotiated_bundle_media(sdp: &str) -> Option<(String, u16)> {
    let preferred: Vec<&str> = sdp
        .lines()
        .find_map(|l| l.strip_prefix("a=group:BUNDLE "))?
        .split_whitespace()
        .collect();
    let mut sections: Vec<(bool, Option<String>)> = Vec::new();
    for line in sdp.lines() {
        if let Some(media) = line.strip_prefix("m=") {
            sections.push((media.split_whitespace().nth(1)? != "0", None));
        } else if let Some(mid) = line.strip_prefix("a=mid:") {
            if let Some(section) = sections.last_mut() {
                section.1 = Some(mid.to_string());
            }
        }
    }
    preferred.into_iter().find_map(|mid| {
        sections
            .iter()
            .position(|(active, m)| *active && m.as_deref() == Some(mid))
            .and_then(|index| Some((mid.to_string(), u16::try_from(index).ok()?)))
    })
}

fn offer_sdp(soac: &SoacMessage) -> Option<String> {
    if let Some(sdp) = soac.sdp.clone().filter(|s| !s.is_empty()) {
        return Some(sdp);
    }
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(soac.gzip_sdp.as_ref()?)
        .ok()?;
    let mut sdp = String::new();
    flate2::read::GzDecoder::new(&compressed[..])
        .read_to_string(&mut sdp)
        .ok()?;
    Some(sdp)
}

fn closed(state: PcState) -> bool {
    matches!(state, PcState::Closed | PcState::Failed)
}

fn preset_for(kbps: u32) -> BitratePreset {
    match kbps {
        1_000 => BitratePreset::Speed,
        4_000 => BitratePreset::Balanced,
        8_000 => BitratePreset::Quality,
        other => BitratePreset::Custom(other.clamp(*CUSTOM_KBPS.start(), *CUSTOM_KBPS.end())),
    }
}

#[derive(Default, Clone)]
struct Context {
    client_id: Option<String>,
    ice_id: Option<serde_json::Value>,
    app_control_id: Option<serde_json::Value>,
    bundle_media: Option<(String, u16)>,
}

#[derive(Clone)]
pub struct PeerDeps {
    pub system: crate::config::UuRemoteSystem,
    pub reverse_scroll: bool,
    pub video_codec: crate::config::UuVideoCodec,
    pub video: Arc<VideoStreamManager>,
    pub hid: Arc<HidController>,
    pub audio: Arc<AudioController>,
    pub signaling: Arc<SignalingClient>,
    pub terminal: Arc<super::terminal::Registry>,
    pub desktop: Arc<DesktopCoordinator>,
    #[cfg(target_os = "linux")]
    pub files: Arc<super::files::storage::Storage>,
}

/// Serialize desktop takeover across normal and promoted tool connections.
/// Tool transports survive losing the desktop role, including their terminals.
#[derive(Default)]
pub struct DesktopCoordinator(Mutex<Weak<Shared>>);

impl DesktopCoordinator {
    async fn claim(&self, next: &Arc<Shared>) -> Result<(), String> {
        let mut owner = self.0.lock().await;
        if next.closed.is_cancelled() {
            return Err("Connection closed".into());
        }
        if *next.desktop.borrow() {
            return Ok(());
        }
        if next.media.get().is_none() {
            return Err("Desktop requires a receiving video section in the tool offer".into());
        }
        if let Some(old) = owner.upgrade().filter(|old| !Arc::ptr_eq(old, next)) {
            old.stop_desktop().await;
            if !old.terminal {
                old.closed.cancel();
                old.update_input(SessionInputEvent::Close).await;
                if let Some(pc) = old.media.get().and_then(|m| m.pc.upgrade()) {
                    let _ = tokio::time::timeout(TRANSPORT_CLOSE_TIMEOUT, pc.close()).await;
                }
            }
        }
        next.deps
            .video
            .set_uu_negotiated_codec(Some(next.codec))
            .await;
        #[cfg(feature = "vim4")]
        if let Err(error) = next.save_hdr_default().await {
            next.deps.video.set_uu_negotiated_codec(None).await;
            return Err(error);
        }
        next.desktop.send_replace(true);
        next.update_input(SessionInputEvent::ControlOpened).await;
        if next
            .media
            .get()
            .and_then(|m| m.pc.upgrade())
            .is_some_and(|p| p.connection_state() == PcState::Connected)
        {
            next.update_input(SessionInputEvent::Connected).await;
        }
        *owner = Arc::downgrade(next);
        start_desktop_streams(next.clone());
        Ok(())
    }
}

struct DesktopMedia {
    pc: Weak<RTCPeerConnection>,
    video: Arc<VideoTrack>,
    audio: Arc<TrackLocalStaticSample>,
}

/// State shared between the media tasks and the CONTROL channel.
struct Shared {
    closed: CancellationToken,
    terminal: bool,
    capture_type: u64,
    desktop: watch::Sender<bool>,
    media: OnceLock<DesktopMedia>,
    desktop_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    terminal_conn: parking_lot::Mutex<Option<Arc<super::terminal::TerminalConn>>>,
    deps: PeerDeps,
    sequence: Arc<AtomicU64>,
    #[cfg(target_os = "linux")]
    files: Arc<super::files::Connection>,
    /// Encoded size and fps, refreshed by the video task.
    screen: watch::Sender<(Rect, u32)>,
    /// Last viewer quality choice reported back in CaptureSettingSync.
    quality: AtomicU64,
    custom_kbps: AtomicU64,
    actual_kbps: AtomicU64,
    default_bitrate: BitratePreset,
    settings_lock: Mutex<()>,
    #[cfg(feature = "vim4")]
    hdr_restore: Mutex<Option<bool>>,
    context: Arc<Mutex<Context>>,
    input: SessionInput,
    codec: VideoCodecType,
    hevc_profile: u8,
    video_diagnostics: Mutex<VideoDiagnostics>,
}

impl Shared {
    async fn stop_desktop(&self) {
        if !self.desktop.send_replace(false) {
            return;
        }
        let task = self.desktop_task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.update_input(SessionInputEvent::Disconnected).await;
        self.deps.video.set_uu_negotiated_codec(None).await;
        self.video_diagnostics.lock().await.sender = None;
        #[cfg(feature = "vim4")]
        self.restore_hdr().await;
    }

    fn hdr_control_available(&self) -> bool {
        #[cfg(feature = "vim4")]
        return self.codec == VideoCodecType::H265
            && self.hevc_profile == 2
            && crate::platform::vim4::hdr_remote_available();
        #[cfg(not(feature = "vim4"))]
        false
    }

    #[cfg(feature = "vim4")]
    async fn save_hdr_default(&self) -> Result<(), String> {
        if !crate::platform::vim4::hdr_remote_available() {
            return Ok(());
        }
        let output = self
            .deps
            .video
            .vim4_output_settings()
            .await
            .ok_or("Video output settings unavailable")?;
        *self.hdr_restore.lock().await = Some(output.hdr);
        if !self.hdr_control_available() && output.hdr {
            // A global HDR preference cannot make an eight-bit-only viewer
            // decode Main10. Keep this session SDR and restore the default later.
            self.deps.video.restore_vim4_hdr(false).await;
        }
        Ok(())
    }

    #[cfg(feature = "vim4")]
    async fn restore_hdr(&self) {
        // Serialize with a request already applying when CONTROL closes.
        let _guard = self.settings_lock.lock().await;
        let Some(hdr) = self.hdr_restore.lock().await.take() else {
            return;
        };
        let hdr = crate::platform::vim4::global_hdr_default()
            .map(|hdr| hdr && crate::platform::vim4::hdr_remote_available())
            .unwrap_or(hdr);
        self.deps.video.restore_vim4_hdr(hdr).await;
    }

    async fn report_hdr_capability(&self) {
        let context = self.context.lock().await.clone();
        let (Some(client), Some(ice)) = (
            context.client_id.as_deref(),
            context
                .ice_id
                .as_ref()
                .and_then(super::signaling::session_key),
        ) else {
            return;
        };
        let (size, fps) = *self.screen.borrow();
        let mut capability = device_capability(client, &ice, native_screen(size), fps, self.codec);
        if self.hdr_control_available() {
            #[cfg(feature = "vim4")]
            {
                let enabled = self
                    .deps
                    .video
                    .vim4_output_settings()
                    .await
                    .is_some_and(|output| output.hdr);
                capability["data"]["device_capability"]["display_info"][0]["hdr"] =
                    u8::from(enabled).into();
                capability["data"]["device_capability"]["video_codec_capability"][0]["bit_depth"] =
                    10.into();
            }
        }
        if let Err(error) = self
            .deps
            .signaling
            .emit_event(FORWARD_SETTING_EVENT, capability)
            .await
        {
            warn!("UU HDR capability notification failed: {error}");
        }
    }

    async fn refresh_screen(&self) {
        if let Some(config) = self.deps.video.get_encoding_config().await {
            let kbps = u64::from(config.bitrate_kbps());
            let bitrate_changed = self.actual_kbps.swap(kbps, Ordering::Relaxed) != kbps;
            if bitrate_changed {
                self.quality.store(wire::quality::CUSTOM, Ordering::Relaxed);
                self.custom_kbps.store(kbps, Ordering::Relaxed);
            }
            let next = (
                Rect {
                    width: config.resolution.width as i32,
                    height: config.resolution.height as i32,
                },
                config.fps,
            );
            self.screen.send_if_modified(|current| {
                std::mem::replace(current, next) != next || bitrate_changed
            });
        }
    }

    fn capture_sync(&self) -> Vec<u8> {
        let (size, fps) = *self.screen.borrow();
        wire::capture_sync_with_bitrate(
            fps,
            self.quality.load(Ordering::Relaxed),
            size,
            self.custom_kbps.load(Ordering::Relaxed) as u32,
        )
    }

    fn next_seq(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    async fn send_hid(&self, actions: Vec<HidAction>, source: crate::hid::control::InputSource) {
        for action in actions {
            let result = match action {
                HidAction::Keyboard(event) => self.deps.hid.send_keyboard_from(source, event).await,
                HidAction::Mouse(event) => self.deps.hid.send_mouse_from(source, event).await,
            };
            if let Err(e) = result {
                debug!("UU input not delivered: {e}");
            }
        }
        if source == crate::hid::control::InputSource::System {
            if let Err(e) = self
                .deps
                .hid
                .end_manual_input(crate::hid::control::ManualKind::Uu)
                .await
            {
                debug!("UU input cleanup not delivered: {e}");
            }
        }
    }

    async fn update_input(&self, event: SessionInputEvent<'_>) {
        if !*self.desktop.borrow()
            && !matches!(
                event,
                SessionInputEvent::Disconnected | SessionInputEvent::Close
            )
        {
            return;
        }
        let source = if matches!(
            &event,
            SessionInputEvent::Disconnected | SessionInputEvent::Close
        ) {
            crate::hid::control::InputSource::System
        } else {
            crate::hid::control::InputSource::Manual(crate::hid::control::ManualKind::Uu)
        };
        self.input
            .update(event, |actions| self.send_hid(actions, source))
            .await;
    }
}

fn start_desktop_streams(shared: Arc<Shared>) {
    let Some(media) = shared.media.get() else {
        return;
    };
    let Some(pc) = media.pc.upgrade() else { return };
    let (video, audio) = (media.video.clone(), media.audio.clone());
    let mut slot = shared.desktop_task.lock();
    if slot.as_ref().is_some_and(|task| !task.is_finished()) {
        return;
    }
    let running = shared.clone();
    *slot = Some(tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = running.closed.cancelled() => {},
            _ = async {
                tokio::join!(
                    stream_video(pc.clone(), video, running.clone(), running.codec),
                    stream_audio(pc, audio, running.deps.audio.clone())
                );
            } => {},
        }
    }));
}

pub struct Peer {
    pub created_at: Instant,
    pub pc: Arc<RTCPeerConnection>,
    context: Arc<Mutex<Context>>,
    video: Arc<VideoStreamManager>,
    codec: VideoCodecType,
    shared: Arc<Shared>,
    _shutdown_guard: DropGuard,
}

fn spawn_until_closed(
    closed: CancellationToken,
    task: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = closed.cancelled() => {},
            _ = task => {},
        }
    });
}

fn offered_hevc_profile(
    sdp: &str,
    preference: crate::config::UuVideoCodec,
    allowed: &[VideoCodecType],
    main10_available: bool,
) -> u8 {
    if main10_available
        && select_video_codec_with_profile(sdp, preference, allowed, 2) == Ok(VideoCodecType::H265)
    {
        2
    } else {
        1
    }
}

#[cfg(test)]
fn select_video_codec(
    sdp: &str,
    preference: crate::config::UuVideoCodec,
    allowed: &[VideoCodecType],
) -> Result<VideoCodecType, String> {
    select_video_codec_with_profile(sdp, preference, allowed, 1)
}

fn select_video_codec_with_profile(
    sdp: &str,
    preference: crate::config::UuVideoCodec,
    allowed: &[VideoCodecType],
    hevc_profile: u8,
) -> Result<VideoCodecType, String> {
    use crate::config::UuVideoCodec;
    let session = webrtc::sdp::description::session::SessionDescription::unmarshal(
        &mut std::io::Cursor::new(sdp),
    )
    .map_err(|_| "Invalid viewer SDP")?;
    let media = session
        .media_descriptions
        .iter()
        .find(|m| {
            m.media_name.media == "video"
                && m.media_name.port.value != 0
                && !m
                    .attributes
                    .iter()
                    .any(|a| matches!(a.key.as_str(), "inactive" | "sendonly"))
        })
        .ok_or("Viewer has no receiving video section")?;
    let supports = |codec: VideoCodecType| {
        let name = if codec == VideoCodecType::H265 {
            "H265/90000"
        } else {
            "H264/90000"
        };
        media
            .attributes
            .iter()
            .filter(|a| a.key == "rtpmap")
            .any(|a| {
                let Some((pt, format)) = a.value.as_deref().and_then(|v| v.split_once(' ')) else {
                    return false;
                };
                if !format.eq_ignore_ascii_case(name)
                    || !media.media_name.formats.iter().any(|f| f == pt)
                {
                    return false;
                }
                if codec == VideoCodecType::H264 {
                    return true;
                }
                // Main and Main10 must match the selected encoder, including
                // when the receiver also offers the other profile.
                let mut profile_present = hevc_profile == 1;
                let compatible = media
                    .attributes
                    .iter()
                    .filter(|a| a.key == "fmtp")
                    .all(|a| {
                        let Some((payload, params)) =
                            a.value.as_deref().and_then(|v| v.split_once(' '))
                        else {
                            return true;
                        };
                        payload != pt
                            || params.split(';').all(|param| {
                                let Some((key, value)) = param.trim().split_once('=') else {
                                    return true;
                                };
                                match key {
                                    "profile-id" => {
                                        profile_present = true;
                                        value.parse::<u8>().ok() == Some(hevc_profile)
                                    }
                                    "tx-mode" => value.eq_ignore_ascii_case("SRST"),
                                    _ => true,
                                }
                            })
                    });
                compatible && profile_present
            })
    };
    let candidates: &[VideoCodecType] = match preference {
        UuVideoCodec::Auto => &[VideoCodecType::H265, VideoCodecType::H264],
        UuVideoCodec::H264 => &[VideoCodecType::H264],
        UuVideoCodec::H265 => &[VideoCodecType::H265],
    };
    candidates
        .iter()
        .copied()
        .find(|c| allowed.contains(c) && supports(*c))
        .ok_or_else(|| "No compatible video codec; select Auto or H.264 and reconnect".into())
}

impl Peer {
    pub async fn new(
        deps: PeerDeps,
        transport: SessionIce,
        offer: &SoacMessage,
        capture_type: u64,
    ) -> Result<Self, String> {
        let terminal = capture_type > 2;
        let sdp = offer_sdp(offer).ok_or("Offer SDP is empty")?;
        let constraints = deps.video.codec_constraints().await;
        #[cfg(not(feature = "vim4"))]
        let main10_available = false;
        #[cfg(feature = "vim4")]
        let main10_available = crate::platform::vim4::hdr_remote_available();
        let hevc_profile = offered_hevc_profile(
            &sdp,
            deps.video_codec,
            &constraints.allowed_webrtc_codecs,
            main10_available,
        );
        let offered_codec = select_video_codec_with_profile(
            &sdp,
            deps.video_codec,
            &constraints.allowed_webrtc_codecs,
            hevc_profile,
        );
        let media_available = offered_codec.is_ok();
        let codec = match offered_codec {
            Ok(codec) => codec,
            Err(_) if terminal => VideoCodecType::H264,
            Err(error) => return Err(error),
        };
        if !terminal {
            info!(?codec, hevc_profile, preference = ?deps.video_codec, "UU negotiated video codec");
        }
        let mut media = MediaEngine::default();
        media
            .register_default_codecs()
            .map_err(|e| format!("Register codecs failed: {e}"))?;
        if hevc_profile == 2 {
            media
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: RTCRtpCodecCapability {
                            mime_type: "video/H265".into(),
                            clock_rate: 90_000,
                            sdp_fmtp_line: "profile-id=2;tier-flag=0;level-id=180;tx-mode=SRST"
                                .into(),
                            rtcp_feedback: vec![
                                webrtc::rtp_transceiver::RTCPFeedback {
                                    typ: "nack".into(),
                                    parameter: "".into(),
                                },
                                webrtc::rtp_transceiver::RTCPFeedback {
                                    typ: "nack".into(),
                                    parameter: "pli".into(),
                                },
                                webrtc::rtp_transceiver::RTCPFeedback {
                                    typ: "ccm".into(),
                                    parameter: "fir".into(),
                                },
                            ],
                            ..Default::default()
                        },
                        payload_type: 123,
                        ..Default::default()
                    },
                    RTPCodecType::Video,
                )
                .map_err(|e| format!("Register Main10 failed: {e}"))?;
        }
        for uri in [
            CAPTURE_INDEX_URI,
            NEW_FRAME_URI,
            crate::webrtc::color_space::URI,
        ] {
            media
                .register_header_extension(
                    RTCRtpHeaderExtensionCapability { uri: uri.into() },
                    RTPCodecType::Video,
                    None,
                )
                .map_err(|e| format!("Register UU RTP metadata failed: {e}"))?;
        }
        let registry = register_default_interceptors(Registry::new(), &mut media)
            .map_err(|e| format!("Register interceptors failed: {e}"))?;
        let mut settings = SettingEngine::default();
        settings
            .set_answering_dtls_role(DTLSRole::Server)
            .map_err(|e| format!("Configure DTLS role failed: {e}"))?;
        settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
        settings.set_interface_filter(Box::new(ice_interface_allowed));
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_setting_engine(settings)
            .with_interceptor_registry(registry)
            .build();
        let pc = Arc::new(
            api.new_peer_connection(transport.configuration()?)
                .await
                .map_err(|e| format!("Create peer failed: {e}"))?,
        );
        let context = Arc::new(Mutex::new(Context::default()));

        let initial = deps
            .video
            .get_encoding_config()
            .await
            .map(|c| {
                (
                    Rect {
                        width: c.resolution.width as i32,
                        height: c.resolution.height as i32,
                    },
                    c.fps,
                )
            })
            .unwrap_or((
                Rect {
                    width: 1920,
                    height: 1080,
                },
                60,
            ));
        let default_bitrate = deps
            .video
            .get_encoding_config()
            .await
            .map(|c| c.bitrate_preset)
            .unwrap_or(BitratePreset::Balanced);
        let closed = CancellationToken::new();
        let sequence = Arc::new(AtomicU64::new(1));
        let shared = Arc::new(Shared {
            #[cfg(target_os = "linux")]
            files: super::files::Connection::new(
                deps.files.clone(),
                sequence.clone(),
                closed.child_token(),
            ),
            closed,
            terminal,
            capture_type,
            desktop: watch::channel(false).0,
            media: OnceLock::new(),
            desktop_task: parking_lot::Mutex::new(None),
            terminal_conn: parking_lot::Mutex::new(None),
            default_bitrate,
            custom_kbps: AtomicU64::new(u64::from(default_bitrate.bitrate_kbps())),
            actual_kbps: AtomicU64::new(u64::from(default_bitrate.bitrate_kbps())),
            settings_lock: Mutex::new(()),
            #[cfg(feature = "vim4")]
            hdr_restore: Mutex::new(None),
            context: context.clone(),
            deps: deps.clone(),
            sequence,
            screen: watch::channel(initial).0,
            quality: AtomicU64::new(wire::quality::AUTO),
            input: SessionInput::new(deps.system, deps.reverse_scroll),
            codec,
            video_diagnostics: Mutex::new(VideoDiagnostics::default()),
            hevc_profile,
        });
        // Also cancel application tasks if constructing or owning the peer ends early.
        let shutdown_guard = shared.closed.clone().drop_guard();

        let (ice_context, ice_signal) = (context.clone(), deps.signaling.clone());
        pc.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let (context, signal) = (ice_context.clone(), ice_signal.clone());
            Box::pin(async move {
                let Some(init) = candidate.and_then(|c| c.to_json().ok()) else { return };
                let context = context.lock().await.clone();
                let (Some(client_id), Some((mid, mline))) = (context.client_id, context.bundle_media) else {
                    debug!("UU local candidate before negotiation; skipped");
                    return;
                };
                let message = SoacMessage {
                    msg_type: "candidate".into(),
                    sdp: None,
                    gzip_sdp: None,
                    candidate: Some(serde_json::json!({ "candidate": init.candidate, "sdpMid": mid, "sdpMLineIndex": mline })),
                    ice_id: context.ice_id,
                    sdp_mid: None,
                    sdp_mline_index: None,
                    client_id: Some(client_id),
                    app_control_id: context.app_control_id,
                };
                if let Err(e) = signal.emit_soac(message).await {
                    warn!("UU candidate signaling failed: {e}");
                }
            })
        }));

        if media_available {
            let video_track = Arc::new(VideoTrack::new(codec, hevc_profile));
            let video_sender = pc
                .add_track(video_track.local())
                .await
                .map_err(|e| format!("Add video track failed: {e}"))?;
            spawn_until_closed(
                shared.closed.clone(),
                read_video_feedback(video_sender, shared.clone()),
            );
            // The viewer always offers an Opus m-line.
            let audio_track = Arc::new(TrackLocalStaticSample::new(
                RTCRtpCodecCapability {
                    mime_type: "audio/opus".to_owned(),
                    clock_rate: 48_000,
                    channels: 2,
                    ..Default::default()
                },
                "audio_0".to_owned(),
                "audio_0".to_owned(),
            ));
            let audio_sender = pc
                .add_track(audio_track.clone())
                .await
                .map_err(|e| format!("Add audio track failed: {e}"))?;
            spawn_until_closed(shared.closed.clone(), async move {
                let mut rtcp = [0u8; 1500];
                while audio_sender.read(&mut rtcp).await.is_ok() {}
            });

            let _ = shared.media.set(DesktopMedia {
                pc: Arc::downgrade(&pc),
                video: video_track.clone(),
                audio: audio_track.clone(),
            });
            let started = Arc::new(AtomicBool::new(false));
            let (state_shared, state_context) = (shared.clone(), context.clone());
            pc.on_peer_connection_state_change(Box::new(move |state| {
                let (shared, context, started) =
                    (state_shared.clone(), state_context.clone(), started.clone());
                Box::pin(async move {
                    if shared.closed.is_cancelled() {
                        return;
                    }
                    info!("UU peer connection state: {state:?}");
                    if let Some(conn) = shared.terminal_conn.lock().as_ref() {
                        terminal_connection_state(conn, state);
                    }
                    match state {
                        PcState::Connected => {
                            shared.update_input(SessionInputEvent::Connected).await
                        }
                        PcState::Disconnected => {
                            shared.update_input(SessionInputEvent::Disconnected).await
                        }
                        PcState::Closed | PcState::Failed => {
                            shared.closed.cancel();
                            shared.update_input(SessionInputEvent::Close).await
                        }
                        _ => {}
                    }
                    if state != PcState::Connected || started.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    if *shared.desktop.borrow() {
                        start_desktop_streams(shared.clone());
                    }
                    let context = context.lock().await.clone();
                    let signal = &shared.deps.signaling;
                    if let (Some(client), Some(ice)) = (
                        &context.client_id,
                        context
                            .ice_id
                            .as_ref()
                            .and_then(super::signaling::session_key),
                    ) {
                        shared.report_hdr_capability().await;
                        if let Err(e) = signal
                            .emit_event(FORWARD_SETTING_EVENT, sender_parameters(client, &ice))
                            .await
                        {
                            warn!("UU sender parameters notification failed: {e}");
                        }
                        if let Some(control) =
                            context.app_control_id.as_ref().and_then(|v| v.as_str())
                        {
                            let payload =
                                serde_json::json!({ "ice_id": ice, "app_control_id": control });
                            if let Err(e) = signal.emit_event(ICE_FINISHED_EVENT, payload).await {
                                warn!("UU connection completion notification failed: {e}");
                            }
                        }
                    }
                })
            }));
        } else {
            let closing = shared.clone();
            pc.on_peer_connection_state_change(Box::new(move |state| {
                let shared = closing.clone();
                Box::pin(async move {
                    if matches!(state, PcState::Closed | PcState::Failed) {
                        shared.closed.cancel();
                    }
                    if let Some(conn) = shared.terminal_conn.lock().as_ref() {
                        terminal_connection_state(conn, state);
                    }
                })
            }));
        }

        let channel_shared = shared.clone();
        pc.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
            let shared = channel_shared.clone();
            Box::pin(async move {
                if shared.closed.is_cancelled() {
                    return;
                }
                let label = channel.label().to_string();
                info!("UU data channel opened: {label}");
                match label.as_str() {
                    "CONTROL_DATA_CHANNEL" => attach_control(channel, shared),
                    "BINARY_DATA_CHANNEL" if shared.terminal => attach_terminal(channel, shared),
                    #[cfg(target_os = "linux")]
                    "FILE_DATA_CHANNEL" => attach_file(channel, shared.files.clone()),
                    "STREAMER_DATA_CHANNEL" => {
                        let reply = Arc::downgrade(&channel);
                        channel.on_message(Box::new(move |message: DataChannelMessage| {
                            let reply = reply.clone();
                            Box::pin(async move {
                                if message.data.len() > 256 * 1024 {
                                    return;
                                }
                                let Ok(value) =
                                    serde_json::from_slice::<serde_json::Value>(&message.data)
                                else {
                                    return;
                                };
                                if let Some(event) = value.get("connection_period_stats_event") {
                                    let metrics = super::metrics::collect(event);
                                    info!(?metrics, "UU viewer timing");
                                }
                                let ping =
                                    value.get("type").and_then(|t| t.as_str()) == Some("ping");
                                if ping {
                                    let Some(reply) = reply.upgrade() else { return };
                                    let _ = reply
                                        .send(&Bytes::from_static(br#"{"type":"pong"}"#))
                                        .await;
                                }
                            })
                        }));
                    }
                    _ => {}
                }
            })
        }));

        if !terminal {
            deps.desktop.claim(&shared).await?;
        }
        Ok(Self {
            created_at: Instant::now(),
            pc,
            context,
            video: deps.video.clone(),
            codec,
            shared,
            _shutdown_guard: shutdown_guard,
        })
    }

    /// Apply the viewer's offer and return the answer SDP.
    pub async fn handle_offer(&self, soac: &SoacMessage) -> Result<String, String> {
        {
            let mut context = self.context.lock().await;
            context.client_id = soac.client_id.clone();
            context.ice_id = soac.ice_id.clone();
            context.app_control_id = soac.app_control_id.clone();
        }
        let sdp = offer_sdp(soac).ok_or("Offer SDP is empty")?;
        let offer = RTCSessionDescription::offer(sdp).map_err(|e| format!("Invalid offer: {e}"))?;
        self.pc
            .set_remote_description(offer)
            .await
            .map_err(|e| format!("Set remote description failed: {e}"))?;
        if self.shared.media.get().is_some() {
            restrict_video_codec(&self.pc, self.codec).await?;
        }
        let answer = self
            .pc
            .create_answer(None)
            .await
            .map_err(|e| format!("Create answer failed: {e}"))?;
        let bundle =
            negotiated_bundle_media(&answer.sdp).ok_or("Answer has no active BUNDLE transport")?;
        self.context.lock().await.bundle_media = Some(bundle);
        self.pc
            .set_local_description(answer.clone())
            .await
            .map_err(|e| format!("Set local description failed: {e}"))?;
        Ok(answer.sdp)
    }

    pub async fn add_candidate(&self, soac: &SoacMessage) -> Result<(), String> {
        let Some(value) = &soac.candidate else {
            return Ok(());
        };
        let (candidate, mid, mline) = match value.as_str() {
            Some(s) => (
                s.to_string(),
                soac.sdp_mid.clone(),
                soac.sdp_mline_index.map(|i| i as u16),
            ),
            None => (
                value
                    .get("candidate")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                value
                    .get("sdpMid")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .or_else(|| soac.sdp_mid.clone()),
                value
                    .get("sdpMLineIndex")
                    .and_then(|v| v.as_u64())
                    .map(|i| i as u16)
                    .or(soac.sdp_mline_index.map(|i| i as u16)),
            ),
        };
        if candidate.is_empty() {
            return Ok(());
        }
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid: mid.or_else(|| Some("0".into())),
            sdp_mline_index: mline.or(Some(0)),
            ..Default::default()
        };
        self.pc
            .add_ice_candidate(init)
            .await
            .map_err(|e| format!("Add candidate failed: {e}"))
    }

    pub fn connected(&self) -> bool {
        self.pc.connection_state() == PcState::Connected
    }

    pub async fn status(&self, id: String) -> super::UuSessionStatus {
        let client_id = self.context.lock().await.client_id.clone();
        let state = self.pc.connection_state();
        // A disconnected transport may retain its old selected pair. Do not
        // present that pair as a currently working media path.
        let link = if state == PcState::Connected {
            self.pc
                .dtls_transport()
                .ice_transport()
                .get_selected_candidate_pair()
                .await
                .map(Into::into)
        } else {
            None
        };
        super::UuSessionStatus {
            id,
            client_id,
            state: state.to_string(),
            ice_state: self.pc.ice_connection_state().to_string(),
            video_codec: if self.codec == VideoCodecType::H265 {
                "h265"
            } else {
                "h264"
            }
            .into(),
            link,
            video_stats: if state == PcState::Connected {
                Some(
                    self.shared
                        .video_diagnostics
                        .lock()
                        .await
                        .snapshot(Instant::now()),
                )
            } else {
                None
            },
        }
    }

    pub fn ended(&self) -> bool {
        closed(self.pc.connection_state())
    }

    pub fn is_terminal(&self) -> bool {
        self.shared.capture_type == 8
    }

    pub fn is_tool(&self) -> bool {
        self.shared.terminal
    }

    pub fn is_desktop(&self) -> bool {
        *self.shared.desktop.borrow()
    }

    pub async fn stop_desktop(&self) {
        let _owner = self.shared.deps.desktop.0.lock().await;
        self.shared.stop_desktop().await;
    }

    pub async fn close(&self) {
        self.shared.closed.cancel();
        #[cfg(target_os = "linux")]
        self.shared.files.close().await;
        self.stop_desktop().await;
        if let Some(conn) = self.shared.terminal_conn.lock().take() {
            conn.close();
        }
        // Finish releases before the room loop can create a replacement peer.
        self.shared.update_input(SessionInputEvent::Close).await;
        // Only transport teardown is bounded. Input releases above must finish
        // before a replacement can acquire HID ownership or the shared codec.
        match tokio::time::timeout(TRANSPORT_CLOSE_TIMEOUT, self.pc.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!("UU peer close: {e}"),
            Err(_) => warn!("UU peer transport close timed out; application tasks cancelled"),
        }
    }
}

async fn restrict_video_codec(pc: &RTCPeerConnection, codec: VideoCodecType) -> Result<(), String> {
    if codec == VideoCodecType::H265 {
        // Apply after set_remote_description: webrtc-rs preserves nonzero
        // preference payload types instead of remapping them to the offer.
        // Reading parameters before negotiation would pin our default 126.
        for transceiver in pc.get_transceivers().await {
            if transceiver.kind() != RTPCodecType::Video {
                continue;
            }
            let codecs: Vec<_> = transceiver
                .sender()
                .await
                .get_parameters()
                .await
                .rtp_parameters
                .codecs
                .into_iter()
                .filter(|c| c.capability.mime_type.eq_ignore_ascii_case("video/H265"))
                .collect();
            if codecs.is_empty() {
                return Err("H.265 is not registered in the WebRTC engine".into());
            }
            for codec in &codecs {
                info!(
                    payload_type = codec.payload_type,
                    "UU negotiated H.265 RTP payload"
                );
            }
            transceiver
                .set_codec_preferences(codecs)
                .await
                .map_err(|e| format!("Select H.265 transceiver failed: {e}"))?;
        }
    }
    Ok(())
}

/// forward_setting device_capability: the displayable source and negotiated encoder.
/// codec_impl values below 5 name specific GPU encoders; 5 is the generic implementation.
fn device_capability(
    client_id: &str,
    ice_id: &str,
    native: Rect,
    fps: u32,
    codec: VideoCodecType,
) -> serde_json::Value {
    serde_json::json!({
        "client_id": client_id,
        "data": {
            "type": "device_capability",
            "device_capability": {
                "display_info": [{"id": 0, "fps": fps, "type": 0, "hdr": -1}],
                "video_codec_capability": [{
                    "video_codec": if codec == VideoCodecType::H265 { 2 } else { 1 },
                    "chroma_sampling": 1, "codec_impl": 5, "bit_depth": 8,
                    "width": native.width, "height": native.height
                }],
                "ice_id": ice_id
            }
        }
    })
}

fn sender_parameters(client_id: &str, ice_id: &str) -> serde_json::Value {
    let media = serde_json::json!({
        "capture_impl": "HDMI", "encoder_impl": "One-KVM", "video_track_index": 0
    });
    let mut parameters = media.clone();
    parameters["ice_id"] = ice_id.into();
    parameters["sender_media_infos"] = serde_json::json!([media]);
    serde_json::json!({
        "client_id": client_id,
        "data": {"type": "sender_para_info", "sender_para_info": parameters}
    })
}

/// Keyframe requests (PLI/FIR) from the viewer, rate-limited for the shared encoder.
async fn read_video_feedback(sender: Arc<RTCRtpSender>, shared: Arc<Shared>) {
    use webrtc::rtcp::payload_feedbacks::{
        full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
    };
    let mut buffer = [0u8; 1500];
    let video = &shared.deps.video;
    let mut last_request = Instant::now() - Duration::from_secs(1);
    while let Ok((packets, _)) = sender.read(&mut buffer).await {
        let received_at = Instant::now();
        let wall_time = SystemTime::now();
        let mut diagnostics = shared.video_diagnostics.lock().await;
        let feedback = &mut diagnostics.feedback;
        for packet in &packets {
            feedback.observe(packet.as_any(), wall_time, received_at);
        }
        feedback.log_if_due(received_at);
        drop(diagnostics);
        let wants_keyframe = packets.iter().any(|p| {
            p.as_any().is::<PictureLossIndication>() || p.as_any().is::<FullIntraRequest>()
        });
        if wants_keyframe
            && *shared.desktop.borrow()
            && last_request.elapsed() >= Duration::from_millis(500)
        {
            last_request = Instant::now();
            if let Err(e) = video.request_keyframe().await {
                debug!("UU keyframe request failed: {e}");
            }
        }
    }
}

async fn wait_connected(pc: &RTCPeerConnection) -> bool {
    loop {
        match pc.connection_state() {
            PcState::Connected => return true,
            state if closed(state) => return false,
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn stream_video(
    pc: Arc<RTCPeerConnection>,
    track: Arc<VideoTrack>,
    shared: Arc<Shared>,
    codec: VideoCodecType,
) {
    if !wait_connected(&pc).await {
        return;
    }
    let video = shared.deps.video.clone();
    let settings = shared.settings_lock.lock().await;
    if let Err(e) = video.set_video_codec(codec).await {
        warn!("UU could not select {codec:?} on the shared pipeline: {e}");
        return;
    }
    let mut first_frames = video.subscribe_encoded_frames().await;
    drop(settings);
    let mut health = tokio::time::interval(Duration::from_millis(500));
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = 0u64;
    let mut hevc = HevcPackets::new();
    'subscribe: while !closed(pc.connection_state()) {
        shared.video_diagnostics.lock().await.sender = None;
        let subscribed_at = Instant::now();
        let subscribed = match first_frames.take() {
            Some(frames) => Some(frames),
            None => video.subscribe_encoded_frames().await,
        };
        let Some(mut frames) = subscribed else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        if let Some(config) = video.get_encoding_config().await {
            let screen = (
                Rect {
                    width: config.resolution.width as i32,
                    height: config.resolution.height as i32,
                },
                config.fps,
            );
            shared
                .screen
                .send_if_modified(|current| std::mem::replace(current, screen) != screen);
            info!(
                "UU video subscribed: {}x{} fps={} {}",
                screen.0.width, screen.0.height, screen.1, config.bitrate_preset
            );
        }
        let _ = video.request_keyframe().await;
        let mut timeline = RtpTimeline::new(shared.screen.borrow().1);
        let mut waiting_for_keyframe = true;
        let mut last_sequence: Option<u64> = None;
        let mut last_request = Instant::now();
        let mut report_at = Instant::now();
        let mut report_frames = 0u64;
        let mut report_encoded_bytes = 0u64;
        let mut send_us = 0u128;
        let mut max_send_us = 0u128;
        let mut max_gap_us = 0u128;
        let mut previous_arrival: Option<Instant> = None;
        let mut first_pts: Option<(Instant, i64)> = None;
        let mut sequence_gaps = 0u64;
        let mut frame_ages = FrameAgeStats::default();
        loop {
            let frame = tokio::select! {
                frame = frames.recv() => frame,
                _ = health.tick() => {
                    if closed(pc.connection_state()) { break 'subscribe; }
                    shared.refresh_screen().await;
                    continue;
                }
            };
            let Some(frame) = frame else {
                debug!("UU video pipeline restarted; re-subscribing");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue 'subscribe;
            };
            let expected = if codec == VideoCodecType::H265 {
                VideoEncoderType::H265
            } else {
                VideoEncoderType::H264
            };
            if frame.codec != expected {
                continue;
            }
            if codec == VideoCodecType::H265
                && crate::video::codec::h265_bitstream::conforms_to_profile(
                    &frame.data,
                    shared.hevc_profile,
                )
                .is_some_and(|compatible| !compatible)
            {
                warn!("UU HEVC profile changed; receiver must negotiate again");
                let _ = pc.close().await;
                return;
            }
            let gap = last_sequence.is_some_and(|prev| frame.sequence > prev.saturating_add(1));
            sequence_gaps += u64::from(gap && !waiting_for_keyframe);
            if waiting_for_keyframe || gap {
                if !frame.is_keyframe {
                    waiting_for_keyframe = true;
                    if last_request.elapsed() >= Duration::from_millis(300) {
                        last_request = Instant::now();
                        let _ = video.request_keyframe().await;
                    }
                    continue;
                }
                waiting_for_keyframe = false;
            }
            last_sequence = Some(frame.sequence);
            let arrived = Instant::now();
            if let Some(previous) = previous_arrival.replace(arrived) {
                max_gap_us = max_gap_us.max(arrived.duration_since(previous).as_micros());
            }
            let (pts_origin, pts_start) = *first_pts.get_or_insert((arrived, frame.pts_ms));
            let sample = Sample {
                data: frame.data.clone(),
                duration: timeline.duration_after(Instant::now()),
                ..Default::default()
            };
            let send = track.write(&sample, &mut hevc, frame.color_space);
            tokio::pin!(send);
            loop {
                tokio::select! {
                    result = &mut send => {
                        if let Err(e) = result { warn!("UU video send failed: {e}"); break 'subscribe; }
                        break;
                    }
                    _ = health.tick() => if closed(pc.connection_state()) { break 'subscribe; },
                }
            }
            sent += 1;
            frame_ages.observe(frame.encoded_at, arrived, Instant::now(), subscribed_at);
            let elapsed_us = arrived.elapsed().as_micros();
            send_us += elapsed_us;
            max_send_us = max_send_us.max(elapsed_us);
            report_frames += 1;
            report_encoded_bytes += frame.data.len() as u64;
            if report_at.elapsed() >= Duration::from_secs(5) {
                let fps = report_frames as f64 / report_at.elapsed().as_secs_f64();
                shared.video_diagnostics.lock().await.sender = Some((
                    Instant::now(),
                    UuVideoSender {
                        age_ms: 0.0,
                        window_ms: report_at.elapsed().as_secs_f64() * 1000.0,
                        fps,
                        mean_queue_ms: frame_ages.total_queue_us as f64
                            / frame_ages.frames as f64
                            / 1000.0,
                        max_queue_ms: frame_ages.max_queue_us as f64 / 1000.0,
                        max_encoded_to_send_ms: frame_ages.max_encoded_to_send_us as f64 / 1000.0,
                        cached_frames: frame_ages.cached_frames as f64,
                    },
                ));
                // Relative growth only: PTS has no shared absolute epoch with Instant.
                let pts_lag_growth_ms = arrived.duration_since(pts_origin).as_millis() as i128
                    - i128::from(frame.pts_ms.saturating_sub(pts_start));
                info!(
                    fps,
                    encoded_bytes = report_encoded_bytes,
                    encoded_kbps = report_encoded_bytes as f64 * 8.0
                        / report_at.elapsed().as_secs_f64()
                        / 1000.0,
                    mean_send_us = send_us / u128::from(report_frames),
                    max_send_us,
                    max_gap_us,
                    pts_lag_growth_ms,
                    sequence_gaps,
                    mean_encoded_queue_us = frame_ages.total_queue_us / frame_ages.frames,
                    max_encoded_queue_us = frame_ages.max_queue_us,
                    max_encoded_to_send_us = frame_ages.max_encoded_to_send_us,
                    cached_frames = frame_ages.cached_frames,
                    "UU sender timing"
                );
                report_at = Instant::now();
                report_frames = 0;
                report_encoded_bytes = 0;
                send_us = 0;
                max_send_us = 0;
                max_gap_us = 0;
                sequence_gaps = 0;
                frame_ages = FrameAgeStats::default();
            }
        }
    }
    info!("UU video ended: frames={sent}");
}

async fn stream_audio(
    pc: Arc<RTCPeerConnection>,
    track: Arc<TrackLocalStaticSample>,
    audio: Arc<AudioController>,
) {
    if !wait_connected(&pc).await {
        return;
    }
    let mut health = tokio::time::interval(Duration::from_millis(500));
    while !closed(pc.connection_state()) {
        let Some(mut packets) = audio.subscribe_opus().await else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        info!("UU audio subscribed");
        loop {
            tokio::select! {
                packet = packets.recv() => {
                    let Some(packet) = packet else { break };
                    let duration = Duration::from_millis(u64::from(packet.duration_ms.max(1)));
                    if let Err(e) = track.write_sample(&Sample { data: packet.data.clone(), duration, ..Default::default() }).await {
                        warn!("UU audio send failed: {e}");
                        return;
                    }
                }
                _ = health.tick() => if closed(pc.connection_state()) { return },
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn send(channel: &RTCDataChannel, message: Vec<u8>) -> bool {
    match channel.send(&Bytes::from(message)).await {
        Ok(_) => true,
        Err(e) => {
            debug!("UU CONTROL send failed: {e}");
            false
        }
    }
}

fn attach_terminal(channel: Arc<RTCDataChannel>, shared: Arc<Shared>) {
    let (out, mut frames) = tokio::sync::mpsc::channel(64);
    let conn = super::terminal::TerminalConn::new(shared.deps.terminal.clone(), out);
    if let Some(old) = shared.terminal_conn.lock().replace(conn.clone()) {
        old.close();
    }
    let sender = Arc::downgrade(&channel);
    let sending = conn.clone();
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                _ = sending.cancelled() => break,
                frame = frames.recv() => match frame { Some(frame) => frame, None => break },
            };
            sending.output_ready();
            let Some(channel) = sender.upgrade() else {
                break;
            };
            if !terminal_send(&sending, channel.send(&Bytes::from(frame))).await {
                break;
            }
        }
        sending.close();
        if let Some(channel) = sender.upgrade() {
            let _ = channel.close().await;
        }
    });
    let closing = conn.clone();
    channel.on_close(Box::new(move || {
        let conn = closing.clone();
        Box::pin(async move {
            conn.close();
        })
    }));
    let reply = Arc::downgrade(&channel);
    channel.on_message(Box::new(move |message: DataChannelMessage| {
        let (conn, shared, reply) = (conn.clone(), shared.clone(), reply.clone());
        Box::pin(async move {
            if message.data.starts_with(b"TERM") {
                conn.handle(&message.data).await;
            } else {
                if let Some(reply) = reply.upgrade() {
                    terminal_rpc(&reply, &shared, &message.data).await;
                }
            }
        })
    }));
}

#[cfg(target_os = "linux")]
fn attach_file(channel: Arc<RTCDataChannel>, connection: Arc<super::files::Connection>) {
    let weak = Arc::downgrade(&channel);
    let opened = connection.clone();
    channel.on_open(Box::new(move || {
        let (weak, connection) = (weak.clone(), opened.clone());
        Box::pin(async move {
            let Some(mut receiver) = connection.take_receiver() else { return };
            tokio::spawn(async move {
                loop {
                    let bytes = tokio::select! {
                        biased;
                        _ = connection.cancelled() => break,
                        bytes = receiver.recv() => match bytes { Some(bytes) => bytes, None => break },
                    };
                    let Some(channel) = weak.upgrade() else { break };
                    let bytes = Bytes::from(bytes);
                    let sent = tokio::select! {
                        biased;
                        _ = connection.cancelled() => false,
                        sent = tokio::time::timeout(Duration::from_secs(10), channel.send(&bytes)) => matches!(sent, Ok(Ok(_))),
                    };
                    if !sent { break; }
                }
                connection.close().await;
            });
        })
    }));
    let closing = connection.clone();
    channel.on_close(Box::new(move || {
        let connection = closing.clone();
        Box::pin(async move { connection.close().await })
    }));
    channel.on_message(Box::new(move |message| {
        let connection = connection.clone();
        Box::pin(async move {
            connection.handle(&message.data).await;
        })
    }));
}

fn terminal_connection_state(conn: &super::terminal::TerminalConn, state: PcState) {
    if matches!(state, PcState::Closed | PcState::Failed) {
        conn.close();
    }
}

async fn terminal_send<T, E>(
    conn: &super::terminal::TerminalConn,
    send: impl std::future::Future<Output = Result<T, E>>,
) -> bool {
    // SCTP may remain blocked while ICE recovers. Keep the same send future so
    // a timeout cannot discard or duplicate a partially accepted terminal frame.
    tokio::select! {
        _ = conn.cancelled() => false,
        result = send => result.is_ok(),
    }
}

async fn terminal_rpc(channel: &RTCDataChannel, shared: &Shared, data: &[u8]) -> bool {
    if shared.closed.is_cancelled() {
        return true;
    }
    let response = wire::decode(data).ok().and_then(|fields| {
        wire::message(&fields, wire::which::RPC_REQUEST)
            .and_then(|rpc| wire::decode(rpc).ok())
            .and_then(|rpc| shared.deps.terminal.rpc_response(&rpc))
    });
    if let Some(response) = response {
        // uu-remote-linux replies on the channel carrying the request, including BINARY.
        send(
            channel,
            wire::envelope(
                shared.next_seq(),
                now_secs(),
                wire::which::RPC_RESPONSE,
                &response,
            ),
        )
        .await;
        true
    } else {
        false
    }
}

fn attach_control(channel: Arc<RTCDataChannel>, shared: Arc<Shared>) {
    let (open_channel, open_shared) = (Arc::downgrade(&channel), shared.clone());
    channel.on_open(Box::new(move || {
        let (channel, shared) = (open_channel.clone(), open_shared.clone());
        Box::pin(async move {
            let Some(channel) = channel.upgrade() else {
                return;
            };
            shared.update_input(SessionInputEvent::ControlOpened).await;
            spawn_until_closed(shared.closed.clone(), control_supervisor(channel, shared));
        })
    }));
    let closing = shared.clone();
    channel.on_close(Box::new(move || {
        let shared = closing.clone();
        Box::pin(async move { shared.update_input(SessionInputEvent::Close).await })
    }));
    let reply = Arc::downgrade(&channel);
    channel.on_message(Box::new(move |message: DataChannelMessage| {
        let (channel, shared) = (reply.clone(), shared.clone());
        Box::pin(async move {
            if let Some(channel) = channel.upgrade() {
                handle_control(&channel, &shared, &message.data).await;
            }
        })
    }));
}

/// CaptureChange, SystemMetrics, ScreenSources and the unlocked state, then ScreenSources
/// again whenever the encoded size or rate changes.
async fn control_supervisor(channel: Arc<RTCDataChannel>, shared: Arc<Shared>) {
    let mut desktop = shared.desktop.subscribe();
    if !*desktop.borrow_and_update() {
        // Tools get nominal source metadata until they acquire the desktop role.
        let size = Rect {
            width: 1920,
            height: 1080,
        };
        send(
            &channel,
            wire::capture_change(shared.next_seq(), now_secs(), SCREEN_NAME),
        )
        .await;
        send(
            &channel,
            wire::system_metrics(shared.next_seq(), now_secs(), false),
        )
        .await;
        if let Ok(sources) = wire::screen_sources_with_output(
            shared.next_seq(),
            now_secs(),
            size,
            size,
            60,
            SCREEN_NAME,
        ) {
            send(&channel, sources).await;
        }
        send(
            &channel,
            wire::secure_desktop_unlocked(shared.next_seq(), now_secs()),
        )
        .await;
    }
    loop {
        if *desktop.borrow_and_update() {
            tokio::select! {
                _ = control_initialization(channel.clone(), shared.clone()) => return,
                changed = desktop.changed() => if changed.is_err() { return; },
            }
        } else if desktop.changed().await.is_err() {
            return;
        }
        if channel.ready_state() != RTCDataChannelState::Open {
            return;
        }
    }
}

async fn control_initialization(channel: Arc<RTCDataChannel>, shared: Arc<Shared>) {
    let sound = shared.deps.audio.is_streaming().await;
    let mut screen = shared.screen.subscribe();
    let (size, fps) = *screen.borrow_and_update();
    let ts = now_secs();
    let mut messages = vec![
        wire::capture_change(shared.next_seq(), ts, SCREEN_NAME),
        wire::system_metrics(shared.next_seq(), ts, sound),
        wire::codec_negotiation(shared.next_seq(), ts, size, fps, shared.codec),
    ];
    match wire::screen_sources_with_output(
        shared.next_seq(),
        ts,
        native_screen(size),
        size,
        fps as i32,
        SCREEN_NAME,
    ) {
        Ok(sources) => messages.push(sources),
        Err(e) => warn!("UU screen sources not sent: {e}"),
    }
    messages.push(wire::secure_desktop_unlocked(shared.next_seq(), ts));
    for message in messages {
        if !send(&channel, message).await {
            return;
        }
    }
    info!(
        "UU CONTROL initialization sent: {}x{} fps={fps} sound={sound}",
        size.width, size.height
    );
    let mut health = tokio::time::interval(Duration::from_secs(1));
    loop {
        let changed = tokio::select! {
            changed = screen.changed() => changed.is_ok(),
            _ = health.tick() => false,
        };
        if channel.ready_state() != RTCDataChannelState::Open || screen.has_changed().is_err() {
            return;
        }
        if !changed {
            continue;
        }
        let (size, fps) = *screen.borrow_and_update();
        let ts = now_secs();
        let Ok(sources) = wire::screen_sources_with_output(
            shared.next_seq(),
            ts,
            native_screen(size),
            size,
            fps as i32,
            SCREEN_NAME,
        ) else {
            continue;
        };
        let sync = shared.capture_sync();
        if !send(&channel, sources).await
            || !send(
                &channel,
                wire::envelope(
                    shared.next_seq(),
                    ts,
                    wire::which::CAPTURE_SETTING_SYNC,
                    &sync,
                ),
            )
            .await
        {
            return;
        }
        info!(
            "UU screen size update: {}x{} fps={fps}",
            size.width, size.height
        );
    }
}

async fn handle_control(channel: &RTCDataChannel, shared: &Arc<Shared>, data: &[u8]) {
    #[cfg(target_os = "linux")]
    if shared.files.handle(data).await {
        return;
    }
    if wire::decode(data)
        .ok()
        .and_then(|f| {
            wire::message(&f, wire::which::SIMPLE_ACTION).and_then(|a| wire::decode(a).ok())
        })
        .is_some_and(|a| wire::number(&a, 1) == Some(4))
    {
        if let Err(error) = shared.deps.desktop.claim(shared).await {
            warn!(%error, "UU desktop request rejected");
        }
        return;
    }
    if terminal_rpc(channel, shared, data).await {
        return;
    }
    if !*shared.desktop.borrow() {
        if let Ok(fields) = wire::decode(data) {
            if wire::message(&fields, wire::which::SIMPLE_ACTION)
                .and_then(|a| wire::decode(a).ok())
                .is_some_and(|a| wire::number(&a, 1).unwrap_or(0) == wire::action::ECHO_REQUEST)
            {
                send(
                    channel,
                    wire::terminal_echo_response(
                        shared.next_seq(),
                        now_secs(),
                        wire::number(&fields, 1).unwrap_or(0),
                    ),
                )
                .await;
            }
            if let Some(response) = wire::message(&fields, wire::which::RPC_REQUEST)
                .and_then(|r| wire::decode(r).ok())
                .and_then(|r| wire::session_rpc(&r))
            {
                send(
                    channel,
                    wire::envelope(
                        shared.next_seq(),
                        now_secs(),
                        wire::which::RPC_RESPONSE,
                        &response,
                    ),
                )
                .await;
            }
        }
        return;
    }
    // Input is JSON; '{' is an invalid protobuf group tag, so the two never overlap.
    let Ok(fields) = wire::decode(data) else {
        if let Some(event) = input::decode(data) {
            shared.update_input(SessionInputEvent::Input(&event)).await;
        }
        return;
    };
    let ts = now_secs();
    if let Some(rpc) =
        wire::message(&fields, wire::which::RPC_REQUEST).and_then(|r| wire::decode(r).ok())
    {
        if let Some((request, response)) = wire::capture_setting_rpc(&rpc) {
            // Do not send a success RPC if the encoder could not apply the request.
            if let Err(error) = apply_capture_setting(shared, request).await {
                warn!("UU capture settings not applied: {error}");
                return;
            }
            let sync = shared.capture_sync();
            send(
                channel,
                wire::envelope(shared.next_seq(), ts, wire::which::RPC_RESPONSE, &response),
            )
            .await;
            send(
                channel,
                wire::envelope(
                    shared.next_seq(),
                    ts,
                    wire::which::CAPTURE_SETTING_SYNC,
                    &sync,
                ),
            )
            .await;
        } else if let Some(response) = wire::session_rpc(&rpc).or_else(|| wire::plugin_rpc(&rpc)) {
            send(
                channel,
                wire::envelope(shared.next_seq(), ts, wire::which::RPC_RESPONSE, &response),
            )
            .await;
        } else {
            info!(
                "UU RPC not supported: fields={:?}",
                rpc.iter().map(|f| f.number).collect::<Vec<_>>()
            );
        }
        return;
    }
    let query =
        wire::message(&fields, wire::which::QUERY_SYSTEM_STATE).and_then(|q| wire::decode(q).ok());
    if query.is_some_and(|q| wire::number(&q, 1) == Some(1)) {
        send(
            channel,
            wire::secure_desktop_unlocked(shared.next_seq(), ts),
        )
        .await;
    }
    let Some(action) =
        wire::message(&fields, wire::which::SIMPLE_ACTION).and_then(|a| wire::decode(a).ok())
    else {
        if !fields.iter().any(|f| f.number == wire::which::INPUT) {
            debug!(
                "UU CONTROL message: fields={:?}",
                fields.iter().map(|f| f.number).collect::<Vec<_>>()
            );
        }
        return;
    };
    use CanonicalKey as K;
    let shortcut: &[K] = match wire::number(&action, 1).unwrap_or(0) {
        wire::action::ECHO_REQUEST => {
            let request_seq = wire::number(&fields, 1).unwrap_or(0);
            send(
                channel,
                wire::echo_response_with_hdr(
                    shared.next_seq(),
                    ts,
                    request_seq,
                    shared.hdr_control_available(),
                ),
            )
            .await;
            return;
        }
        wire::action::SHOW_DESKTOP
            if shared.deps.system == crate::config::UuRemoteSystem::Macos =>
        {
            &[K::MetaLeft, K::F3]
        }
        wire::action::LOCK_SYSTEM if shared.deps.system == crate::config::UuRemoteSystem::Macos => {
            &[K::ControlLeft, K::MetaLeft, K::KeyQ]
        }
        wire::action::SHOW_TASKMGR
            if shared.deps.system == crate::config::UuRemoteSystem::Macos =>
        {
            &[K::MetaLeft, K::AltLeft, K::Escape]
        }
        wire::action::SHOW_DESKTOP => &[K::MetaLeft, K::KeyD],
        wire::action::LOCK_SYSTEM => &[K::MetaLeft, K::KeyL],
        wire::action::SHOW_TASKMGR => &[K::ControlLeft, K::ShiftLeft, K::Escape],
        other => {
            info!("UU SimpleAction {other} not supported on a KVM");
            return;
        }
    };
    shared
        .update_input(SessionInputEvent::Shortcut(shortcut))
        .await;
}

fn native_screen(fallback: Rect) -> Rect {
    #[cfg(feature = "vim4")]
    if let Some((size, _)) = crate::platform::vim4::signal() {
        return Rect {
            width: size.width as i32,
            height: size.height as i32,
        };
    }
    fallback
}

#[cfg(feature = "vim4")]
fn requested_output(
    request: &wire::CaptureRequest,
    native: Rect,
    mut output: crate::platform::vim4::OutputSettings,
) -> crate::platform::vim4::OutputSettings {
    if let Some(fps) = request.fps {
        output.fps = fps.clamp(5, 120) as u32;
    }
    if let Some(size) = request.output {
        output.height = match size {
            None => 0,
            Some((width, height)) => {
                let height_from_width =
                    u128::from(width) * native.height as u128 / native.width.max(1) as u128;
                let height = u128::from(height)
                    .min(height_from_width)
                    .min(native.height as u128);
                if height >= native.height as u128 {
                    0
                } else {
                    (height.clamp(180, 2160) as u32) & !1
                }
            }
        };
    }
    output
}

async fn apply_capture_setting(
    shared: &Shared,
    mut request: wire::CaptureRequest,
) -> Result<(), String> {
    let _guard = shared.settings_lock.lock().await;
    if !*shared.desktop.borrow() || shared.closed.is_cancelled() {
        return Err("Desktop session is no longer active".into());
    }
    if request.hdr.is_some() && !shared.hdr_control_available() {
        return Err("HDR requires a negotiated Main10 receiver and native HDR capture".into());
    }
    let quality = request
        .quality
        .unwrap_or_else(|| shared.quality.load(Ordering::Relaxed));
    if !(wire::quality::FAST..=wire::quality::CUSTOM).contains(&quality) {
        return Err("Unsupported quality".into());
    }
    request.quality = Some(quality);
    if request.custom_bitrate.is_none() {
        let kbps = shared.custom_kbps.load(Ordering::Relaxed);
        request.custom_bitrate = Some(kbps * 1_000);
    }
    let preset = if quality == wire::quality::AUTO {
        shared.default_bitrate
    } else {
        preset_for(request.bitrate_kbps().ok_or("Missing bitrate")?)
    };
    #[cfg(feature = "vim4")]
    {
        shared
            .deps
            .video
            .set_video_codec(shared.codec)
            .await
            .map_err(|e| e.to_string())?;
        // CONTROL can arrive before the media task subscribes. Keep the shared
        // pipeline alive while an HDR profile change closes browser sessions.
        let _frames = shared
            .deps
            .video
            .subscribe_encoded_frames()
            .await
            .ok_or("Video pipeline unavailable")?;
        let current = shared
            .deps
            .video
            .get_encoding_config()
            .await
            .ok_or("Video pipeline unavailable")?;
        let mut output = requested_output(
            &request,
            native_screen(shared.screen.borrow().0),
            current.output,
        );
        if let Some(hdr) = request.hdr {
            output.hdr = hdr;
        }
        shared
            .deps
            .video
            .set_vim4_encoding(output, preset)
            .await
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(feature = "vim4"))]
    {
        if request.fps.is_some() || request.output.is_some() {
            return Err("Output controls require VIM4".into());
        }
        shared
            .deps
            .video
            .set_bitrate_preset(preset)
            .await
            .map_err(|e| e.to_string())?;
    }
    shared
        .actual_kbps
        .store(u64::from(preset.bitrate_kbps()), Ordering::Relaxed);
    if quality == wire::quality::CUSTOM {
        shared.custom_kbps.store(
            u64::from(request.bitrate_kbps().unwrap().clamp(1000, 60000)),
            Ordering::Relaxed,
        );
    }
    shared.quality.store(quality, Ordering::Relaxed);
    shared.refresh_screen().await;
    if request.hdr.is_some() {
        shared.report_hdr_capability().await;
    }
    let (size, fps) = *shared.screen.borrow();
    info!(
        "UU capture settings applied: {}x{} fps={fps} {preset}",
        size.width, size.height
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forwarded_capability_is_readable_by_the_viewer_message_dispatcher() {
        let (outgoing, mut received) = tokio::sync::mpsc::channel(2);
        let signaling = SignalingClient::with_test_sender(outgoing);
        for codec in [VideoCodecType::H264, VideoCodecType::H265] {
            let size = Rect {
                width: 3840,
                height: 2160,
            };
            signaling
                .emit_event(
                    FORWARD_SETTING_EVENT,
                    device_capability("test-client", "test-ice", size, 60, codec),
                )
                .await
                .unwrap();
            let packet = received.recv().await.unwrap();
            let envelope: serde_json::Value = serde_json::from_str(&packet[2..]).unwrap();
            assert_eq!(envelope[0], FORWARD_SETTING_EVENT);
            let dispatch = &envelope[1]["data"];
            assert_eq!(dispatch["type"], "device_capability");
            let capability = &dispatch[dispatch["type"].as_str().unwrap()];
            assert_eq!(capability["ice_id"], "test-ice");
            let formats = capability["video_codec_capability"].as_array().unwrap();
            assert_eq!(formats.len(), 1);
            assert_eq!(formats[0]["width"], size.width);
            assert_eq!(formats[0]["height"], size.height);
            assert_eq!(
                formats[0]["video_codec"],
                if codec == VideoCodecType::H265 { 2 } else { 1 }
            );
            assert_eq!(formats[0]["chroma_sampling"], 1);
            assert_eq!(capability["display_info"][0]["hdr"], -1);
        }
        signaling
            .emit_event(
                FORWARD_SETTING_EVENT,
                sender_parameters("test-client", "test-ice"),
            )
            .await
            .unwrap();
        let packet = received.recv().await.unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&packet[2..]).unwrap();
        let dispatch = &envelope[1]["data"];
        let parameters = &dispatch[dispatch["type"].as_str().unwrap()];
        assert_eq!(parameters["ice_id"], "test-ice");
        assert_eq!(parameters["sender_media_infos"][0]["video_track_index"], 0);
    }

    #[tokio::test]
    async fn terminal_survives_ice_disconnect_and_slow_send_until_closed() {
        use super::super::terminal::{frame::*, Registry, TerminalConn};
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path().join("terminal"));
        let mut config = crate::config::UuTerminalConfig::default();
        config.serial.enabled = true;
        registry.configure(&config, "test").await.unwrap();
        registry.start();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let conn = TerminalConn::new(registry.clone(), tx);
        conn.handle(&encode(OPEN, 0, b"{}")).await;
        let response = rx.recv().await.unwrap();
        let id = decode(&response).unwrap().session;
        terminal_connection_state(&conn, PcState::Disconnected);
        assert!(registry.status(1).await.sessions[0].attached);
        terminal_connection_state(&conn, PcState::Connected);
        conn.handle(&encode(RESIZE, id, br#"{"cols":30,"rows":24}"#))
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let bytes = rx.recv().await.unwrap();
                conn.output_ready();
                if String::from_utf8_lossy(decode(&bytes).unwrap().payload)
                    .contains("      未检测到设备")
                {
                    break;
                }
            }
        })
        .await
        .unwrap();

        let (release, pending) = tokio::sync::oneshot::channel::<()>();
        let sending = conn.clone();
        let task = tokio::spawn(async move { terminal_send(&sending, pending).await });
        // Cross the old five-second deadline with a deliberately blocked send.
        tokio::time::sleep(Duration::from_millis(5100)).await;
        assert!(!task.is_finished());
        release.send(()).unwrap();
        assert!(task.await.unwrap());
        assert!(registry.status(1).await.sessions[0].attached);
        terminal_connection_state(&conn, PcState::Failed);
        assert!(!registry.status(0).await.sessions[0].attached);
        assert!(!terminal_send(&conn, std::future::pending::<Result<(), ()>>()).await);
        registry.shutdown().await;
    }

    #[cfg(unix)]
    async fn terminal_peer_fixture() -> (Peer, RTCPeerConnection, String) {
        let viewer = APIBuilder::new()
            .build()
            .new_peer_connection(Default::default())
            .await
            .unwrap();
        viewer
            .create_data_channel("BINARY_DATA_CHANNEL", None)
            .await
            .unwrap();
        let offer = viewer.create_offer(None).await.unwrap();
        let soac: SoacMessage = serde_json::from_value(
            serde_json::json!({"type":"offer","sdp":offer.sdp,"ice_id":"terminal-test"}),
        )
        .unwrap();
        let video = VideoStreamManager::with_webrtc_streamer(
            crate::video::streamer::Streamer::new(),
            crate::webrtc::WebRtcStreamer::new(),
        );
        video
            .set_uu_negotiated_codec(Some(VideoCodecType::H265))
            .await;
        let (out, _receiver) = tokio::sync::mpsc::channel(32);
        let dir = tempfile::tempdir().unwrap();
        let terminal = super::super::terminal::Registry::new(dir.path().join("terminal"));
        let peer = Peer::new(
            PeerDeps {
                system: crate::config::UuRemoteSystem::Windows,
                reverse_scroll: false,
                video_codec: crate::config::UuVideoCodec::Auto,
                video: video.clone(),
                hid: Arc::new(HidController::new(crate::hid::HidBackendType::None, None)),
                audio: Arc::new(AudioController::default()),
                signaling: Arc::new(SignalingClient::with_test_sender(out)),
                terminal,
                desktop: Arc::new(DesktopCoordinator::default()),
                #[cfg(target_os = "linux")]
                files: super::super::files::storage::Storage::new(dir.path().join("files")),
            },
            SessionIce::from_header(
                &serde_json::json!({"iceServers":[{"urls":"stun:127.0.0.1:9"}]}),
            ),
            &soac,
            8,
        )
        .await
        .unwrap();
        let answer = peer.handle_offer(&soac).await.unwrap();
        (peer, viewer, answer)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_offer_has_no_media_and_preserves_desktop_codec() {
        let (peer, viewer, answer) = terminal_peer_fixture().await;
        let video = peer.video.clone();
        assert!(answer.contains("m=application"));
        assert!(!answer.contains("m=video"));
        assert!(!answer.contains("m=audio"));
        assert!(peer.pc.get_senders().await.is_empty());
        assert_eq!(
            video.uu_negotiated_codec().await,
            Some(VideoCodecType::H265)
        );
        peer.close().await;
        viewer.close().await.unwrap();
        assert_eq!(
            video.uu_negotiated_codec().await,
            Some(VideoCodecType::H265)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stuck_transport_close_releases_room_lock_and_cancels_application_tasks() {
        let (peer, viewer, _) = terminal_peer_fixture().await;
        let (entered, callback_started) = tokio::sync::oneshot::channel();
        let (callback_life, callback_dropped) = tokio::sync::oneshot::channel::<()>();
        let mut pending = Some((entered, callback_life));
        peer.pc
            .on_peer_connection_state_change(Box::new(move |state| {
                let pending = if state == PcState::Closed {
                    pending.take()
                } else {
                    None
                };
                Box::pin(async move {
                    if let Some((entered, _life)) = pending {
                        let _ = entered.send(());
                        std::future::pending::<()>().await;
                    }
                })
            }));
        let (started, task_started) = tokio::sync::oneshot::channel();
        let (task_life, task_dropped) = tokio::sync::oneshot::channel::<()>();
        spawn_until_closed(peer.shared.closed.clone(), async move {
            let _life = task_life;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        task_started.await.unwrap();
        let peer = Arc::new(peer);
        let room = Arc::new(Mutex::new(vec![peer.clone()]));
        let closing_room = room.clone();
        let closing = tokio::spawn(async move {
            let mut peers = closing_room.lock().await;
            peers.pop().unwrap().close().await;
        });
        callback_started.await.unwrap();
        assert!(room.try_lock().is_err());
        assert!(task_dropped.await.is_err());
        tokio::time::timeout(TRANSPORT_CLOSE_TIMEOUT + Duration::from_secs(2), closing)
            .await
            .unwrap()
            .unwrap();
        assert!(callback_dropped.await.is_err());
        assert!(
            room.lock().await.is_empty(),
            "the next room operation must make progress"
        );
        assert!(peer.shared.closed.is_cancelled());
        viewer.close().await.unwrap();
    }

    #[test]
    fn hevc_preserves_cra_with_mixed_prefixes_and_exact_fragment_boundary() {
        let mut data = vec![
            0, 0, 0, 1, 64, 1, 7, 0, 0, 1, 66, 1, 8, 0, 0, 0, 1, 68, 1, 9, 0, 0, 1, 42, 1,
        ];
        data.extend(vec![0x55; 2194]);
        let mut hevc = HevcPackets::new();
        let sample = Sample {
            data: Bytes::from(data),
            duration: ticks_to_duration(1500),
            ..Default::default()
        };
        let packets = hevc.packetize(&sample);
        // VPS/SPS/PPS as separate single NAL units plus two CRA fragments.
        assert_eq!(packets.len(), 5);
        assert_eq!((packets[0].payload[0] >> 1) & 63, 32);
        assert_eq!((packets[1].payload[0] >> 1) & 63, 33);
        assert_eq!((packets[2].payload[0] >> 1) & 63, 34);
        assert_eq!(packets[3].payload[2], 0x80 | 21);
        assert_eq!(packets[4].payload[2], 0x40 | 21);
        assert!(!packets[0].header.marker && !packets[3].header.marker);
        assert!(packets[4].header.marker);
        let body: Vec<u8> = packets[3..]
            .iter()
            .flat_map(|p| p.payload[3..].iter().copied())
            .collect();
        assert_eq!(body, vec![0x55; 2194]);
        assert!(packets
            .iter()
            .all(|p| p.header.timestamp == packets[0].header.timestamp));
        let next = hevc.packetize(&Sample {
            data: Bytes::from_static(&[0, 0, 1, 2, 1, 9]),
            ..sample
        });
        assert_eq!(
            next[0].header.timestamp,
            packets[0].header.timestamp.wrapping_add(1500)
        );
        assert_eq!(
            next[0].header.sequence_number,
            packets[4].header.sequence_number.wrapping_add(1)
        );
    }

    #[tokio::test]
    async fn hevc_answer_uses_h265_when_viewer_also_offers_h264() {
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        let api = APIBuilder::new().with_media_engine(media).build();
        let viewer = api.new_peer_connection(Default::default()).await.unwrap();
        let host = api.new_peer_connection(Default::default()).await.unwrap();
        viewer
            .add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .unwrap();
        let mut offer = viewer.create_offer(None).await.unwrap();
        // A real viewer allocates its own dynamic payload types, independently
        // of the host MediaEngine's default HEVC payload type (126).
        offer.sdp = offer.sdp.replace(" 126", " 104").replace(":126", ":104");
        assert!(offer.sdp.contains("H264/90000"));
        assert!(offer.sdp.contains("H265/90000"));
        let track = VideoTrack::new(VideoCodecType::H265, 1);
        let sender = host.add_track(track.local()).await.unwrap();
        host.set_remote_description(offer).await.unwrap();
        restrict_video_codec(&host, VideoCodecType::H265)
            .await
            .unwrap();
        let answer = host.create_answer(None).await.unwrap();
        assert!(answer.sdp.contains("H265/90000"));
        assert!(answer.sdp.contains("a=rtpmap:104 H265/90000"));
        assert!(!answer.sdp.contains("H264/90000"));
        host.set_local_description(answer).await.unwrap();
        let parameters = sender.get_parameters().await;
        assert_eq!(parameters.rtp_parameters.codecs.len(), 1);
        assert_eq!(parameters.rtp_parameters.codecs[0].payload_type, 104);
        host.close().await.unwrap();
        viewer.close().await.unwrap();
    }

    #[test]
    fn main10_requires_explicit_receiver_support_and_keeps_sdr_fallback() {
        use crate::config::UuVideoCodec as Preference;
        let offer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98 99 100\r\na=recvonly\r\na=rtpmap:98 H264/90000\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1;tx-mode=SRST\r\na=rtpmap:100 H265/90000\r\na=fmtp:100 profile-id=2;tx-mode=SRST\r\n";
        let allowed = [VideoCodecType::H264, VideoCodecType::H265];
        assert_eq!(
            select_video_codec_with_profile(offer, Preference::Auto, &allowed, 2).unwrap(),
            VideoCodecType::H265
        );
        let main_only = offer.replace("profile-id=2", "profile-id=1");
        assert_eq!(
            offered_hevc_profile(offer, Preference::Auto, &allowed, true),
            2
        );
        assert_eq!(
            offered_hevc_profile(offer, Preference::Auto, &allowed, false),
            1
        );
        assert_eq!(
            offered_hevc_profile(&main_only, Preference::H265, &allowed, true),
            1
        );
        assert_eq!(
            select_video_codec_with_profile(
                &main_only,
                Preference::H265,
                &allowed,
                offered_hevc_profile(&main_only, Preference::H265, &allowed, true)
            )
            .unwrap(),
            VideoCodecType::H265
        );
        assert_eq!(
            select_video_codec_with_profile(&main_only, Preference::Auto, &allowed, 2).unwrap(),
            VideoCodecType::H264
        );
        assert!(
            select_video_codec_with_profile(&main_only, Preference::H265, &allowed, 2).is_err()
        );
        let unspecified = offer.replace("profile-id=2;", "");
        assert_eq!(
            select_video_codec_with_profile(&unspecified, Preference::Auto, &allowed, 2).unwrap(),
            VideoCodecType::H264
        );
    }

    #[test]
    fn codec_negotiation_falls_back_without_claiming_unsupported_profiles() {
        use crate::config::UuVideoCodec as Preference;
        let offer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98 99\r\na=recvonly\r\na=rtpmap:98 H264/90000\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1;tx-mode=SRST\r\n";
        let both = [VideoCodecType::H264, VideoCodecType::H265];
        // A shared H.264 consumer or an explicit preference must also constrain
        // the CONTROL announcement, even when the viewer offers both codecs.
        for (preference, allowed, expected) in [
            (Preference::Auto, &both[..], 2),
            (Preference::H264, &both[..], 1),
            (Preference::Auto, &both[..1], 1),
        ] {
            let codec = select_video_codec(offer, preference, allowed).unwrap();
            let announcement = wire::codec_negotiation(
                7,
                123,
                Rect {
                    width: 1920,
                    height: 1080,
                },
                30,
                codec,
            );
            let envelope = wire::decode(&announcement).unwrap();
            let caps =
                wire::decode(wire::message(&envelope, wire::which::CODEC_NEGOTIATION).unwrap())
                    .unwrap();
            assert_eq!(
                caps.len(),
                1,
                "must not advertise codecs locked out of this session"
            );
            let cap = wire::decode(wire::message(&caps, 1).unwrap()).unwrap();
            assert_eq!(wire::number(&cap, 3), Some(expected));
            assert_eq!(wire::number(&cap, 2), Some(30));
            assert_eq!(wire::number(&cap, 4), Some(1920));
            assert_eq!(wire::number(&cap, 5), Some(1080));
        }
        assert_eq!(
            select_video_codec(offer, Preference::Auto, &both).unwrap(),
            VideoCodecType::H265
        );
        assert_eq!(
            select_video_codec(offer, Preference::Auto, &both[..1]).unwrap(),
            VideoCodecType::H264
        );
        assert_eq!(
            select_video_codec(
                &offer.replace("profile-id=1", "profile-id=2"),
                Preference::Auto,
                &both
            )
            .unwrap(),
            VideoCodecType::H264
        );
        assert!(select_video_codec(
            &offer.replace("profile-id=1", "profile-id=2"),
            Preference::H265,
            &both
        )
        .is_err());
        assert_eq!(
            select_video_codec(&offer.replace(" 98 99", " 98"), Preference::Auto, &both).unwrap(),
            VideoCodecType::H264
        );
        assert!(select_video_codec(
            &offer.replace("m=video 9", "m=video 0"),
            Preference::Auto,
            &both
        )
        .is_err());
        assert!(select_video_codec(
            &offer.replace("recvonly", "sendonly"),
            Preference::Auto,
            &both
        )
        .is_err());
    }

    #[cfg(feature = "vim4")]
    #[test]
    fn viewer_output_preserves_aspect_caps_fps_and_restores_native() {
        use crate::platform::vim4::OutputSettings;
        let native = Rect {
            width: 3840,
            height: 2160,
        };
        let request = wire::CaptureRequest {
            fps: Some(144),
            output: Some(Some((1920, 1200))),
            ..Default::default()
        };
        let output = requested_output(&request, native, OutputSettings::default());
        assert_eq!((output.height, output.fps), (1080, 120));
        let native_request = wire::CaptureRequest {
            output: Some(None),
            ..Default::default()
        };
        assert_eq!(requested_output(&native_request, native, output).height, 0);
        let huge = wire::CaptureRequest {
            output: Some(Some((u64::MAX, u64::MAX))),
            ..Default::default()
        };
        assert_eq!(requested_output(&huge, native, output).height, 0);
    }

    #[test]
    fn bundle_transport_skips_rejected_sections_and_follows_group_order() {
        let sdp = "a=group:BUNDLE 1 2\r\nm=audio 0 RTP 0\r\na=mid:0\r\nm=video 9 RTP 98\r\na=mid:1\r\nm=application 9 SCTP 0\r\na=mid:2\r\n";
        assert_eq!(negotiated_bundle_media(sdp), Some(("1".into(), 1)));
        let sdp = "a=group:BUNDLE data video\nm=video 9 RTP 98\na=mid:video\nm=application 9 SCTP 0\na=mid:data\n";
        assert_eq!(negotiated_bundle_media(sdp), Some(("data".into(), 1)));
        assert_eq!(negotiated_bundle_media("m=video 9 RTP 98\n"), None);
    }

    #[test]
    fn rtp_ticks_survive_webrtc_floor_conversion() {
        for ticks in (1..=200_000).chain([90_000 * 60, 90_000 * 3600]) {
            assert_eq!(
                (ticks_to_duration(ticks).as_secs_f64() * 90_000.0) as u64,
                ticks
            );
        }
    }

    #[test]
    fn timeline_follows_real_arrival_and_recovers_after_stall() {
        let start = Instant::now();
        let mut timeline = RtpTimeline::new(60);
        let mut stamp = 0u64;
        let stamps: Vec<u64> = [0u64, 17, 517, 534]
            .iter()
            .map(|ms| {
                let current = stamp;
                stamp += (timeline
                    .duration_after(start + Duration::from_millis(*ms))
                    .as_secs_f64()
                    * 90_000.0) as u64;
                current
            })
            .collect();
        assert_eq!(stamps[2], 17 * 90 + 1500);
        assert_eq!(stamps[3], 517 * 90 + 1500);
    }

    #[test]
    fn metadata_extensions_use_network_order() {
        use webrtc::util::Marshal;
        let mut bytes = [0u8; 2];
        assert_eq!(CaptureIndex(0x1234).marshal_to(&mut bytes).unwrap(), 2);
        assert_eq!(bytes, [0x12, 0x34]);
        assert!(CaptureIndex(1).marshal_to(&mut [0u8; 1]).is_err());
        let mut flag = [0u8];
        NewFrame.marshal_to(&mut flag).unwrap();
        assert_eq!(flag, [1]);
    }

    #[test]
    fn viewer_bitrates_map_to_shared_presets_within_encoder_range() {
        assert_eq!(preset_for(1_000), BitratePreset::Speed);
        assert_eq!(preset_for(8_000), BitratePreset::Quality);
        assert_eq!(preset_for(20_000), BitratePreset::Custom(20_000));
        assert_eq!(preset_for(100_000), BitratePreset::Custom(60_000));
    }

    #[test]
    fn ice_skips_overlay_and_usb_gadget_interfaces() {
        for name in [
            "ztfp6jdvog",
            "docker0",
            "br-1a2b",
            "veth12",
            "lo",
            "usb0",
            "tailscale0",
        ] {
            assert!(!ice_interface_allowed(name), "{name}");
        }
        for name in ["eth0", "wlan0", "enp5s0"] {
            assert!(ice_interface_allowed(name), "{name}");
        }
    }

    #[test]
    fn gzip_offer_is_decoded() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"v=0\r\n").unwrap();
        let gzip = base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());
        let soac = SoacMessage {
            msg_type: "offer".into(),
            sdp: None,
            gzip_sdp: Some(gzip),
            candidate: None,
            ice_id: None,
            sdp_mid: None,
            sdp_mline_index: None,
            client_id: None,
            app_control_id: None,
        };
        assert_eq!(offer_sdp(&soac).as_deref(), Some("v=0\r\n"));
    }
}
