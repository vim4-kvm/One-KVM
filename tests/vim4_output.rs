#![cfg(feature = "vim4")]

use one_kvm::{
    platform::vim4::OutputSettings,
    video::{
        codec::{registry::VideoEncoderType, BitratePreset},
        format::{PixelFormat, Resolution},
        pipeline::{SharedVideoPipeline, SharedVideoPipelineConfig},
    },
};
use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

/// Run only with all KVM services stopped, as the service user and with its GStreamer environment.
#[tokio::test]
#[ignore = "requires exclusive VIM4 capture and encoder access"]
async fn live_output_changes_preserve_capture_and_produce_decodable_streams() {
    assert_eq!(std::env::var("VIM4_HARDWARE_TEST").as_deref(), Ok("1"));
    let destination = PathBuf::from(std::env::var("VIM4_OUTPUT_DIR").expect("Set VIM4_OUTPUT_DIR"));
    std::fs::create_dir_all(&destination).unwrap();
    let (source, source_fps) = one_kvm::platform::vim4::signal().expect("HDMI signal required");
    let pipeline = SharedVideoPipeline::new(SharedVideoPipelineConfig {
        resolution: source,
        fps: source_fps,
        input_format: PixelFormat::Nv21,
        output_codec: VideoEncoderType::H264,
        ..Default::default()
    })
    .unwrap();
    pipeline
        .start_with_device(
            one_kvm::platform::vim4::VIDEO_DEVICE.into(),
            4,
            80,
            None,
            None,
        )
        .await
        .unwrap();
    for (height, fps, kbps) in [
        (1080, 30, 2000),
        (720, 15, 8000),
        (900, 24, 12500),
        (0, 60, 20000),
        (1080, 60, 60000),
        (1080, 60, 2000),
        (720, 60, 8000),
        (0, 60, 4000),
    ] {
        let output = OutputSettings {
            height,
            fps,
            ..Default::default()
        };
        pipeline
            .set_vim4_encoding(output, BitratePreset::Custom(kbps))
            .await
            .unwrap();
        let config = pipeline.config().await;
        assert_eq!(config.bitrate_kbps(), kbps);
        let expected_height = if height == 0 {
            source.height
        } else {
            height.min(source.height)
        };
        let expected = Resolution::new(
            (u64::from(source.width) * u64::from(expected_height) / u64::from(source.height))
                as u32
                & !1,
            expected_height,
        );
        assert_eq!(config.resolution, expected);
        assert_eq!(config.fps, fps.min(source_fps));
        let mut frames = pipeline.subscribe();
        let mut file =
            std::fs::File::create(destination.join(format!("{height}-{fps}-{kbps}.h264"))).unwrap();
        let start = Instant::now();
        let mut count = 0;
        let mut keyframe = false;
        while start.elapsed() < Duration::from_secs(2) {
            let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
                .await
                .unwrap()
                .unwrap();
            keyframe |= frame.is_keyframe;
            if keyframe {
                file.write_all(&frame.data).unwrap();
            }
            count += 1;
        }
        let measured = count as f64 / start.elapsed().as_secs_f64();
        assert!(keyframe);
        assert!(
            measured <= f64::from(config.fps) + 3.0,
            "Measured {measured} fps exceeds the configured cap {}",
            config.fps
        );
        println!(
            "OUTPUT {}x{} target={}kbps measured={measured:.2}fps",
            config.resolution.width, config.resolution.height, kbps
        );
        if measured < f64::from(config.fps) - 3.0 {
            println!(
                "THROUGHPUT LIMIT: requested {}fps, observed {measured:.2}fps",
                config.fps
            );
        }
    }
    pipeline
        .stop_and_wait(Duration::from_secs(5))
        .await
        .unwrap();
}
