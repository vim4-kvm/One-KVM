//! VIM4 keeps capture buffers in DMA memory until the hardware encoder consumes them.
use super::*;
use crate::platform::vim4::VideoBackend;
use crate::video::codec::BitratePreset;
use gst::prelude::*;
use gstreamer as gst;

mod wave5;

struct Capture(gst::Pipeline);
impl Drop for Capture {
    fn drop(&mut self) {
        if VideoBackend::current() == VideoBackend::Wave5 {
            // Release a blocked appsrc bridge even if negotiation never finished.
            let _ = self.0.send_event(gst::event::FlushStart::new());
        }
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn description(config: &SharedVideoPipelineConfig, source: Resolution, fps: u32) -> Result<String> {
    if VideoBackend::current() == VideoBackend::Wave5 {
        return wave5::description(config, source, fps);
    }
    let codec = match config.output_codec {
        VideoEncoderType::H264 => "h264",
        VideoEncoderType::H265 => "h265",
        _ => {
            return Err(AppError::VideoError(
                "VIM4 supports H.264 and H.265 hardware encoding".into(),
            ))
        }
    };
    // Full I-frames stall encoding and delivery at 4K60 even when the average
    // frame rate stays at 60. Spread refresh across frames for both codecs;
    // H.264 uses 16-pixel MB rows, while HEVC uses larger CTU rows.
    // GOP 0 still emits the initial I-frame and honors force-key-unit events:
    // UU/WebRTC/RTSP request one on subscription; PLI/FIR recover lost references.
    let refresh_rows = if config.output_codec == VideoEncoderType::H265 {
        1
    } else {
        3
    };
    let filter = config.output.filter(source, fps);
    Ok(format!("vfmcapsrc ! {}queue max-size-buffers=3 max-size-bytes=0 max-size-time=0 ! {filter}\
        amlvenc name=encoder bitrate={} gop=0 framerate={} intra-refresh-rows={refresh_rows} min-force-key-unit-interval=250000000 ! \
        video/x-{codec} ! {codec}parse config-interval=-1 ! \
        video/x-{codec},stream-format=byte-stream,alignment=au ! appsink name=frames sync=false max-buffers=2",
        crate::video::snapshot::vim4::BRANCH, config.bitrate_kbps().clamp(1000, 60000), fps))
}

impl SharedVideoPipeline {
    /// Reconfigure the DMA pipeline without dropping its subscribers or peer connections.
    pub async fn set_vim4_encoding(
        &self,
        output: crate::platform::vim4::OutputSettings,
        preset: BitratePreset,
    ) -> Result<()> {
        let mut applied = self.vim4_applied.subscribe();
        let previous = {
            let mut config = self.config.write().await;
            let previous = (config.output, config.bitrate_preset);
            config.output = output;
            config.bitrate_preset = preset;
            previous
        };
        // A requested configuration is not evidence that GStreamer produced a frame.
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if *applied.borrow_and_update() == Some((output, preset)) {
                    return Ok(());
                }
                applied
                    .changed()
                    .await
                    .map_err(|_| AppError::VideoError("Video pipeline closed".into()))?;
            }
        })
        .await;
        if let Ok(result) = ready {
            return result;
        }
        let mut config = self.config.write().await;
        if (config.output, config.bitrate_preset) == (output, preset) {
            (config.output, config.bitrate_preset) = previous;
        }
        Err(AppError::VideoError(
            "Timed out waiting for the requested video output".into(),
        ))
    }

    pub(super) async fn start_vim4(self: &Arc<Self>) -> Result<()> {
        if self.lifecycle() != PipelineLifecycle::Stopped {
            return Ok(());
        }
        let config = self.config.read().await.clone();
        description(&config, config.resolution, config.fps.max(1))?;
        gst::init().map_err(|e| AppError::VideoError(e.to_string()))?;
        *self.parameter_sets.lock() = CachedH26xParameterSets::default();
        *self.bootstrap_frame.write() = None;
        self.running_flag.store(true, Ordering::Release);
        self.running.send_replace(true);
        self.encoder_done.send_replace(false);
        let pipeline = self.clone();
        let result = spawn_video_worker("vim4-video", move || {
            pipeline.run_vim4(config);
            // Publish completion only after all GStreamer handles have been released.
            pipeline.running_flag.store(false, Ordering::Release);
            pipeline.encoder_done.send_replace(true);
            pipeline.running.send_replace(false);
        });
        if let Err(e) = result {
            self.running_flag.store(false, Ordering::Release);
            self.encoder_done.send_replace(true);
            self.running.send_replace(false);
            return Err(AppError::VideoError(e.to_string()));
        }
        Ok(())
    }

    fn run_vim4(&self, mut config: SharedVideoPipelineConfig) {
        let mut idle_since = None;
        while self.running_flag.load(Ordering::Acquire) {
            if self.snapshot_idle_expired(&mut idle_since) {
                break;
            }
            let Some(source) = crate::platform::vim4::signal() else {
                self.notify_state(PipelineStateNotification::no_signal(
                    SignalStatus::NoSignal,
                    Some(250),
                ));
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            {
                let current = self.config.blocking_read();
                config.bitrate_preset = current.bitrate_preset;
                config.output = current.output;
            }
            match self.capture_vim4(&config, source) {
                Ok(()) => {}
                Err(e) => {
                    warn!("VIM4 video: {e}");
                    self.notify_state(PipelineStateNotification::no_signal(
                        SignalStatus::NoSignal,
                        Some(1000),
                    ));
                    // Bounded sleep lets application switches release hardware promptly.
                    for _ in 0..10 {
                        if !self.running_flag.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    }

    fn capture_vim4(
        &self,
        config: &SharedVideoPipelineConfig,
        source: (Resolution, u32),
    ) -> Result<()> {
        let fps = config.output.frame_rate(source.1, 60);
        let error = |e: &dyn std::fmt::Display| AppError::VideoError(e.to_string());
        let element =
            gst::parse::launch(&description(config, source.0, fps)?).map_err(|e| error(&e))?;
        let capture = Capture(
            element
                .downcast::<gst::Pipeline>()
                .map_err(|_| AppError::VideoError("Expected GStreamer pipeline".into()))?,
        );
        let sink = capture
            .0
            .by_name("frames")
            .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
            .ok_or_else(|| AppError::VideoError("Missing appsink".into()))?;
        let bus = capture
            .0
            .bus()
            .ok_or_else(|| AppError::VideoError("Missing bus".into()))?;
        let progress = if VideoBackend::current() == VideoBackend::Wave5 {
            Some(wave5::configure(&capture.0, config, source.0)?)
        } else {
            None
        };
        capture
            .0
            .set_state(gst::State::Playing)
            .map_err(|e| error(&e))?;
        let mut last_frame = Instant::now();
        let _snapshot = self
            .snapshot_tap
            .attach(&capture.0)
            .map_err(|e| error(&e))?;
        let mut check = Instant::now();
        let mut rate_changes = 0;
        let mut frames = 0;
        let mut stats_time = Instant::now();
        let mut idle_since = None;
        let mut total_frames = 0u64;
        let mut progress_time = Instant::now();
        info!(?source, codec = ?config.output_codec, "VIM4 capture started");
        while self.running_flag.load(Ordering::Acquire) {
            if progress_time.elapsed() >= Duration::from_secs(5) {
                info!(raw = ?progress.as_ref().map(|p| p.counts()), encoded = total_frames,
                    last_frame_ms = last_frame.elapsed().as_millis() as u64,
                    pipeline_state = ?capture.0.state(gst::ClockTime::ZERO), "VIM4 capture progress");
                progress_time = Instant::now();
            }
            if self.snapshot_idle_expired(&mut idle_since) {
                self.running_flag.store(false, Ordering::Release);
                break;
            }
            if self.keyframe_requested.swap(false, Ordering::AcqRel) {
                sink.send_event(
                    gstreamer_video::UpstreamForceKeyUnitEvent::builder()
                        .all_headers(true)
                        .build(),
                );
            }
            if check.elapsed() >= Duration::from_millis(250) {
                check = Instant::now();
                let current = self.config.blocking_read();
                if current.bitrate_preset != config.bitrate_preset
                    || current.output != config.output
                {
                    break;
                }
                drop(current);
                let signal = crate::platform::vim4::signal();
                rate_changes = if signal != Some(source) {
                    rate_changes + 1
                } else {
                    0
                };
                if rate_changes >= 3 {
                    break;
                }
            }
            if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos]) {
                warn!(raw = ?progress.as_ref().map(|p| p.counts()), encoded = total_frames,
                    last_frame_ms = last_frame.elapsed().as_millis() as u64,
                    signal = ?crate::platform::vim4::signal(),
                    pipeline_state = ?capture.0.state(gst::ClockTime::ZERO), "VIM4 capture bus stop");
                return Err(AppError::VideoError(format!(
                    "Capture stopped: {:?}",
                    msg.view()
                )));
            }
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) else {
                if last_frame.elapsed() > Duration::from_secs(3) {
                    warn!(raw = ?progress.as_ref().map(|p| p.counts()), encoded = total_frames,
                        signal = ?crate::platform::vim4::signal(), "VIM4 capture timeout");
                    return Err(AppError::VideoError("HDMI capture stalled".into()));
                }
                continue;
            };
            last_frame = Instant::now();
            let buffer = sample
                .buffer()
                .ok_or_else(|| AppError::VideoError("Missing video buffer".into()))?;
            let map = buffer.map_readable().map_err(|e| error(&e))?;
            let data = Bytes::copy_from_slice(map.as_slice());
            let (data, is_keyframe) = self.inspect_and_parameterize_packet(
                config.output_codec,
                data,
                !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
            );
            if let Some(caps) = sample.caps().and_then(|caps| caps.structure(0)) {
                if let (Ok(width), Ok(height)) =
                    (caps.get::<i32>("width"), caps.get::<i32>("height"))
                {
                    let resolution = Resolution::new(width as u32, height as u32);
                    self.notify_state(PipelineStateNotification::streaming(
                        resolution,
                        VideoBackend::current().input_format(),
                        fps,
                    ));
                    let mut current = self.config.blocking_write();
                    current.resolution = resolution;
                    current.fps = fps;
                }
            }
            self.vim4_applied.send_if_modified(|applied| {
                let next = Some((config.output, config.bitrate_preset));
                if *applied == next {
                    false
                } else {
                    *applied = next;
                    true
                }
            });
            let frame = Arc::new(EncodedVideoFrame {
                encoded_at: last_frame,
                data,
                is_keyframe,
                pts_ms: self.pts_ms(),
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
                duration: Duration::from_nanos(1_000_000_000 / u64::from(fps)),
                codec: config.output_codec,
            });
            // Continue consuming the encoder while viewers are slow. Delivery
            // marks skipped subscribers for recovery at the next IDR.
            let reservations = self.reserve_outputs().unwrap_or_default();
            self.broadcast_encoded_batch(vec![frame].into(), reservations);
            frames += 1;
            total_frames += 1;
            if stats_time.elapsed() >= Duration::from_secs(1) {
                self.stats.lock().current_fps = frames as f32 / stats_time.elapsed().as_secs_f32();
                frames = 0;
                stats_time = Instant::now();
            }
        }
        Ok(())
    }

    fn snapshot_idle_expired(&self, since: &mut Option<Instant>) -> bool {
        if self.subscriber_count() != 0 || self.snapshot_tap.active() != 0 {
            *since = None;
            return false;
        }
        since.get_or_insert_with(Instant::now).elapsed()
            >= Duration::from_secs(AUTO_STOP_GRACE_PERIOD_SECS)
    }
}
