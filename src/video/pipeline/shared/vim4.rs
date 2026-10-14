//! VIM4 keeps capture buffers in DMA memory until the hardware encoder consumes them.
use super::*;
use gst::prelude::*;
use gstreamer as gst;

struct Capture(gst::Pipeline);
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn description(config: &SharedVideoPipelineConfig, source: Resolution, fps: u32) -> Result<String> {
    let codec = match config.output_codec {
        VideoEncoderType::H264 => "h264",
        VideoEncoderType::H265 => "h265",
        _ => {
            return Err(AppError::VideoError(
                "VIM4 supports H.264 and H.265 hardware encoding".into(),
            ))
        }
    };
    let filter = config.output.filter(source, fps);
    Ok(format!("vfmcapsrc ! queue max-size-buffers=3 max-size-bytes=0 max-size-time=0 ! {filter}\
        amlvenc name=encoder bitrate={} gop={} framerate={} min-force-key-unit-interval=250000000 ! \
        video/x-{codec} ! {codec}parse config-interval=-1 ! \
        video/x-{codec},stream-format=byte-stream,alignment=au ! appsink name=frames sync=false max-buffers=2",
        config.bitrate_kbps().clamp(1000, 12000), config.bitrate_preset.gop_size(fps), fps))
}

impl SharedVideoPipeline {
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
        while self.running_flag.load(Ordering::Acquire) {
            let Some(source) = crate::platform::vim4::signal() else {
                self.notify_state(PipelineStateNotification::no_signal(
                    SignalStatus::NoSignal,
                    Some(250),
                ));
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            config.bitrate_preset = self.config.blocking_read().bitrate_preset;
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
        capture
            .0
            .set_state(gst::State::Playing)
            .map_err(|e| error(&e))?;
        let mut last_frame = Instant::now();
        let mut check = Instant::now();
        let mut rate_changes = 0;
        let mut frames = 0;
        let mut stats_time = Instant::now();
        while self.running_flag.load(Ordering::Acquire) {
            if self.keyframe_requested.swap(false, Ordering::AcqRel) {
                sink.send_event(
                    gstreamer_video::UpstreamForceKeyUnitEvent::builder()
                        .all_headers(true)
                        .build(),
                );
            }
            if check.elapsed() >= Duration::from_millis(250) {
                check = Instant::now();
                if self.config.blocking_read().bitrate_preset != config.bitrate_preset {
                    break;
                }
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
                return Err(AppError::VideoError(format!(
                    "Capture stopped: {:?}",
                    msg.view()
                )));
            }
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) else {
                if last_frame.elapsed() > Duration::from_secs(3) {
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
                        PixelFormat::Nv21,
                        fps,
                    ));
                    let mut current = self.config.blocking_write();
                    current.resolution = resolution;
                    current.fps = fps;
                }
            }
            let frame = Arc::new(EncodedVideoFrame {
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
            if stats_time.elapsed() >= Duration::from_secs(1) {
                self.stats.lock().current_fps = frames as f32 / stats_time.elapsed().as_secs_f32();
                frames = 0;
                stats_time = Instant::now();
            }
        }
        Ok(())
    }
}
