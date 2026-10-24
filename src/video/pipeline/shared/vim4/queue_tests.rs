use super::*;
use crate::platform::vim4::OutputSettings;
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video::{VideoFormat, VideoInfo};

fn pipeline_with_software_scaler(
    resolution: Resolution,
    output: OutputSettings,
) -> (Capture, AppSrc, AppSink, VideoInfo) {
    gst::init().unwrap();
    let info = VideoInfo::builder(VideoFormat::Nv21, resolution.width, resolution.height)
        .fps(gst::Fraction::new(60, 1))
        .build()
        .unwrap();
    let filter = output
        .filter(resolution, 60)
        .replace("amlvconv", "videoconvertscale");
    let pipeline = gst::parse::launch(&format!(
        "appsrc name=source format=time ! {filter}identity name=encoder ! \
         appsink name=frames sync=false async=false"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let source = pipeline
        .by_name("source")
        .unwrap()
        .downcast::<AppSrc>()
        .unwrap();
    source.set_caps(Some(&info.to_caps().unwrap()));
    let sink = pipeline
        .by_name("frames")
        .unwrap()
        .downcast::<AppSink>()
        .unwrap();
    (Capture(pipeline), source, sink, info)
}

fn frame(info: &VideoInfo, sequence: u64) -> gst::Buffer {
    let mut buffer = gst::Buffer::from_mut_slice(vec![128; info.size()]);
    let writable = buffer.get_mut().unwrap();
    writable.set_pts(gst::ClockTime::from_nseconds(sequence * 1_000_000_000 / 60));
    writable.set_duration(gst::ClockTime::from_nseconds(1_000_000_000 / 60));
    buffer
}

#[test]
fn native_uhd_frames_keep_their_original_memory() {
    let (capture, source, sink, info) = pipeline_with_software_scaler(
        Resolution::new(3840, 2160),
        OutputSettings { height: 0, fps: 60 },
    );
    let buffer = frame(&info, 0);
    let memory = buffer.peek_memory(0).as_ptr();
    capture.0.set_state(gst::State::Playing).unwrap();
    source.push_buffer(buffer).unwrap();
    source.end_of_stream().unwrap();
    let sample = sink
        .try_pull_sample(gst::ClockTime::from_seconds(2))
        .unwrap();
    assert_eq!(sample.buffer().unwrap().peek_memory(0).as_ptr(), memory);
    let output = VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
    assert_eq!((output.width(), output.height()), (3840, 2160));
}

#[test]
fn scaling_progresses_while_the_encoder_holds_a_frame() {
    let (capture, source, _sink, info) = pipeline_with_software_scaler(
        Resolution::new(64, 48),
        OutputSettings {
            height: 24,
            fps: 60,
        },
    );
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = ParkingMutex::new(release_rx);
    let blocked = AtomicBool::new(false);
    capture
        .0
        .by_name("encoder")
        .unwrap()
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if !blocked.swap(true, Ordering::Relaxed) {
                entered_tx.send(()).unwrap();
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(5));
            }
            gst::PadProbeReturn::Ok
        });
    let (scaled_tx, scaled_rx) = std::sync::mpsc::sync_channel(1);
    let frames = AtomicU64::new(0);
    capture
        .0
        .by_name("scaler")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if frames.fetch_add(1, Ordering::Relaxed) == 1 {
                scaled_tx.send(()).unwrap();
            }
            gst::PadProbeReturn::Ok
        });
    capture.0.set_state(gst::State::Playing).unwrap();
    for sequence in 0..3 {
        source.push_buffer(frame(&info, sequence)).unwrap();
    }
    source.end_of_stream().unwrap();
    let entered = entered_rx.recv_timeout(Duration::from_secs(2));
    let scaled = scaled_rx.recv_timeout(Duration::from_secs(2));
    let _ = release_tx.send(());
    assert!(entered.is_ok(), "encoder did not receive a frame");
    assert!(
        scaled.is_ok(),
        "scaling serialized behind the blocked encoder"
    );
}
