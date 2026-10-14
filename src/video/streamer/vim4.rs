use super::*;
use gst::prelude::*;
use gstreamer as gst;

impl Streamer {
    pub(super) fn run_vim4_mjpeg(&self, config: StreamerConfig) {
        let result = self.capture_vim4_mjpeg(config);
        if let Err(e) = result {
            warn!("VIM4 MJPEG: {e}");
            *self.state.blocking_write() = StreamerState::Error;
        }
        self.current_fps.store(0, Ordering::Relaxed);
        self.direct_active.store(false, Ordering::SeqCst);
        self.mjpeg_handler.set_offline();
    }

    fn capture_vim4_mjpeg(&self, config: StreamerConfig) -> anyhow::Result<()> {
        gst::init()?;
        // Software JPEG is intentionally bounded; hardware 4K60 is WebRTC.
        let element = gst::parse::launch(&format!(
            "vfmcapsrc ! videorate drop-only=true ! video/x-raw,framerate=10/1 ! \
             videoconvert ! jpegenc quality={} ! appsink name=frames sync=false max-buffers=1 drop=true",
            config.jpeg_quality.clamp(1, 100)))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Invalid pipeline"))?;
        let result = (|| -> anyhow::Result<()> {
            let sink = pipeline
                .by_name("frames")
                .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
                .ok_or_else(|| anyhow::anyhow!("Missing appsink"))?;
            pipeline.set_state(gst::State::Playing)?;
            let mut sequence = 0;
            let mut last_frame = std::time::Instant::now();
            let mut idle_since = None;
            while !self.direct_stop.load(Ordering::SeqCst) {
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
                self.current_fps.store(10, Ordering::Relaxed);
                self.mjpeg_handler.set_online();
                self.mjpeg_handler.update_frame(frame);
            }
            Ok(())
        })();
        let _ = pipeline.set_state(gst::State::Null);
        result
    }
}
