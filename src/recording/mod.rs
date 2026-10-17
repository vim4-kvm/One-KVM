//! One device-owned recording, independent of browser and playback audio state.
pub mod locations;
mod mux;
mod storage;
use locations::{bounded, Availability, Location, Locations, Unavailable};

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
    StorageUnavailable,
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

#[cfg(test)]
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
    pub location: Location,
    pub storage_reason: Option<Unavailable>,
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
    locations: Arc<Locations>,
    config: Option<crate::config::ConfigStore>,
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
            locations: Arc::new(Locations::new(dir)),
            config: None,
            session: Mutex::new(None),
            status: Arc::new(RwLock::new(RecordingStatus::default())),
        }
    }

    pub fn with_config(dir: PathBuf, config: crate::config::ConfigStore) -> Self {
        let mut recorder = Self::new(dir);
        recorder.config = Some(config);
        recorder
    }

    fn configuration(&self) -> crate::config::RecordingConfig {
        self.config
            .as_ref()
            .map(|c| c.get().recording.clone())
            .unwrap_or_default()
    }

    fn selected(&self) -> Location {
        let config = self.configuration();
        self.locations
            .discover(&config)
            .into_iter()
            .find(|l| l.location.id == config.location)
            .map(|l| l.location)
            .unwrap_or(Location {
                id: config.location.clone(),
                name: config.location.clone(),
                folder: config
                    .folders
                    .get(&config.location)
                    .cloned()
                    .unwrap_or_else(|| "One-KVM".into()),
            })
    }

    async fn probe(
        &self,
        location: Location,
    ) -> std::result::Result<storage::StorageStatus, Unavailable> {
        self.find_location(&location.id)
            .map_err(|_| Unavailable::NotMounted)?;
        let locations = self.locations.clone();
        bounded(
            locations.gate(&format!("probe:{}", location.id)),
            move || {
                locations
                    .directory(&location, true)?
                    .probe()
                    .map_err(|_| Unavailable::NotWritable)
            },
        )
        .await?
    }

    pub async fn locations(&self) -> Vec<Availability> {
        let entries = self.locations.discover(&self.configuration());
        futures::future::join_all(entries.into_iter().map(|mut entry| async move {
            match self.probe(entry.location.clone()).await {
                Ok(space) => {
                    entry.available = true;
                    entry.free_bytes = Some(space.free_bytes);
                }
                Err(reason) => entry.reason = Some(reason),
            }
            entry
        }))
        .await
    }

    pub async fn set_location(&self, id: String, folder: Option<String>) -> Result<()> {
        let session = self.session.lock().await;
        if session.as_ref().is_some_and(|s| !s.task.is_finished()) {
            return Err(AppError::Conflict(
                "Stop recording before changing location".into(),
            ));
        }
        let mut target = self
            .locations
            .discover(&self.configuration())
            .into_iter()
            .find(|l| l.location.id == id)
            .ok_or_else(|| recording_error("not_mounted"))?
            .location;
        if id != "local" {
            target.folder = folder.unwrap_or_else(|| "One-KVM".into());
            locations::validate_folder(&target.folder).map_err(recording_error)?;
        }
        self.probe(target.clone()).await.map_err(recording_error)?;
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| recording_error("Configuration unavailable"))?;
        config
            .update(|c| {
                c.recording.location = id.clone();
                if id != "local" {
                    c.recording.folders.insert(id, target.folder);
                }
            })
            .await
    }

    pub async fn status(&self) -> RecordingStatus {
        let mut status = self.status.read().await.clone();
        status.location = self.selected();
        match self.probe(status.location.clone()).await {
            Ok(space) => {
                status.storage = Some(space);
                status.storage_reason = None;
            }
            Err(reason) => {
                status.storage = None;
                status.storage_reason = Some(reason);
                status.blocker = Some(RecordingErrorCode::StorageUnavailable);
            }
        }
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
        let location = self.selected();
        let directory = self.prepare_destination(location.clone()).await?;
        let check_dir = directory.clone();
        bounded(self.locations.gate(&location.id), move || {
            storage::check_space(&check_dir.path())
        })
        .await
        .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))??;
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
        let (name, file) = {
            let directory = directory.clone();
            bounded(self.locations.gate(&location.id), move || {
                storage::create(&directory.path())
            })
            .await
            .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))?
            .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))?
        };
        let mux = match mux::Mux::new(file, config.output_codec == VideoEncoderType::H265) {
            Ok(mux) => mux,
            Err(error) => {
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
            let directory = directory.clone();
            let metadata_copy = metadata.clone();
            let name_copy = name.clone();
            bounded(self.locations.gate(&location.id), move || {
                storage::write_metadata(&directory.path(), &name_copy, &metadata_copy)
            })
            .await
            .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))??;
            Ok::<_, anyhow::Error>((resolution, metadata))
        }
        .await;
        let (resolution, mut metadata) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                drop(mux);
                return Err(error);
            }
        };
        *self.status.write().await = RecordingStatus {
            recording: true,
            location: location.clone(),
            file: Some(name.clone()),
            started_at: metadata.started_at,
            codec: metadata.codec,
            ..Default::default()
        };
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let status = self.status.clone();
        let gate = self.locations.gate(&location.id);
        let is_nas = location.id != "local";
        let locations = self.locations.clone();
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
                &status,
                &mut duration_seconds,
                gate.clone(),
                directory.clone(),
                locations,
                location,
            )
            .await;
            let finalized = mux.finish().await;
            drop(mux);
            let mut code = result.as_ref().err().map(error_code);
            if is_nas
                && matches!(
                    code,
                    Some(
                        RecordingErrorCode::StorageError
                            | RecordingErrorCode::StorageUnavailable
                            | RecordingErrorCode::PipelineStopped
                    )
                )
            {
                code = Some(RecordingErrorCode::StorageUnavailable);
            }
            let mut error = result.err().map(|error| error.to_string());
            let mut final_name = name.clone();
            if let Err(failure) = finalized {
                code = Some(if is_nas {
                    RecordingErrorCode::StorageUnavailable
                } else {
                    RecordingErrorCode::SaveFailed
                });
                error = Some(format!(
                    "{}{}",
                    error.map(|e| format!("{e}; ")).unwrap_or_default(),
                    failure
                ));
            } else {
                let complete = name.trim_end_matches(".part").to_owned();
                let directory = directory.clone();
                let name_copy = name.clone();
                let complete_copy = complete.clone();
                match bounded(gate.clone(), move || {
                    std::fs::rename(
                        directory.path().join(name_copy),
                        directory.path().join(complete_copy),
                    )
                })
                .await
                .and_then(|r| r.map_err(|_| Unavailable::NotWritable))
                {
                    Ok(()) => final_name = complete,
                    Err(failure) => {
                        code = Some(if is_nas {
                            RecordingErrorCode::StorageUnavailable
                        } else {
                            RecordingErrorCode::SaveFailed
                        });
                        error = Some(format!("Could not save recording: {failure}"));
                    }
                }
            }
            metadata.duration_seconds = Some(duration_seconds);
            metadata.stop_reason = code;
            let name_copy = final_name.clone();
            if let Err(failure) = bounded(gate, move || {
                storage::write_metadata(&directory.path(), &name_copy, &metadata)
            })
            .await
            .and_then(|r| r.map_err(|_| Unavailable::NotWritable))
            {
                code = Some(if is_nas {
                    RecordingErrorCode::StorageUnavailable
                } else {
                    RecordingErrorCode::SaveFailed
                });
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
        });
        Ok(Session { stop, task })
    }

    pub async fn stop(&self) -> RecordingStatus {
        match tokio::time::timeout(Duration::from_secs(15), self.stop_inner()).await {
            Ok(status) => status,
            Err(_) => {
                // Includes waiting for an in-flight start. Shutdown must remain
                // below the service's 20-second deadline even on stuck NAS I/O.
                let mut status = self.status.read().await.clone();
                status.error = Some("Recording storage cleanup timed out".into());
                status.error_code = Some(RecordingErrorCode::StorageUnavailable);
                status
            }
        }
    }

    async fn stop_inner(&self) -> RecordingStatus {
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

    async fn prepare_destination(
        &self,
        target: Location,
    ) -> anyhow::Result<Arc<locations::Directory>> {
        self.find_location(&target.id).map_err(|_| {
            failure(
                RecordingErrorCode::StorageUnavailable,
                Unavailable::NotMounted,
            )
        })?;
        let locations = self.locations.clone();
        let directory = bounded(locations.gate(&target.id), move || {
            let directory = locations.directory(&target, true)?;
            directory.probe().map_err(|_| Unavailable::NotWritable)?;
            Ok::<_, Unavailable>(directory)
        })
        .await
        .and_then(|r| r)
        .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))?;
        Ok(Arc::new(directory))
    }

    pub async fn files(&self) -> (Vec<RecordingFile>, Vec<Availability>) {
        let entries = self.locations.discover(&self.configuration());
        let results = futures::future::join_all(entries.into_iter().map(|mut entry| async move {
            let locations = self.locations.clone();
            let location = entry.location.clone();
            let result = bounded(
                locations.gate(&format!("files:{}", location.id)),
                move || {
                    let directory = locations.directory(&location, true)?;
                    let mut files =
                        storage::list(&directory.path()).map_err(|_| Unavailable::NotWritable)?;
                    for file in &mut files {
                        file.location = location.clone();
                    }
                    Ok::<_, Unavailable>(files)
                },
            )
            .await
            .and_then(|r| r);
            match result {
                Ok(files) => (files, None),
                Err(reason) => {
                    entry.reason = Some(reason);
                    (Vec::new(), Some(entry))
                }
            }
        }))
        .await;
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        for (items, missing) in results {
            files.extend(items);
            unavailable.extend(missing);
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.started_at.unwrap_or(f.modified)));
        (files, unavailable)
    }

    fn find_location(&self, id: &str) -> Result<Location> {
        self.locations
            .discover(&self.configuration())
            .into_iter()
            .find(|l| l.location.id == id)
            .map(|l| l.location)
            .ok_or_else(|| recording_error("not_mounted"))
    }

    pub fn download_gate(&self, id: &str) -> Arc<tokio::sync::Semaphore> {
        self.locations.gate(&format!("download:{id}"))
    }

    pub async fn open_at(&self, id: &str, name: &str) -> Result<(std::fs::File, u64, String)> {
        let _session = self.session.lock().await;
        self.ensure_inactive(id, name).await?;
        let location = self.find_location(id)?;
        let locations = self.locations.clone();
        let name = name.to_owned();
        bounded(self.download_gate(id), move || -> anyhow::Result<_> {
            let directory = locations.directory(&location, false)?;
            let path = directory.path();
            let file = storage::open(&path, &name)?;
            let stat = file.metadata()?;
            let modified = stat
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let download =
                storage::download_name(storage::read_metadata(&path, &name).started_at, modified);
            Ok((file, stat.len(), download))
        })
        .await
        .map_err(recording_error)?
        .map_err(recording_error)
    }

    pub async fn delete_at(&self, id: &str, name: &str) -> Result<()> {
        let _session = self.session.lock().await;
        self.ensure_inactive(id, name).await?;
        let location = self.find_location(id)?;
        let locations = self.locations.clone();
        let name = name.to_owned();
        bounded(
            locations.gate(&format!("delete:{id}")),
            move || -> anyhow::Result<()> {
                let directory = locations.directory(&location, false)?;
                storage::delete(&directory.path(), &name)
            },
        )
        .await
        .map_err(recording_error)?
        .map_err(recording_error)
    }

    async fn ensure_inactive(&self, id: &str, name: &str) -> Result<()> {
        let status = self.status.read().await;
        if status.recording
            && (status.location.id == id || status.location.id.is_empty())
            && status.file.as_deref() == Some(name)
        {
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
    status: &RwLock<RecordingStatus>,
    duration_seconds: &mut f64,
    gate: Arc<tokio::sync::Semaphore>,
    directory: Arc<locations::Directory>,
    locations: Arc<Locations>,
    location: Location,
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
                current.bytes = mux.bytes();
                drop(current);
                let directory = directory.clone();
                let locations = locations.clone();
                let location = location.clone();
                bounded(gate.clone(), move || check_active_storage(&locations, &location, &directory)).await
                    .map_err(|e| failure(RecordingErrorCode::StorageUnavailable, e))??;
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

fn check_active_storage(
    locations: &Locations,
    location: &Location,
    directory: &locations::Directory,
) -> anyhow::Result<()> {
    let current = locations
        .directory(location, false)
        .map_err(|reason| failure(RecordingErrorCode::StorageUnavailable, reason))?;
    if !directory.same_mount(&current) {
        return Err(failure(
            RecordingErrorCode::StorageUnavailable,
            "Recording mount was replaced",
        ));
    }
    storage::check_space(&directory.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_recording_stops_when_mount_is_removed_even_with_live_directory_fd() {
        let root = tempfile::tempdir().unwrap();
        let locations = locations::tests::fixture(root.path());
        let target = Location {
            id: "nas:test".into(),
            name: "Test".into(),
            folder: "One-KVM".into(),
        };
        let directory = locations.directory(&target, true).unwrap();
        assert!(check_active_storage(&locations, &target, &directory).is_ok());
        std::fs::write(&locations.mountinfo, "").unwrap();
        assert_eq!(
            error_code(&check_active_storage(&locations, &target, &directory).unwrap_err()),
            RecordingErrorCode::StorageUnavailable
        );
    }

    #[tokio::test]
    async fn unavailable_destination_rejects_start_preflight_without_local_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::new(root.path().join("local"));
        let locations = locations::tests::fixture(root.path());
        std::fs::write(&locations.mountinfo, "").unwrap();
        recorder.locations = Arc::new(locations);
        let target = Location {
            id: "nas:test".into(),
            name: "Test".into(),
            folder: "One-KVM".into(),
        };
        let error = recorder.prepare_destination(target).await.err().unwrap();
        assert_eq!(error_code(&error), RecordingErrorCode::StorageUnavailable);
        assert!(!root.path().join("local").exists());
        assert!(!root.path().join("mounts/test/One-KVM").exists());
    }

    #[tokio::test]
    async fn library_merges_locations_and_targets_same_name_independently() {
        let root = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::new(root.path().join("local"));
        recorder.locations = Arc::new(locations::tests::fixture(root.path()));
        let local = recorder.find_location("local").unwrap();
        let nas = recorder.find_location("nas:test").unwrap();
        let local_dir = recorder.locations.directory(&local, true).unwrap();
        let nas_dir = recorder.locations.directory(&nas, true).unwrap();
        let (name, local_file) = storage::create(&local_dir.path()).unwrap();
        std::fs::write(nas_dir.path().join(&name), b"nas").unwrap();
        local_file
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let (files, missing) = recorder.files().await;
        assert!(missing.is_empty());
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].location.id, "nas:test");
        let gate = recorder.locations.gate("files:nas:test");
        let stalled = gate.acquire().await.unwrap();
        let (available, missing) = recorder.files().await;
        assert_eq!(available.len(), 1);
        assert_eq!(missing[0].reason, Some(Unavailable::Unresponsive));
        drop(stalled);
        assert_eq!(recorder.open_at("nas:test", &name).await.unwrap().1, 3);
        recorder.delete_at("nas:test", &name).await.unwrap();
        assert!(recorder.open_at("local", &name).await.is_ok());
        assert!(recorder.open_at("nas:test", &name).await.is_err());
        std::fs::write(&recorder.locations.mountinfo, "").unwrap();
        let (files, missing) = recorder.files().await;
        assert_eq!(files.len(), 1);
        assert_eq!(missing[0].reason, Some(Unavailable::NotMounted));
    }

    #[tokio::test]
    async fn recording_config_migrates_and_persists_selected_folders() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::db::DatabasePool::new(&root.path().join("config.db"))
            .await
            .unwrap();
        db.init_schema().await.unwrap();
        let mut old = serde_json::to_value(crate::config::AppConfig::default()).unwrap();
        old.as_object_mut().unwrap().remove("recording");
        sqlx::query("INSERT INTO config (key, value) VALUES ('app_config', ?)")
            .bind(old.to_string())
            .execute(db.pool())
            .await
            .unwrap();
        let config = crate::config::ConfigStore::new(db.clone_pool());
        config.load().await.unwrap();
        assert_eq!(config.get().recording.location, "local");
        let mut recorder = Recorder::with_config(root.path().join("local"), config.clone());
        recorder.locations = Arc::new(locations::tests::fixture(root.path()));
        recorder
            .set_location("nas:test".into(), Some("nested/folder".into()))
            .await
            .unwrap();
        recorder.set_location("local".into(), None).await.unwrap();
        let stop = CancellationToken::new();
        let wait = stop.clone();
        *recorder.session.lock().await = Some(Session {
            stop,
            task: tokio::spawn(async move { wait.cancelled().await }),
        });
        assert!(matches!(
            recorder.set_location("nas:test".into(), None).await,
            Err(AppError::Conflict(_))
        ));
        recorder.stop().await;
        config.load().await.unwrap();
        assert_eq!(config.get().recording.location, "local");
        assert_eq!(config.get().recording.folders["nas:test"], "nested/folder");
        assert!(recorder
            .set_location("nas:test".into(), Some("../escape".into()))
            .await
            .is_err());
    }
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
        assert!(recorder.open_at("local", &name).await.is_err());
        assert!(recorder.delete_at("local", &name).await.is_err());
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
        assert!(recorder.open_at("local", &name).await.is_err());
        finalize.send(()).unwrap();
        assert!(!recorder.stop().await.recording);
        assert!(recorder.open_at("local", &name).await.is_ok());
        recorder.delete_at("local", &name).await.unwrap();
        assert!(recorder.files().await.0.is_empty());
    }
}
