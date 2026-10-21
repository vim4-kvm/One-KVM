use super::*;
use gst::prelude::*;
use gstreamer as gst;

impl Streamer {
    pub(super) fn run_vim4_mjpeg(&self, config: StreamerConfig) {
        while !self.direct_stop.load(Ordering::SeqCst) {
            match self.capture_vim4_mjpeg(&config) {
                Ok(false) => break,
                Ok(true) => continue,
                Err(e) => {
                    warn!("VIM4 MJPEG: {e}");
                    *self.state.blocking_write() = StreamerState::Error;
                    self.mjpeg_handler.set_offline();
                    if self.mjpeg_handler.client_count() == 0 {
                        break;
                    }
                    for _ in 0..10 {
                        if self.direct_stop.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
        self.current_fps.store(0, Ordering::Relaxed);
        self.direct_active.store(false, Ordering::SeqCst);
        self.mjpeg_handler.set_offline();
    }

    // true requests a new pipeline after source timings change; false releases an idle stream.
    fn capture_vim4_mjpeg(&self, config: &StreamerConfig) -> anyhow::Result<bool> {
        gst::init()?;
        let source = crate::platform::vim4::signal().unwrap_or((config.resolution, config.fps));
        let wave5 = crate::platform::vim4::VideoBackend::current()
            == crate::platform::vim4::VideoBackend::Wave5;
        let hardware_jpeg = gst::ElementFactory::find("vim4jpegenc").is_some()
            && std::path::Path::new("/dev/jpegenc").exists();
        if wave5 && !hardware_jpeg {
            anyhow::bail!("The T7 hardware JPEG driver or vim4jpegenc plugin is unavailable");
        }
        let fps = config
            .output
            .frame_rate(source.1, if hardware_jpeg { 60 } else { 10 });
        let (capture, filter) = if wave5 {
            let output =
                crate::platform::vim4::video_backend::output_resolution(config.output, source.0);
            (format!("v4l2src name=source io-mode=dmabuf ! video/x-raw,format=NV12,width={},height={},framerate=60/1 ! ",
                output.width, output.height),
             format!("videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! "))
        } else {
            (
                "vfmcapsrc ! ".to_owned(),
                config.output.filter(source.0, fps),
            )
        };
        let encoder = if hardware_jpeg {
            "vim4jpegenc"
        } else {
            "videoconvert ! jpegenc"
        };
        let element = gst::parse::launch(&format!(
            "{capture}{}{filter}\
             queue max-size-buffers=1 max-size-bytes=0 max-size-time=0 leaky=downstream ! \
             {encoder} quality={} ! appsink name=frames sync=false max-buffers=1 drop=true enable-last-sample=false",
            crate::video::snapshot::vim4::BRANCH,
            config.jpeg_quality.clamp(1, 100)
        ))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Invalid pipeline"))?;
        if wave5 {
            let device = crate::platform::vim4::video_backend::wave5_device("t7-vdin-test")?;
            pipeline
                .by_name("source")
                .ok_or_else(|| anyhow::anyhow!("Missing capture source"))?
                .set_property("device", device.to_string_lossy().as_ref());
        }
        let result = (|| -> anyhow::Result<bool> {
            let sink = pipeline
                .by_name("frames")
                .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
                .ok_or_else(|| anyhow::anyhow!("Missing appsink"))?;
            pipeline.set_state(gst::State::Playing)?;
            let _snapshot = self.snapshot_tap.attach(&pipeline)?;
            let mut sequence = 0;
            let mut last_frame = std::time::Instant::now();
            let mut idle_since = None;
            let mut check = std::time::Instant::now();
            let mut source_changes = 0;
            let mut fps_since = std::time::Instant::now();
            let mut fps_frames = 0u32;
            while !self.direct_stop.load(Ordering::SeqCst) {
                if check.elapsed() >= Duration::from_millis(250) {
                    check = std::time::Instant::now();
                    source_changes = if crate::platform::vim4::signal() != Some(source) {
                        source_changes + 1
                    } else {
                        0
                    };
                    if source_changes >= 3 {
                        // Recompute size/rate limits: a smaller new input must never be upscaled.
                        return Ok(true);
                    }
                }
                if self.mjpeg_handler.client_count() == 0 {
                    let since = idle_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed().as_secs() >= 5 {
                        *self.state.blocking_write() = StreamerState::Ready;
                        break;
                    }
                } else {
                    idle_since = None;
                }
                let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) else {
                    if last_frame.elapsed() > Duration::from_secs(2) {
                        *self.state.blocking_write() = StreamerState::NoSignal;
                    }
                    if let Some(msg) = pipeline.bus().and_then(|bus| {
                        bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
                    }) {
                        anyhow::bail!("Capture stopped: {:?}", msg.view());
                    }
                    continue;
                };
                last_frame = std::time::Instant::now();
                let buffer = sample
                    .buffer()
                    .ok_or_else(|| anyhow::anyhow!("Missing buffer"))?;
                let map = buffer.map_readable()?;
                let caps = sample
                    .caps()
                    .and_then(|c| c.structure(0))
                    .ok_or_else(|| anyhow::anyhow!("Missing JPEG caps"))?;
                let resolution = Resolution::new(
                    caps.get::<i32>("width")? as u32,
                    caps.get::<i32>("height")? as u32,
                );
                let frame = VideoFrame::new(
                    bytes::Bytes::copy_from_slice(map.as_slice()),
                    resolution,
                    PixelFormat::Mjpeg,
                    0,
                    sequence,
                );
                sequence += 1;
                *self.state.blocking_write() = StreamerState::Streaming;
                fps_frames += 1;
                let elapsed = fps_since.elapsed().as_secs_f64();
                if elapsed >= 1.0 {
                    self.current_fps.store(
                        (f64::from(fps_frames) / elapsed * 100.0).round() as u32,
                        Ordering::Relaxed,
                    );
                    fps_since = std::time::Instant::now();
                    fps_frames = 0;
                }
                self.mjpeg_handler.set_online();
                self.mjpeg_handler.update_frame(frame);
            }
            Ok(false)
        })();
        let _ = pipeline.set_state(gst::State::Null);
        result
    }
}
