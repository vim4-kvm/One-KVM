use super::*;
use crate::platform::vim4::OutputSettings;
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video::{VideoFormat, VideoInfo};

#[test]
fn lowest_hevc_tier_keeps_recovery_keyframes_without_cyclic_refresh_overhead() {
    let mut config = SharedVideoPipelineConfig::default();
    config.output_codec = VideoEncoderType::H265;
    config.bitrate_preset = BitratePreset::Custom(1000);
    let source = Resolution::new(3840, 2160);
    let low = description(&config, source, 60, false).unwrap();
    assert!(low.contains("bitrate=1000 gop=0 framerate=60 intra-refresh-rows=0"));
    assert!(low.contains("min-force-key-unit-interval=250000000"));
    config.bitrate_preset = BitratePreset::Balanced;
    let normal = description(&config, source, 60, false).unwrap();
    assert!(normal.contains("bitrate=4000 gop=0 framerate=60 intra-refresh-rows=1"));
    config.output_codec = VideoEncoderType::H264;
    let avc = description(&config, source, 60, false).unwrap();
    assert!(avc.contains("intra-refresh-rows=3"));
}

#[test]
fn secondary_capture_negotiates_full_resolution_sdr_and_rejects_invalid_selection() {
    gst::init().unwrap();
    let source = vendor_capture(Some("1"), Resolution::new(3840, 2160), 60).unwrap();
    let pipeline = gst::parse::launch(&format!("{source} ! fakesink"))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    assert_eq!(
        pipeline
            .by_name("source")
            .unwrap()
            .property::<String>("device"),
        "/dev/video71"
    );
    let filter = pipeline
        .iterate_elements()
        .into_iter()
        .filter_map(|element| element.ok())
        .find(|element| {
            element
                .factory()
                .is_some_and(|factory| factory.name() == "capsfilter")
        })
        .unwrap();
    let caps = filter.property::<gst::Caps>("caps");
    let info = VideoInfo::from_caps(&caps).unwrap();
    assert_eq!(
        (info.width(), info.height(), info.fps()),
        (3840, 2160, gst::Fraction::new(60, 1))
    );
    assert_eq!(info.format(), VideoFormat::Nv21);
    assert!(vendor_capture(Some("unexpected"), Resolution::new(3840, 2160), 60).is_err());
}

#[test]
fn secondary_dma_allocation_retains_real_video_meta_across_snapshot_branches() {
    gst::init().unwrap();
    let pipeline = gst::parse::launch(&format!(
        "appsrc name=source ! {} fakesink",
        crate::video::snapshot::vim4::BRANCH
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    secondary_allocation(&pipeline, Resolution::new(3840, 2160), "source", true).unwrap();
    let mut query = gst::query::Allocation::new(None, true);
    assert!(pipeline
        .by_name("source")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .peer_query(&mut query));
    assert!(query
        .find_allocation_meta::<gstreamer_video::VideoMeta>()
        .is_some());
    let pools: Vec<_> = query.allocation_pools().collect();
    assert_eq!(pools.len(), 1);
    let (_, bytes, minimum, maximum) = &pools[0];
    assert_eq!(*bytes, 3840 * 2160 * 3 / 2);
    assert!(*minimum >= 6 && *maximum >= *minimum);
}

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
        OutputSettings {
            height: 0,
            fps: 60,
            hdr: false,
            ..Default::default()
        },
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
fn scaler_clock_failures_do_not_stop_frame_delivery() {
    let mut unavailable = std::process::Command::new("/nonexistent-vim4-clock-helper");
    let mut rejected = std::process::Command::new("/bin/sh");
    rejected.args(["-c", "exit 1"]);
    for command in [&mut unavailable, &mut rejected] {
        let (capture, source, sink, info) = pipeline_with_software_scaler(
            Resolution::new(64, 48),
            OutputSettings {
                height: 24,
                fps: 60,
                hdr: false,
                ..Default::default()
            },
        );
        capture.0.set_state(gst::State::Playing).unwrap();
        for sequence in 0..3 {
            source.push_buffer(frame(&info, sequence)).unwrap();
        }
        source.end_of_stream().unwrap();
        let first = sink
            .try_pull_sample(gst::ClockTime::from_seconds(2))
            .unwrap();
        let () = check_scaler_clock(command);
        let mut previous = first.buffer().unwrap().pts().unwrap();
        for _ in 0..2 {
            let sample = sink
                .try_pull_sample(gst::ClockTime::from_seconds(2))
                .expect("video must continue after the clock helper fails");
            let timestamp = sample.buffer().unwrap().pts().unwrap();
            assert!(timestamp > previous);
            previous = timestamp;
        }
    }
}

#[test]
fn scaling_progresses_while_the_encoder_holds_a_frame() {
    let (capture, source, _sink, info) = pipeline_with_software_scaler(
        Resolution::new(64, 48),
        OutputSettings {
            height: 24,
            fps: 60,
            hdr: false,
            ..Default::default()
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
