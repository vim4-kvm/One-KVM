use super::*;
use crate::platform::vim4::VideoBackend;
use gstreamer::{self as gst, prelude::*};
use gstreamer_video::VideoFrameExt;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{oneshot, Notify};

// The valve precedes the queue: while idle even the queue cannot retain a capture DMA fd.
// async=false avoids waiting for preroll from the closed branch; last-sample is disabled.
pub(crate) const BRANCH: &str = "tee name=snapshot_tee snapshot_tee. ! valve name=snapshot_valve drop=true ! queue max-size-buffers=1 max-size-bytes=0 max-size-time=0 leaky=downstream ! appsink name=snapshot_raw sync=false async=false max-buffers=1 drop=true enable-last-sample=false snapshot_tee. ! ";

type Reply = oneshot::Sender<Result<gst::Sample, SnapshotError>>;
struct Pending {
    after: gst::ClockTime,
    reply: Reply,
}
struct Attached {
    pipeline: gst::glib::WeakRef<gst::Pipeline>,
    valve: gst::Element,
}
#[derive(Default)]
struct State {
    attached: Option<Attached>,
    pending: Option<Pending>,
}

#[derive(Default)]
pub(crate) struct RawTap {
    state: Mutex<State>,
    changed: Notify,
    leases: AtomicUsize,
}

impl RawTap {
    pub(crate) fn active(&self) -> usize {
        self.leases.load(Ordering::Acquire)
    }
    pub(crate) fn lease(self: &Arc<Self>) -> TapLease {
        self.leases.fetch_add(1, Ordering::AcqRel);
        TapLease(self.clone())
    }

    pub(crate) fn attach(self: &Arc<Self>, pipeline: &gst::Pipeline) -> anyhow::Result<Attachment> {
        let valve = pipeline
            .by_name("snapshot_valve")
            .ok_or_else(|| anyhow::anyhow!("Missing snapshot valve"))?;
        let sink = pipeline
            .by_name("snapshot_raw")
            .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
            .ok_or_else(|| anyhow::anyhow!("Missing snapshot sink"))?;
        let weak = Arc::downgrade(self);
        sink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(tap) = weak.upgrade() {
                        let mut state = tap.state.lock();
                        let running = sample
                            .segment()
                            .and_then(|s| s.downcast_ref::<gst::ClockTime>())
                            .and_then(|s| {
                                sample
                                    .buffer()
                                    .and_then(|b| b.pts())
                                    .and_then(|pts| s.to_running_time(pts))
                            });
                        if state
                            .pending
                            .as_ref()
                            .is_some_and(|p| is_fresh(running, p.after))
                        {
                            if let Some(attached) = &state.attached {
                                attached.valve.set_property("drop", true);
                            }
                            if let Some(pending) = state.pending.take() {
                                let _ = pending.reply.send(Ok(sample));
                            }
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        self.state.lock().attached = Some(Attached {
            pipeline: pipeline.downgrade(),
            valve,
        });
        self.changed.notify_waiters();
        Ok(Attachment(self.clone()))
    }

    pub(crate) async fn sample(self: &Arc<Self>) -> Result<gst::Sample, SnapshotError> {
        let receiver = loop {
            let changed = self.changed.notified();
            {
                let mut state = self.state.lock();
                if let Some(attached) = &state.attached {
                    let after = attached
                        .pipeline
                        .upgrade()
                        .and_then(|p| p.current_running_time())
                        .ok_or_else(|| SnapshotError::unavailable("capture clock stopped"))?;
                    if state.pending.is_some() {
                        return Err(SnapshotError::Busy);
                    }
                    let (reply, receiver) = oneshot::channel();
                    state.pending = Some(Pending { after, reply });
                    state
                        .attached
                        .as_ref()
                        .unwrap()
                        .valve
                        .set_property("drop", false);
                    break receiver;
                }
            }
            changed.await;
        };
        let _request = Request(self.clone());
        receiver.await.map_err(SnapshotError::unavailable)?
    }
}

fn is_fresh(running: Option<gst::ClockTime>, after: gst::ClockTime) -> bool {
    running.is_some_and(|pts| pts > after)
}

struct Request(Arc<RawTap>);
impl Drop for Request {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        if let Some(attached) = &state.attached {
            attached.valve.set_property("drop", true);
        }
        state.pending = None;
    }
}
pub(crate) struct Attachment(Arc<RawTap>);
impl Drop for Attachment {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        if let Some(attached) = state.attached.take() {
            attached.valve.set_property("drop", true);
        }
        if let Some(pending) = state.pending.take() {
            let _ = pending
                .reply
                .send(Err(SnapshotError::unavailable("capture pipeline changed")));
        }
    }
}
pub(crate) struct TapLease(Arc<RawTap>);
impl Drop for TapLease {
    fn drop(&mut self) {
        self.0.leases.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct CaptureLease {
    pub(crate) tap: Arc<RawTap>,
    pub(crate) output_height: u32,
    pub(crate) _tap_lease: TapLease,
    pub(crate) _mjpeg: Option<crate::stream::mjpeg::ClientGuard>,
}
impl CaptureLease {
    pub(crate) fn tap(&self) -> &Arc<RawTap> {
        &self.tap
    }
}

struct Conversion(gst::Pipeline);
impl Drop for Conversion {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn conversion_caps(caps: &gst::CapsRef) -> gst::Caps {
    let mut caps = caps.to_owned();
    // amlvconv ignores 0/1 and reads uninitialized rate values from caps queries.
    // This is a single-frame conversion, so use the same nonzero rate at both
    // ends of GE2D. Copy caps to leave the shared capture pipeline untouched.
    caps.make_mut().set("framerate", gst::Fraction::new(1, 1));
    caps
}

fn start_conversion(
    pipeline: &gst::Pipeline,
    input: &gstreamer_app::AppSrc,
    caps: &gst::CapsRef,
) -> Result<(), SnapshotError> {
    let caps = conversion_caps(caps);
    input.set_caps(Some(&caps));
    pipeline
        .set_state(gst::State::Playing)
        .map_err(SnapshotError::unavailable)?;
    // appsrc normally pushes CAPS without querying downstream first. amlvconv
    // caches its output rate in the reverse caps query, not in set_info, so a
    // fixed output capsfilter alone leaves the rate from earlier template queries
    // uninitialized. Query the complete, fixed path before submitting our frame.
    let accepted = input
        .static_pad("src")
        .unwrap()
        .peer_query_caps(Some(&caps));
    if accepted.is_empty() {
        return Err(SnapshotError::unavailable(
            "conversion caps are unsupported",
        ));
    }
    Ok(())
}

fn converted_sample(
    pipeline: &gst::Pipeline,
    output: &gstreamer_app::AppSink,
    deadline: std::time::Instant,
) -> Result<gst::Sample, SnapshotError> {
    let bus = pipeline.bus().unwrap();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let wait = remaining.min(Duration::from_millis(10));
        if let Some(sample) =
            output.try_pull_sample(gst::ClockTime::from_nseconds(wait.as_nanos() as u64))
        {
            return Ok(sample);
        }
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(error) = message.view() {
                return Err(SnapshotError::unavailable(format!(
                    "conversion failed: {}",
                    error.error()
                )));
            }
        }
        // try_pull_sample also returns None on EOS, not only on timeout.
        if output.is_eos() {
            return Err(SnapshotError::unavailable(
                "conversion ended without producing a frame",
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(SnapshotError::Timeout);
        }
    }
}

pub(crate) fn encode(
    sample: gst::Sample,
    target: SnapshotTarget,
    quality: u8,
    deadline: std::time::Instant,
    output_height: u32,
    backend: VideoBackend,
) -> Result<SnapshotFrame, SnapshotError> {
    if std::time::Instant::now() >= deadline {
        return Err(SnapshotError::Timeout);
    }
    let error = SnapshotError::unavailable;
    let caps = sample.caps().ok_or_else(|| error("Missing raw caps"))?;
    let info = gstreamer_video::VideoInfo::from_caps(caps).map_err(SnapshotError::unavailable)?;
    let (sw, sh) = output_size((info.width(), info.height()), output_height);
    let g = geometry(target, (sw, sh))?;
    let padded = sample
        .buffer()
        .and_then(|buffer| buffer.meta::<gstreamer_video::VideoMeta>())
        .is_some_and(|meta| meta.stride()[0] != info.stride()[0]);
    let scaler = match backend {
        VideoBackend::Vendor if padded => "videoconvert ! videoscale",
        VideoBackend::Vendor => "amlvconv",
        VideoBackend::Wave5 => "videoconvert ! videoscale",
    };
    // The vendor path scales in DMA memory. Wave5 screenshots use bounded CPU
    // conversion; live capture/encoding retains its DMA path. Crop in RGB so odd
    // coordinates retain their exact pixel boundaries on either backend.
    let transform = match target {
        SnapshotTarget::Fit { .. } => format!("{scaler} ! video/x-raw,format=NV12,framerate=1/1,width={},height={} ! videoconvert ! video/x-raw,format=RGB ! videoscale ! video/x-raw,width={},height={} ! ",
            (g.width + 1) & !1, (g.height + 1) & !1, g.width, g.height),
        SnapshotTarget::Region { .. } => {
            let output_scale = if (sw,sh) != (info.width(),info.height()) {
                format!("{scaler} ! video/x-raw,format=NV12,framerate=1/1,width={sw},height={sh} ! ")
            } else { String::new() };
            format!("{output_scale}videoconvert ! video/x-raw,format=RGB ! videocrop left={} top={} right={} bottom={} ! videoscale ! video/x-raw,width={},height={} ! ",
                g.x, g.y, sw-g.x-g.w, sh-g.y-g.h, g.width, g.height)
        },
    };
    let pipeline = gst::parse::launch(&format!("appsrc name=input format=time ! {transform}appsink name=output sync=false async=false enable-last-sample=false max-buffers=1 drop=true"))
        .map_err(SnapshotError::unavailable)?.downcast::<gst::Pipeline>().map_err(|_| error("Invalid conversion pipeline"))?;
    let conversion = Conversion(pipeline);
    let input = conversion
        .0
        .by_name("input")
        .unwrap()
        .downcast::<gstreamer_app::AppSrc>()
        .unwrap();
    let output = conversion
        .0
        .by_name("output")
        .unwrap()
        .downcast::<gstreamer_app::AppSink>()
        .unwrap();
    start_conversion(&conversion.0, &input, caps)?;
    input
        .push_buffer(
            sample
                .buffer()
                .ok_or_else(|| error("Missing raw buffer"))?
                .to_owned(),
        )
        .map_err(SnapshotError::unavailable)?;
    input.end_of_stream().map_err(SnapshotError::unavailable)?;
    let rgb = converted_sample(&conversion.0, &output, deadline)?;
    // Only CPU RGB memory survives this point. Return the capture DMA buffer before
    // JPEG compression (including the potentially expensive quality retry).
    drop(sample);
    drop(input);
    drop(output);
    drop(conversion);
    let info =
        gstreamer_video::VideoInfo::from_caps(rgb.caps().ok_or_else(|| error("Missing RGB caps"))?)
            .map_err(SnapshotError::unavailable)?;
    if (info.width(), info.height()) != (g.width, g.height) {
        return Err(error("Unexpected RGB dimensions"));
    }
    let buffer = rgb.buffer().ok_or_else(|| error("Missing RGB buffer"))?;
    let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(SnapshotError::unavailable)?;
    let mut compressor = turbojpeg::Compressor::new().map_err(SnapshotError::unavailable)?;
    compressor
        .set_subsamp(turbojpeg::Subsamp::None)
        .map_err(SnapshotError::unavailable)?;
    for attempt in 0..2 {
        if std::time::Instant::now() >= deadline {
            return Err(SnapshotError::Timeout);
        }
        compressor
            .set_quality(i32::from(if attempt == 0 {
                quality
            } else {
                (quality / 2).max(1)
            }))
            .map_err(SnapshotError::unavailable)?;
        let jpeg = compressor
            .compress_to_vec(turbojpeg::Image {
                pixels: frame.plane_data(0).map_err(SnapshotError::unavailable)?,
                width: g.width as usize,
                height: g.height as usize,
                pitch: frame.plane_stride()[0] as usize,
                format: turbojpeg::PixelFormat::RGB,
            })
            .map_err(SnapshotError::unavailable)?;
        if std::time::Instant::now() >= deadline {
            return Err(SnapshotError::Timeout);
        }
        if jpeg.len() < MAX_JPEG_BYTES {
            return Ok(SnapshotFrame {
                jpeg: Bytes::from(jpeg),
                width: g.width,
                height: g.height,
                source_width: sw,
                source_height: sh,
            });
        }
    }
    Err(error("JPEG exceeds 1 MiB after quality reduction"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave5_nv12_screenshots_scale_and_preserve_odd_crop_boundaries() {
        gst::init().unwrap();
        let info = gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::Nv12, 64, 48)
            .fps(gst::Fraction::new(60, 1))
            .build()
            .unwrap();
        let mut pixels = vec![128; info.size()];
        pixels[..info.offset()[1]].fill(16);
        for y in 5..12 {
            for x in 3..14 {
                pixels[y * info.stride()[0] as usize + x] = 235;
            }
        }
        let sample = gst::Sample::builder()
            .buffer(&gst::Buffer::from_mut_slice(pixels))
            .caps(&info.to_caps().unwrap())
            .build();
        let region = SnapshotTarget::Region {
            x: 3,
            y: 5,
            w: 11,
            h: 7,
            max_long_edge: 64,
            max_upscale: 1.0,
        };
        for (target, output_height, expected, source_size) in [
            (
                SnapshotTarget::Fit { max_long_edge: 31 },
                0,
                (31, 23),
                (64, 48),
            ),
            (region, 0, (11, 7), (64, 48)),
            (
                SnapshotTarget::Fit { max_long_edge: 31 },
                24,
                (31, 23),
                (32, 24),
            ),
            (
                SnapshotTarget::Region {
                    x: 0,
                    y: 0,
                    w: 32,
                    h: 24,
                    max_long_edge: 32,
                    max_upscale: 1.0,
                },
                24,
                (32, 24),
                (32, 24),
            ),
        ] {
            let frame = encode(
                sample.clone(),
                target,
                95,
                std::time::Instant::now() + Duration::from_secs(3),
                output_height,
                VideoBackend::Wave5,
            )
            .unwrap();
            let decoded = turbojpeg::decompress(&frame.jpeg, turbojpeg::PixelFormat::RGB).unwrap();
            assert_eq!((decoded.width, decoded.height), expected);
            assert_eq!((frame.source_width, frame.source_height), source_size);
            if expected == (11, 7) {
                assert!(
                    decoded.pixels.iter().all(|value| *value > 230),
                    "A crop rounded to chroma boundaries includes the black border"
                );
            }
        }
    }

    #[test]
    fn single_frame_conversion_negotiates_a_rate_without_changing_capture_caps() {
        gst::init().unwrap();
        for rate in [gst::Fraction::new(0, 1), gst::Fraction::new(60000, 1001)] {
            let sample = raw_sample(16, 16);
            let mut caps = sample.caps().unwrap().to_owned();
            caps.make_mut().set("framerate", rate);
            let sample = gst::Sample::builder()
                .buffer(&sample.buffer().unwrap().to_owned())
                .caps(&caps)
                .build();
            let pipeline = gst::parse::launch("appsrc name=input format=time ! identity name=converter ! video/x-raw,framerate=1/1 ! appsink name=output sync=false async=false")
                .unwrap().downcast::<gst::Pipeline>().unwrap();
            let conversion = Conversion(pipeline);
            // Model the vendor converter's dependency on a fixed caps query
            // before its only frame: without the query it drops that frame.
            let queried = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen = queried.clone();
            conversion
                .0
                .by_name("converter")
                .unwrap()
                .static_pad("sink")
                .unwrap()
                .add_probe(
                    gst::PadProbeType::QUERY_DOWNSTREAM | gst::PadProbeType::BUFFER,
                    move |_, probe| {
                        if let Some(query) = probe.query() {
                            if let gst::QueryView::Caps(query) = query.view() {
                                if query.filter().is_some_and(|caps| caps.is_fixed()) {
                                    seen.store(true, Ordering::Release);
                                }
                            }
                        }
                        if probe.buffer().is_some() && !seen.load(Ordering::Acquire) {
                            gst::PadProbeReturn::Drop
                        } else {
                            gst::PadProbeReturn::Ok
                        }
                    },
                );
            let input = conversion
                .0
                .by_name("input")
                .unwrap()
                .downcast::<gstreamer_app::AppSrc>()
                .unwrap();
            let output = conversion
                .0
                .by_name("output")
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            start_conversion(&conversion.0, &input, sample.caps().unwrap()).unwrap();
            input
                .push_buffer(sample.buffer().unwrap().to_owned())
                .unwrap();
            input.end_of_stream().unwrap();
            let converted = converted_sample(
                &conversion.0,
                &output,
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(
                converted.buffer().unwrap().size(),
                sample.buffer().unwrap().size()
            );
            assert_eq!(
                sample
                    .caps()
                    .unwrap()
                    .structure(0)
                    .unwrap()
                    .get::<gst::Fraction>("framerate")
                    .unwrap(),
                rate
            );
        }
    }

    #[test]
    fn conversion_distinguishes_dropped_frame_error_and_deadline() {
        gst::init().unwrap();
        for (source, expected) in [
            (
                "videotestsrc num-buffers=1 ! identity drop-probability=1.0",
                "ended without",
            ),
            (
                "videotestsrc num-buffers=1 ! identity error-after=1",
                "conversion failed",
            ),
            ("appsrc format=time", "timeout"),
        ] {
            let pipeline = gst::parse::launch(&format!(
                "{source} ! appsink name=output sync=false async=false"
            ))
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
            let conversion = Conversion(pipeline);
            let output = conversion
                .0
                .by_name("output")
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            conversion.0.set_state(gst::State::Playing).unwrap();
            let deadline = std::time::Instant::now()
                + if expected == "timeout" {
                    Duration::ZERO
                } else {
                    Duration::from_secs(1)
                };
            match converted_sample(&conversion.0, &output, deadline) {
                Err(SnapshotError::Unavailable(message)) if expected != "timeout" => {
                    assert!(message.contains(expected), "{message}");
                }
                Err(SnapshotError::Timeout) if expected == "timeout" => {}
                result => panic!("Expected {expected}, got {result:?}"),
            }
        }
    }

    fn raw_sample(width: u32, height: u32) -> gst::Sample {
        gst::init().unwrap();
        let info =
            gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::Rgb, width, height)
                .build()
                .unwrap();
        let mut data = vec![0; info.size()];
        let mut seed = 17u32;
        for byte in &mut data {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *byte = seed as u8;
        }
        gst::Sample::builder()
            .buffer(&gst::Buffer::from_mut_slice(data))
            .caps(&info.to_caps().unwrap())
            .build()
    }

    #[test]
    fn region_encodes_real_jpeg_and_retries_large_output() {
        let sample = raw_sample(768, 768);
        let mut compressor = turbojpeg::Compressor::new().unwrap();
        compressor.set_quality(100).unwrap();
        compressor.set_subsamp(turbojpeg::Subsamp::None).unwrap();
        let map = sample.buffer().unwrap().map_readable().unwrap();
        let original = compressor
            .compress_to_vec(turbojpeg::Image {
                pixels: map.as_slice(),
                width: 768,
                height: 768,
                pitch: 768 * 3,
                format: turbojpeg::PixelFormat::RGB,
            })
            .unwrap();
        assert!(
            original.len() >= MAX_JPEG_BYTES,
            "Fixture must exercise the quality retry"
        );
        drop(map);
        let target = SnapshotTarget::Region {
            x: 0,
            y: 0,
            w: 768,
            h: 768,
            max_long_edge: 768,
            max_upscale: 1.0,
        };
        let frame = encode(
            sample,
            target,
            100,
            std::time::Instant::now() + Duration::from_secs(3),
            0,
            VideoBackend::Vendor,
        )
        .unwrap();
        assert!(frame.jpeg.len() < MAX_JPEG_BYTES);
        let decoded = turbojpeg::decompress(&frame.jpeg, turbojpeg::PixelFormat::RGB).unwrap();
        assert_eq!((decoded.width, decoded.height), (768, 768));
        assert_eq!((frame.source_width, frame.source_height), (768, 768));
    }

    #[test]
    fn oversized_retry_and_expired_deadline_fail() {
        let target = SnapshotTarget::Region {
            x: 0,
            y: 0,
            w: 1536,
            h: 1536,
            max_long_edge: 1536,
            max_upscale: 1.0,
        };
        let sample = raw_sample(1536, 1536);
        assert!(matches!(
            encode(
                sample.clone(),
                target,
                100,
                std::time::Instant::now(),
                0,
                VideoBackend::Vendor
            ),
            Err(SnapshotError::Timeout)
        ));
        assert!(matches!(
            encode(
                sample,
                target,
                100,
                std::time::Instant::now() + Duration::from_secs(3),
                0,
                VideoBackend::Vendor,
            ),
            Err(SnapshotError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn tap_delivers_new_frame_without_retaining_an_idle_sample() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(&format!("appsrc name=input is-live=true format=time ! {BRANCH} fakesink sync=false async=false enable-last-sample=false"))
            .unwrap().downcast::<gst::Pipeline>().unwrap();
        let capture = Conversion(pipeline);
        let input = capture
            .0
            .by_name("input")
            .unwrap()
            .downcast::<gstreamer_app::AppSrc>()
            .unwrap();
        input.set_caps(Some(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .field("width", 2i32)
                .field("height", 2i32)
                .field("framerate", gst::Fraction::new(30, 1))
                .build(),
        ));
        capture.0.set_state(gst::State::Playing).unwrap();
        let tap = Arc::new(RawTap::default());
        let _attachment = tap.attach(&capture.0).unwrap();
        let mut waiting = Box::pin(tap.sample());
        assert!(futures::poll!(&mut waiting).is_pending());
        for pts in [gst::ClockTime::ZERO, gst::ClockTime::from_seconds(100)] {
            let mut buffer = gst::Buffer::with_size(16).unwrap();
            buffer.get_mut().unwrap().set_pts(pts);
            input.push_buffer(buffer).unwrap();
        }
        let sample = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sample.buffer().unwrap().pts(),
            Some(gst::ClockTime::from_seconds(100))
        );
        assert!(tap.state.lock().pending.is_none());
        assert!(capture
            .0
            .by_name("snapshot_valve")
            .unwrap()
            .property::<bool>("drop"));
        let sink = capture
            .0
            .by_name("snapshot_raw")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        assert!(sink
            .property::<Option<gst::Sample>>("last-sample")
            .is_none());
        assert!(sink.try_pull_preroll(gst::ClockTime::ZERO).is_none());
        assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
    }

    #[test]
    fn region_preserves_odd_pixel_boundaries() {
        let sample = raw_sample(64, 48);
        let target = SnapshotTarget::Region {
            x: 3,
            y: 5,
            w: 11,
            h: 7,
            max_long_edge: 1280,
            max_upscale: 4.0,
        };
        let frame = encode(
            sample,
            target,
            80,
            std::time::Instant::now() + Duration::from_secs(3),
            0,
            VideoBackend::Vendor,
        )
        .unwrap();
        let decoded = turbojpeg::decompress(&frame.jpeg, turbojpeg::PixelFormat::RGB).unwrap();
        assert_eq!((decoded.width, decoded.height), (44, 28));
    }

    #[tokio::test]
    async fn timeout_waiting_for_capture_releases_request() {
        let tap = Arc::new(RawTap::default());
        assert!(tokio::time::timeout(Duration::ZERO, tap.sample())
            .await
            .is_err());
        assert!(tap.state.lock().pending.is_none());
    }

    #[tokio::test]
    async fn cancellation_closes_valve_and_pipeline_loss_wakes_request() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=input is-live=true format=time ! {BRANCH} fakesink sync=false async=false"
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let capture = Conversion(pipeline);
        capture.0.set_state(gst::State::Playing).unwrap();
        let tap = Arc::new(RawTap::default());
        let attachment = tap.attach(&capture.0).unwrap();
        let valve = capture.0.by_name("snapshot_valve").unwrap();
        {
            let mut waiting = Box::pin(tap.sample());
            assert!(futures::poll!(&mut waiting).is_pending());
            assert!(!valve.property::<bool>("drop"));
        }
        assert!(valve.property::<bool>("drop"));
        assert!(tap.state.lock().pending.is_none());
        let mut waiting = Box::pin(tap.sample());
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(attachment);
        assert!(matches!(waiting.await, Err(SnapshotError::Unavailable(_))));
        assert!(valve.property::<bool>("drop"));
    }
    #[test]
    fn rejects_old_missing_and_equal_timestamps() {
        let after = gst::ClockTime::from_seconds(5);
        assert!(!is_fresh(None, after));
        assert!(!is_fresh(Some(after), after));
        assert!(!is_fresh(Some(gst::ClockTime::from_seconds(4)), after));
        assert!(is_fresh(Some(gst::ClockTime::from_seconds(6)), after));
    }
}
