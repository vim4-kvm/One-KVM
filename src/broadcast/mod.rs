//! Device-owned network outputs subscribe to the same encoder as KVM viewers.
mod mux;

use crate::{
    audio::AudioController,
    config::{BroadcastConfig, ConfigStore},
    video::{codec::VideoEncoderType, pipeline::EncodedVideoFrame, VideoStreamManager},
};
use anyhow::{bail, Context, Result};
use mux::{Destination, Mux};
use serde::Serialize;
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, Serialize)]
pub struct OutputStatus {
    pub state: &'static str,
    pub error: Option<String>,
    pub codec: Option<&'static str>,
    pub frames: u64,
    pub bytes_sent: u64,
    pub clients: u64,
    pub restarts: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct BroadcastStatus {
    pub srt: OutputStatus,
    pub rtmp: OutputStatus,
}

struct Session {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct Sessions {
    srt: Option<Session>,
    rtmp: Option<Session>,
}

#[derive(Default)]
pub struct Broadcaster {
    sessions: Mutex<Sessions>,
    srt: Arc<RwLock<OutputStatus>>,
    rtmp: Arc<RwLock<OutputStatus>>,
}

impl Broadcaster {
    pub async fn status(&self) -> BroadcastStatus {
        BroadcastStatus {
            srt: self.srt.read().await.clone(),
            rtmp: self.rtmp.read().await.clone(),
        }
    }

    pub async fn stop(&self) {
        let mut sessions = self.sessions.lock().await;
        stop_session(&mut sessions.srt).await;
        stop_session(&mut sessions.rtmp).await;
    }

    pub fn watch(
        self: Arc<Self>,
        config: ConfigStore,
        video: Arc<VideoStreamManager>,
        audio: Arc<AudioController>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut changes = config.subscribe();
            let mut current: Option<BroadcastConfig> = None;
            loop {
                let desired = config.get().broadcast.clone();
                if current.as_ref() != Some(&desired) {
                    let mut sessions = self.sessions.lock().await;
                    if desired.validate().is_ok() {
                        if current.as_ref().map(|c| &c.srt) != Some(&desired.srt) {
                            stop_session(&mut sessions.srt).await;
                            *self.srt.write().await = OutputStatus {
                                state: "disabled",
                                ..Default::default()
                            };
                            if desired.srt.enabled {
                                sessions.srt = Some(spawn_output(
                                    Destination::Srt(desired.srt.clone()),
                                    video.clone(),
                                    audio.clone(),
                                    self.srt.clone(),
                                ));
                            }
                        }
                        if current.as_ref().map(|c| &c.rtmp) != Some(&desired.rtmp) {
                            stop_session(&mut sessions.rtmp).await;
                            *self.rtmp.write().await = OutputStatus {
                                state: "disabled",
                                ..Default::default()
                            };
                            if desired.rtmp.enabled {
                                sessions.rtmp = Some(spawn_output(
                                    Destination::Rtmp(desired.rtmp.clone()),
                                    video.clone(),
                                    audio.clone(),
                                    self.rtmp.clone(),
                                ));
                            }
                        }
                    } else {
                        stop_session(&mut sessions.srt).await;
                        stop_session(&mut sessions.rtmp).await;
                        for status in [&self.srt, &self.rtmp] {
                            status.write().await.state = "invalid_config";
                        }
                    }
                    current = Some(desired);
                }
                if matches!(
                    changes.recv().await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed)
                ) {
                    break;
                }
            }
        })
    }
}

async fn stop_session(session: &mut Option<Session>) {
    if let Some(session) = session.take() {
        session.stop.cancel();
        session.task.abort();
        let _ = session.task.await;
    }
}

fn spawn_output(
    destination: Destination,
    video: Arc<VideoStreamManager>,
    audio: Arc<AudioController>,
    status: Arc<RwLock<OutputStatus>>,
) -> Session {
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let task = tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let codec = video.current_video_codec().await;
            let supported = video.is_webrtc_enabled().await
                && match &destination {
                    Destination::Srt(_) => matches!(
                        codec,
                        crate::video::codec::VideoCodecType::H264
                            | crate::video::codec::VideoCodecType::H265
                    ),
                    Destination::Rtmp(_) => codec == crate::video::codec::VideoCodecType::H264,
                };
            if !supported {
                status.write().await.state = if matches!(destination, Destination::Rtmp(_)) {
                    "requires_h264"
                } else {
                    "requires_h26x"
                };
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(1)) => continue }
            }
            {
                let mut s = status.write().await;
                s.state = "connecting";
                s.error = None;
                s.frames = 0;
                s.bytes_sent = 0;
                s.clients = 0;
            }
            let started = Instant::now();
            let result = tokio::select! {
                _ = cancel.cancelled() => break,
                result = stream(&destination, &video, &audio, &status) => result,
            };
            {
                let mut s = status.write().await;
                s.state = "retrying";
                s.error = result.err().map(|e| e.to_string());
                s.clients = 0;
                s.restarts += 1;
            }
            if started.elapsed() >= Duration::from_secs(15) {
                backoff = 1;
            }
            tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(backoff)) => {} }
            backoff = (backoff * 2).min(15);
        }
    });
    Session { stop, task }
}

async fn stream(
    destination: &Destination,
    video: &VideoStreamManager,
    audio: &AudioController,
    status: &RwLock<OutputStatus>,
) -> Result<()> {
    let with_audio = destination.audio();
    if with_audio {
        if !audio.status().await.enabled {
            bail!("Enable HDMI audio or disable audio for this output");
        }
        audio
            .start_streaming()
            .await
            .context("HDMI audio is unavailable")?;
    }
    let mut packets = if with_audio {
        Some(
            audio
                .subscribe_opus()
                .await
                .context("HDMI audio is unavailable")?,
        )
    } else {
        None
    };
    // Subscribe before retrieving encoder configuration: a device-owned output
    // also starts the shared capture when no browser is connected.
    let mut frames = video
        .subscribe_encoded_frames()
        .await
        .context("Video is unavailable")?;
    let config = video
        .get_encoding_config()
        .await
        .context("Video is unavailable")?;
    let hevc = config.output_codec == VideoEncoderType::H265;
    if !matches!(
        config.output_codec,
        VideoEncoderType::H264 | VideoEncoderType::H265
    ) {
        bail!("Select H.264 or H.265");
    }
    let destination_copy = match destination {
        Destination::Srt(c) => Destination::Srt(c.clone()),
        Destination::Rtmp(c) => Destination::Rtmp(c.clone()),
    };
    let mux = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || Mux::new(destination_copy, hevc)),
    )
    .await
    .context("Output connection timed out")???;
    let subscribed_at = Instant::now();
    video.request_keyframe().await?;
    let first = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let frame = frames.recv().await.context("Video capture stopped")?;
            if let Some(packets) = &mut packets {
                while packets.try_recv().is_ok() {}
            }
            if frame.encoded_at >= subscribed_at
                && frame.is_keyframe
                && frame.codec == config.output_codec
            {
                return Ok::<_, anyhow::Error>(frame);
            }
        }
    })
    .await
    .context("Waiting for a video keyframe timed out")??;
    push_video(&mux, &first, first.pts_ms)?;
    let mut sequence = first.sequence;
    let mut pts_ms = first.pts_ms;
    let mut last_video = Instant::now();
    let mut last_audio = last_video;
    let mut audio_origin = None;
    let mut last_keyframe_request = Instant::now();
    let mut health = tokio::time::interval(Duration::from_millis(200));
    {
        let mut s = status.write().await;
        s.codec = Some(if hevc { "h265" } else { "h264" });
        s.frames = 1;
    }
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let frame = frame.context("Video capture stopped")?;
                if frame.codec != first.codec || frame.color_space != first.color_space || frame.sequence != sequence + 1 || frame.pts_ms <= pts_ms { bail!("Video changed or output fell behind; restarting from a keyframe"); }
                push_video(&mux, &frame, first.pts_ms)?;
                sequence = frame.sequence; pts_ms = frame.pts_ms; last_video = Instant::now();
                status.write().await.frames += 1;
            }
            packet = async { match &mut packets { Some(rx) => rx.recv().await, None => std::future::pending().await } } => {
                let packet = packet.context("HDMI audio stopped")?;
                if packet.encoded_at < first.encoded_at { continue; }
                let (start_sequence, offset, previous_sequence) = audio_origin.get_or_insert((packet.sequence, packet.encoded_at.duration_since(first.encoded_at), packet.sequence.saturating_sub(1)));
                if packet.sequence != *previous_sequence + 1 { bail!("Audio output fell behind"); }
                *previous_sequence = packet.sequence;
                let pts = *offset + Duration::from_millis((packet.sequence - *start_sequence) * u64::from(packet.duration_ms));
                mux.push(true, &packet.data, pts, Duration::from_millis(u64::from(packet.duration_ms)), true)?;
                last_audio = Instant::now();
            }
            _ = health.tick() => {
                mux.check()?;
                if last_video.elapsed() > Duration::from_secs(3) || (with_audio && last_audio.elapsed() > Duration::from_secs(3)) { bail!("Video or audio capture timed out"); }
                let current = video.get_encoding_config().await.context("Video capture stopped")?;
                if current.output_codec != config.output_codec || current.resolution != config.resolution { bail!("Video format changed"); }
                if mux.request_keyframe.load(Ordering::Relaxed) && last_keyframe_request.elapsed() >= Duration::from_millis(500) {
                    mux.request_keyframe.store(false, Ordering::Relaxed);
                    video.request_keyframe().await?;
                    last_keyframe_request = Instant::now();
                }
                let (bytes, clients) = mux.transport();
                let mut s = status.write().await;
                s.bytes_sent = bytes; s.clients = clients;
                s.state = if matches!(destination, Destination::Srt(_)) && clients == 0 { "listening" } else if bytes == 0 { "connecting" } else { "streaming" };
            }
        }
    }
}

fn push_video(mux: &Mux, frame: &EncodedVideoFrame, origin_ms: i64) -> Result<()> {
    mux.push(
        false,
        &frame.data,
        Duration::from_millis((frame.pts_ms - origin_ms) as u64),
        frame.duration,
        frame.is_keyframe,
    )
}
