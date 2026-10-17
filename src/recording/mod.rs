//! One device-owned recording, independent of browser and playback audio state.
mod mux;
mod storage;

use crate::audio::OpusFrame;
use crate::{
    audio::AudioController,
    error::{AppError, Result},
    video::{
        codec::VideoEncoderType,
        pipeline::{EncodedVideoFrame, EncodedVideoFrameReceiver},
        VideoStreamManager,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
pub use storage::RecordingFile;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingErrorCode {
    AlreadyRecording,
    VideoCodec,
    AudioUnavailable,
    LowStorage,
    StorageError,
    VideoUnavailable,
    StartTimeout,
    CodecChanged,
    ResolutionChanged,
    PacketsLost,
    CaptureTimeout,
    PipelineStopped,
    SaveFailed,
    Internal,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct RecordingFailure {
    code: RecordingErrorCode,
    message: String,
}

fn failure(code: RecordingErrorCode, message: impl std::fmt::Display) -> anyhow::Error {
    RecordingFailure {
        code,
        message: message.to_string(),
    }
    .into()
}

pub(crate) fn error_code(error: &anyhow::Error) -> RecordingErrorCode {
    error
        .downcast_ref::<RecordingFailure>()
        .map_or(RecordingErrorCode::Internal, |e| e.code)
}

fn error_code_or_storage(error: &anyhow::Error) -> RecordingErrorCode {
    error
        .downcast_ref::<RecordingFailure>()
        .map_or(RecordingErrorCode::StorageError, |e| e.code)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingCodec {
    H264,
    H265,
}

#[derive(Clone, Default, Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    pub file: Option<String>,
    pub elapsed_seconds: u64,
    pub error: Option<String>,
    pub error_code: Option<RecordingErrorCode>,
    pub started_at: Option<u64>,
    pub codec: Option<RecordingCodec>,
    pub bytes: u64,
    pub storage: Option<storage::StorageStatus>,
    pub blocker: Option<RecordingErrorCode>,
}

struct Session {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

pub struct Recorder {
    dir: PathBuf,
    // Serializes start, stop and deletion, including finalization.
    session: Mutex<Option<Session>>,
    status: Arc<RwLock<RecordingStatus>>,
}

fn recording_error(error: impl std::fmt::Display) -> AppError {
    AppError::BadRequest(error.to_string())
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            session: Mutex::new(None),
            status: Arc::new(RwLock::new(RecordingStatus::default())),
        }
    }

    pub async fn status(&self) -> RecordingStatus {
        let mut status = self.status.read().await.clone();
        status.storage = storage::space(&self.dir).ok();
        status
    }

    pub async fn start(
        &self,
        video: Arc<VideoStreamManager>,
        audio: Arc<AudioController>,
    ) -> anyhow::Result<RecordingStatus> {
        let mut session = self.session.lock().await;
        if session
            .as_ref()
            .is_some_and(|session| !session.task.is_finished())
        {
            return Err(failure(
                RecordingErrorCode::AlreadyRecording,
                "A recording is already running",
            ));
        }
        if let Some(previous) = session.take() {
            let _ = previous.task.await;
        }
        let result = self.start_inner(video, audio).await;
        match result {
            Ok(new_session) => {
                *session = Some(new_session);
                Ok(self.status().await)
            }
            Err(error) => {
                let mut status = self.status.write().await;
                status.error = Some(error.to_string());
                status.error_code = Some(error_code(&error));
                Err(error)
            }
        }
    }

    async fn start_inner(
        &self,
        video: Arc<VideoStreamManager>,
        audio: Arc<AudioController>,
    ) -> anyhow::Result<Session> {
        storage::prepare(&self.dir).map_err(|e| failure(error_code_or_storage(&e), e))?;
        if !video.is_webrtc_enabled().await {
            return Err(failure(
                RecordingErrorCode::VideoCodec,
                "Select H.264 or H.265 video before recording",
            ));
        }
        // Use the configured shared audio source. Disabled/missing audio is an
        // explicit failure, never a silent video-only recording.
        audio
            .start_streaming()
            .await
            .map_err(|e| failure(RecordingErrorCode::AudioUnavailable, e))?;
        let mut frames = video.subscribe_encoded_frames().await.ok_or_else(|| {
            failure(
                RecordingErrorCode::VideoUnavailable,
                "Video capture is unavailable",
            )
        })?;
        let config = video.get_encoding_config().await.ok_or_else(|| {
            failure(
                RecordingErrorCode::VideoUnavailable,
                "Video configuration is unavailable",
            )
        })?;
        if !matches!(
            config.output_codec,
            VideoEncoderType::H264 | VideoEncoderType::H265
        ) {
            return Err(failure(
                RecordingErrorCode::VideoCodec,
                "Recording requires H.264 or H.265 video",
            ));
        }
        let mut packets = audio.subscribe_opus().await.ok_or_else(|| {
            failure(
                RecordingErrorCode::AudioUnavailable,
                "Audio capture is unavailable",
            )
        })?;
        let (name, file) =
            storage::create(&self.dir).map_err(|e| failure(RecordingErrorCode::StorageError, e))?;
        let mux = match mux::Mux::new(file, config.output_codec == VideoEncoderType::H265) {
            Ok(mux) => mux,
            Err(error) => {
                let _ = std::fs::remove_file(self.dir.join(&name));
                return Err(error);
            }
        };
        // Pipeline/plugin setup may take longer than the subscriber queue. Only
        // start the recording clock after setup and discard bootstrap frames.
        let subscribed_at = Instant::now();
        let first = tokio::time::timeout(Duration::from_secs(8), async {
            video
                .request_keyframe()
                .await
                .map_err(|e| failure(RecordingErrorCode::VideoUnavailable, e))?;
            let first_video = loop {
                let frame = frames.recv().await.ok_or_else(|| {
                    failure(RecordingErrorCode::CaptureTimeout, "Video capture stopped")
                })?;
                if frame.encoded_at >= subscribed_at && frame.is_keyframe {
                    break frame;
                }
                // Do not retain stale audio while waiting for a fresh keyframe.
                while packets.try_recv().is_ok() {}
            };
            if first_video.codec != config.output_codec {
                return Err(failure(
                    RecordingErrorCode::CodecChanged,
                    "Video codec changed while starting recording",
                ));
            }
            let first_audio = loop {
                let packet = packets.recv().await.ok_or_else(|| {
                    failure(RecordingErrorCode::CaptureTimeout, "Audio capture stopped")
                })?;
                if packet.encoded_at >= first_video.encoded_at {
                    break packet;
                }
            };
            Ok::<_, anyhow::Error>((first_video, first_audio))
        })
        .await
        .map_err(|_| {
            failure(
                RecordingErrorCode::StartTimeout,
                "Timed out waiting for video keyframe and HDMI audio",
            )
        })
        .and_then(|result| result);
        let (first_video, first_audio) = match first {
            Ok(first) => first,
            Err(error) => {
                drop(mux);
                let _ = std::fs::remove_file(self.dir.join(&name));
                return Err(error);
            }
        };
        let setup = async {
            let resolution = video
                .get_encoding_config()
                .await
                .ok_or_else(|| {
                    failure(
                        RecordingErrorCode::PipelineStopped,
                        "Video pipeline stopped",
                    )
                })?
                .resolution;
            let metadata = storage::Metadata {
                started_at: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs(),
                ),
                codec: Some(if config.output_codec == VideoEncoderType::H265 {
                    RecordingCodec::H265
                } else {
                    RecordingCodec::H264
                }),
                ..Default::default()
            };
            storage::write_metadata(&self.dir, &name, &metadata)
                .map_err(|e| failure(RecordingErrorCode::StorageError, e))?;
            Ok::<_, anyhow::Error>((resolution, metadata))
        }
        .await;
        let (resolution, mut metadata) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                drop(mux);
                let _ = storage::delete(&self.dir, &name);
                return Err(error);
            }
        };
        *self.status.write().await = RecordingStatus {
            recording: true,
            file: Some(name.clone()),
            started_at: metadata.started_at,
            codec: metadata.codec,
            ..Default::default()
        };
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let status = self.status.clone();
        let dir = self.dir.clone();
        let task = tokio::spawn(async move {
            let mut duration_seconds = 0.0;
            let origin = first_video.encoded_at;
            let result = record(
                &mux,
                &video,
                &mut frames,
                &mut packets,
                first_video,
                first_audio,
                resolution,
                &task_stop,
                &dir,
                &status,
                &mut duration_seconds,
            )
            .await;
            let finalized = mux.finish().await;
            drop(mux);
            let mut code = result.as_ref().err().map(error_code);
            let mut error = result.err().map(|error| error.to_string());
            let mut final_name = name.clone();
            if let Err(failure) = finalized {
                code = Some(RecordingErrorCode::SaveFailed);
                error = Some(format!(
                    "{}{}",
                    error.map(|e| format!("{e}; ")).unwrap_or_default(),
                    failure
                ));
            } else {
                let complete = name.trim_end_matches(".part").to_owned();
                match std::fs::rename(dir.join(&name), dir.join(&complete)) {
                    Ok(()) => final_name = complete,
                    Err(failure) => {
                        code = Some(RecordingErrorCode::SaveFailed);
                        error = Some(format!("Could not save recording: {failure}"));
                    }
                }
            }
            metadata.duration_seconds = Some(duration_seconds);
            metadata.stop_reason = code;
            if let Err(failure) = storage::write_metadata(&dir, &final_name, &metadata) {
                code = Some(RecordingErrorCode::SaveFailed);
                error = Some(format!("Could not save recording metadata: {failure}"));
            }
            if let Some(error) = &error {
                tracing::warn!("Recording stopped: {error}");
            }
            let mut current = status.write().await;
            current.recording = false;
            current.file = Some(final_name);
            current.error = error;
            current.error_code = code;
            current.elapsed_seconds = origin.elapsed().as_secs();
            current.bytes = storage::open(&dir, current.file.as_deref().unwrap())
                .and_then(|f| Ok(f.metadata()?.len()))
                .unwrap_or(current.bytes);
        });
        Ok(Session { stop, task })
    }

    pub async fn stop(&self) -> RecordingStatus {
        let mut session = self.session.lock().await;
        if let Some(session) = session.as_mut() {
            session.stop.cancel();
            if let Err(error) = (&mut session.task).await {
                let mut status = self.status.write().await;
                status.recording = false;
                status.error = Some(format!("Recording task failed: {error}"));
                status.error_code = Some(RecordingErrorCode::Internal);
            }
        }
        *session = None;
        self.status().await
    }

    pub fn files(&self) -> Result<Vec<RecordingFile>> {
        storage::list(&self.dir).map_err(recording_error)
    }

    pub async fn open(&self, name: &str) -> Result<std::fs::File> {
        let _session = self.session.lock().await;
        self.ensure_inactive(name).await?;
        storage::open(&self.dir, name).map_err(recording_error)
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        let _session = self.session.lock().await;
        self.ensure_inactive(name).await?;
        storage::delete(&self.dir, name).map_err(recording_error)?;
        Ok(())
    }

    pub fn download_name(&self, name: &str, file: &std::fs::File) -> Result<String> {
        let modified = file
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(recording_error)?
            .as_secs();
        Ok(storage::download_name(
            storage::read_metadata(&self.dir, name).started_at,
            modified,
        ))
    }

    async fn ensure_inactive(&self, name: &str) -> Result<()> {
        let status = self.status.read().await;
        if status.recording && status.file.as_deref() == Some(name) {
            return Err(AppError::Conflict(
                "Stop recording before downloading or deleting it".into(),
            ));
        }
        Ok(())
    }
}

// Media time is retained even if subscriber scheduling is bursty. Both clocks
// share a monotonic output-time anchor; capture/codec latency is not calibrated.
struct Timeline {
    sequence: u64,
    pts_ms: i64,
}
impl Timeline {
    fn next(&mut self, sequence: u64, pts_ms: i64) -> anyhow::Result<()> {
        if sequence != self.sequence + 1 || pts_ms <= self.pts_ms {
            return Err(failure(
                RecordingErrorCode::PacketsLost,
                "Media stream changed or recording packets were lost",
            ));
        }
        self.sequence = sequence;
        self.pts_ms = pts_ms;
        Ok(())
    }
}

/// A codec switch stops the pipeline before publishing the new codec, so wait briefly to classify it.
async fn closed_video_reason(video: &VideoStreamManager, codec: VideoEncoderType) -> anyhow::Error {
    for _ in 0..20 {
        let current = crate::video::codec_constraints::video_codec_to_encoder_codec(
            video.current_video_codec().await,
        );
        if current != codec {
            return failure(
                RecordingErrorCode::CodecChanged,
                "Video codec changed during recording",
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    failure(RecordingErrorCode::CaptureTimeout, "Video capture stopped")
}

#[allow(clippy::too_many_arguments)]
async fn record(
    mux: &mux::Mux,
    video: &VideoStreamManager,
    frames: &mut EncodedVideoFrameReceiver,
    packets: &mut tokio::sync::mpsc::Receiver<Arc<OpusFrame>>,
    first_video: Arc<EncodedVideoFrame>,
    first_audio: Arc<OpusFrame>,
    resolution: crate::video::format::Resolution,
    stop: &CancellationToken,
    dir: &std::path::Path,
    status: &RwLock<RecordingStatus>,
    duration_seconds: &mut f64,
) -> anyhow::Result<()> {
    let origin = first_video.encoded_at;
    let audio_offset = first_audio.encoded_at.duration_since(origin);
    let mut video_time = Timeline {
        sequence: first_video.sequence,
        pts_ms: first_video.pts_ms,
    };
    let mut audio_sequence = first_audio.sequence;
    mux.push(
        false,
        &first_video.data,
        Duration::ZERO,
        first_video.duration,
        true,
    )?;
    mux.push(
        true,
        &first_audio.data,
        audio_offset,
        Duration::from_millis(first_audio.duration_ms.into()),
        true,
    )?;
    let mut last_video = Instant::now();
    let mut last_audio = last_video;
    let mut health = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            packet = packets.recv() => {
                let packet = packet.ok_or_else(|| failure(RecordingErrorCode::CaptureTimeout, "Audio capture stopped"))?;
                if packet.sequence != audio_sequence + 1 || packet.duration_ms != first_audio.duration_ms {
                    return Err(failure(RecordingErrorCode::PacketsLost, "Audio capture changed or recording packets were lost"));
                }
                audio_sequence = packet.sequence;
                let pts = audio_offset + Duration::from_millis((packet.sequence - first_audio.sequence) * u64::from(packet.duration_ms));
                mux.push(true, &packet.data, pts, Duration::from_millis(packet.duration_ms.into()), true)?;
                last_audio = Instant::now();
            }
            frame = frames.recv() => {
                let Some(frame) = frame else { return Err(closed_video_reason(video, first_video.codec).await) };
                if frame.codec != first_video.codec { return Err(failure(RecordingErrorCode::CodecChanged, "Video codec changed during recording")); }
                video_time.next(frame.sequence, frame.pts_ms)?;
                mux.push(false, &frame.data, Duration::from_millis((frame.pts_ms - first_video.pts_ms) as u64), frame.duration, frame.is_keyframe)?;
                *duration_seconds = (frame.pts_ms - first_video.pts_ms) as f64 / 1000.0;
                last_video = Instant::now();
            }
            _ = health.tick() => {
                let mut current = status.write().await;
                current.elapsed_seconds = origin.elapsed().as_secs();
                current.bytes = mux.bytes().map_err(|e| failure(RecordingErrorCode::StorageError, e))?;
                drop(current);
                storage::check_space(dir)?;
                mux.check()?;
                if last_audio.elapsed() > Duration::from_secs(3) || last_video.elapsed() > Duration::from_secs(3) {
                    return Err(failure(RecordingErrorCode::CaptureTimeout, "Audio or video capture timed out"));
                }
                let config = video.get_encoding_config().await.ok_or_else(|| failure(RecordingErrorCode::PipelineStopped, "Video pipeline stopped"))?;
                if config.output_codec != first_video.codec { return Err(failure(RecordingErrorCode::CodecChanged, "Video codec changed during recording")); }
                if config.resolution != resolution { return Err(failure(RecordingErrorCode::ResolutionChanged, "Video resolution changed during recording")); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_frames_and_pipeline_restarts_are_not_muxed_as_continuous_video() {
        let mut timeline = Timeline {
            sequence: 40,
            pts_ms: 5000,
        };
        timeline.next(41, 5033).unwrap();
        assert_eq!(
            error_code(&timeline.next(43, 5100).unwrap_err()),
            RecordingErrorCode::PacketsLost
        );
        assert!(timeline.next(0, 0).is_err());
        assert!(timeline.next(42, 5033).is_err());
    }

    #[test]
    fn typed_errors_preserve_messages_through_context() {
        let error = failure(RecordingErrorCode::CaptureTimeout, "Audio capture stopped");
        assert_eq!(error.to_string(), "Audio capture stopped");
        assert_eq!(
            error_code(&error.context("recording failed")),
            RecordingErrorCode::CaptureTimeout
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let error = storage::prepare(&file).unwrap_err();
        assert_eq!(
            error_code_or_storage(&error),
            RecordingErrorCode::StorageError
        );
        assert_eq!(
            error_code(&anyhow::anyhow!("unexpected")),
            RecordingErrorCode::Internal
        );
    }

    #[tokio::test]
    async fn canceled_stop_request_keeps_ownership_until_file_is_finalized() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Recorder::new(dir.path().into()));
        let (name, file) = storage::create(dir.path()).unwrap();
        drop(file);
        *recorder.status.write().await = RecordingStatus {
            recording: true,
            file: Some(name.clone()),
            ..Default::default()
        };
        assert!(recorder.open(&name).await.is_err());
        assert!(recorder.delete(&name).await.is_err());
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let (stopping, stopped) = tokio::sync::oneshot::channel();
        let (finalize, finalized) = tokio::sync::oneshot::channel();
        let status = recorder.status.clone();
        *recorder.session.lock().await = Some(Session {
            stop: cancel,
            task: tokio::spawn(async move {
                worker_cancel.cancelled().await;
                stopping.send(()).unwrap();
                finalized.await.unwrap();
                status.write().await.recording = false;
            }),
        });
        let owner = recorder.clone();
        let request = tokio::spawn(async move { owner.stop().await });
        stopped.await.unwrap();
        request.abort();
        let _ = request.await;
        assert!(recorder.session.lock().await.is_some());
        assert!(recorder.open(&name).await.is_err());
        finalize.send(()).unwrap();
        assert!(!recorder.stop().await.recording);
        assert!(recorder.open(&name).await.is_ok());
        recorder.delete(&name).await.unwrap();
        assert!(recorder.files().unwrap().is_empty());
    }
}
