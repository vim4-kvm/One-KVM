//! Remux shared Annex-B video; only the existing Opus audio is converted to AAC.
use crate::config::{RtmpOutputConfig, SrtOutputConfig};
use anyhow::{bail, Context, Result};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

static PIPELINES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub(super) enum Destination {
    Srt(SrtOutputConfig),
    Rtmp(RtmpOutputConfig),
}

impl Destination {
    pub fn audio(&self) -> bool {
        match self {
            Self::Srt(c) => c.audio,
            Self::Rtmp(c) => c.audio,
        }
    }
}

pub(super) struct Mux {
    pipeline: gst::Pipeline,
    video: AppSrc,
    audio: Option<AppSrc>,
    sink: gst::Element,
    pub request_keyframe: Arc<AtomicBool>,
    callers: Arc<AtomicU64>,
    permit: Option<tokio::sync::SemaphorePermit<'static>>,
    srt: bool,
}

impl Mux {
    pub fn new(destination: Destination, hevc: bool) -> Result<Self> {
        let permit = PIPELINES
            .try_acquire()
            .context("Output cleanup is still pending")?;
        gst::init()?;
        let srt = matches!(destination, Destination::Srt(_));
        if !srt && hevc {
            bail!("RTMP requires H.264");
        }
        let codec = if hevc { "h265" } else { "h264" };
        let output = if srt {
            "mpegtsmux name=mux alignment=7 ! srtsink name=output sync=false async=false wait-for-connection=false"
        } else {
            "flvmux name=mux streamable=true ! rtmp2sink name=output sync=false async=false timeout=5"
        };
        let audio = if destination.audio() {
            "appsrc name=audio format=time block=false ! opusparse ! opusdec ! audioconvert ! audioresample ! avenc_aac bitrate=128000 ! aacparse ! queue max-size-buffers=0 max-size-bytes=2097152 max-size-time=1000000000 ! mux."
        } else {
            ""
        };
        // Shared packets already provide pacing and timestamps. Non-live appsrc
        // lets the mux wait for AAC conversion rather than timing out on its own
        // clock and clamping late audio timestamps to newer video timestamps.
        let pipeline = gst::parse::launch(&format!(
            "{output} appsrc name=video format=time block=false ! {codec}parse config-interval=-1 ! queue max-size-buffers=0 max-size-bytes=16777216 max-size-time=1000000000 ! mux. {audio}"
        )).context("Required streaming plugin is unavailable")?
            .downcast::<gst::Pipeline>().map_err(|_| anyhow::anyhow!("Invalid output pipeline"))?;
        let video = pipeline
            .by_name("video")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        video.set_caps(Some(
            &gst::Caps::builder(&format!("video/x-{codec}"))
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));
        let audio = pipeline
            .by_name("audio")
            .map(|s| s.downcast::<AppSrc>().unwrap());
        if let Some(source) = &audio {
            source.set_caps(Some(
                &gst::Caps::builder("audio/x-opus")
                    .field("rate", 48000i32)
                    .field("channels", 2i32)
                    .field("channel-mapping-family", 0i32)
                    .build(),
            ));
        }
        let sink = pipeline.by_name("output").unwrap();
        let request_keyframe = Arc::new(AtomicBool::new(false));
        let callers = Arc::new(AtomicU64::new(0));
        match destination {
            Destination::Srt(c) => {
                sink.set_property("uri", format!("srt://0.0.0.0:{}?mode=listener", c.port));
                sink.set_property("latency", c.latency_ms as i32);
                sink.set_property("poll-timeout", 200i32);
                if !c.passphrase.is_empty() {
                    sink.set_property("passphrase", c.passphrase);
                    sink.set_property_from_str("pbkeylen", "32");
                }
                let key = request_keyframe.clone();
                let count = callers.clone();
                sink.connect("caller-added", false, move |_| {
                    count.fetch_add(1, Ordering::Relaxed);
                    key.store(true, Ordering::Relaxed);
                    None
                });
                let count = callers.clone();
                sink.connect("caller-removed", false, move |_| {
                    let _ = count.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        Some(n.saturating_sub(1))
                    });
                    None
                });
            }
            Destination::Rtmp(c) => {
                sink.set_property("location", c.url);
            }
        }
        // Bound both appsrc queues as well as the downstream queues. A stalled
        // connection is restarted from an IDR rather than dropping arbitrary P frames.
        for source in std::iter::once(&video).chain(audio.iter()) {
            source.set_max_bytes(16 * 1024 * 1024);
            source.set_property("max-time", 1_000_000_000u64);
        }
        let mux = Self {
            pipeline,
            video,
            audio,
            sink,
            request_keyframe,
            callers,
            permit: Some(permit),
            srt,
        };
        mux.pipeline
            .set_state(gst::State::Playing)
            .context("Output could not start")?;
        Ok(mux)
    }

    pub fn push(
        &self,
        audio: bool,
        data: &[u8],
        pts: Duration,
        duration: Duration,
        key: bool,
    ) -> Result<()> {
        let source = if audio {
            self.audio.as_ref().context("Audio output is disabled")?
        } else {
            &self.video
        };
        if source
            .current_level_bytes()
            .saturating_add(data.len() as u64)
            > 16 * 1024 * 1024
            || source.property::<u64>("current-level-time") > 1_000_000_000
        {
            bail!("Output cannot keep up");
        }
        let mut buffer = gst::Buffer::from_mut_slice(data.to_vec());
        let b = buffer.get_mut().unwrap();
        b.set_pts(gst::ClockTime::from_nseconds(pts.as_nanos() as u64));
        b.set_dts(gst::ClockTime::from_nseconds(pts.as_nanos() as u64));
        b.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
        if !audio && !key {
            b.set_flags(gst::BufferFlags::DELTA_UNIT);
        }
        source.push_buffer(buffer).context("Output stopped")?;
        self.check()
    }

    pub fn check(&self) -> Result<()> {
        if self
            .pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
            .is_some()
        {
            // Plugin errors/debug strings may include publishing credentials.
            bail!("Output connection failed; check the receiver and publishing credentials");
        }
        Ok(())
    }

    pub fn transport(&self) -> (u64, u64) {
        let stats = self.sink.property::<gst::Structure>("stats");
        let bytes = stats
            .get::<u64>(if self.srt {
                "bytes-sent-total"
            } else {
                "out-bytes-total"
            })
            .unwrap_or(0);
        (bytes, self.callers.load(Ordering::Relaxed))
    }
}

impl Drop for Mux {
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone();
        let permit = self.permit.take();
        // Network shutdown may wait in the plugin. Never hold the async runtime
        // or shared video/audio subscriptions while that shutdown completes.
        std::thread::spawn(move || {
            let _ = pipeline.set_state(gst::State::Null);
            drop(pipeline);
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gstreamer_app::AppSink;

    struct ReceiverProcess(std::process::Child);
    impl Drop for ReceiverProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn samples(description: &str) -> Vec<gst::Sample> {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(description)
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let sink = pipeline
            .by_name("sink")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut samples = Vec::new();
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(10)) {
            samples.push(sample);
        }
        let eos = sink.is_eos();
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(eos, "Synthetic encoder timed out");
        assert!(!samples.is_empty());
        samples
    }

    #[tokio::test]
    async fn stalled_output_rejects_frames_at_the_queue_limit() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let mux = Mux::new(
            Destination::Srt(SrtOutputConfig {
                port,
                audio: false,
                ..Default::default()
            }),
            false,
        )
        .unwrap();
        let pad = mux.video.static_pad("src").unwrap();
        let probe = pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_, _| {
                gst::PadProbeReturn::Ok
            })
            .unwrap();
        let bytes = vec![0; 8 * 1024 * 1024];
        let mut rejected = false;
        for i in 0..5 {
            if mux
                .push(
                    false,
                    &bytes,
                    Duration::from_millis(i * 40),
                    Duration::from_millis(40),
                    true,
                )
                .is_err()
            {
                rejected = true;
                break;
            }
        }
        let queued = mux.video.current_level_bytes();
        pad.remove_probe(probe);
        assert!(rejected);
        assert!(queued <= 16 * 1024 * 1024);
    }

    #[tokio::test]
    async fn srt_roundtrip_decodes_shared_h264_h265_and_aac_with_encryption() {
        gst::init().unwrap();
        for hevc in [false, true] {
            let encoder = if hevc {
                "x265enc tune=zerolatency speed-preset=ultrafast option-string=pools=1"
            } else {
                "openh264enc"
            };
            let video = samples(&format!("videotestsrc num-buffers=40 pattern=ball ! video/x-raw,width=160,height=120,framerate=25/1,format=I420 ! {encoder} ! appsink name=sink sync=false"));
            let audio = samples("audiotestsrc num-buffers=80 samplesperbuffer=960 wave=sine ! audio/x-raw,rate=48000,channels=2 ! opusenc ! appsink name=sink sync=false");
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let port = socket.local_addr().unwrap().port();
            drop(socket);
            let secret = if hevc { "srt-roundtrip-test" } else { "" };
            let mux = Mux::new(
                Destination::Srt(SrtOutputConfig {
                    port,
                    latency_ms: 20,
                    passphrase: secret.into(),
                    ..Default::default()
                }),
                hevc,
            )
            .unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            let receiver = gst::parse::launch(
                "srtsrc name=input poll-timeout=200 ! filesink name=file sync=false",
            )
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
            let source = receiver.by_name("input").unwrap();
            source.set_property("uri", format!("srt://127.0.0.1:{port}?mode=caller"));
            source.set_property("latency", 20i32);
            if !secret.is_empty() {
                source.set_property("passphrase", secret);
                source.set_property_from_str("pbkeylen", "32");
            }
            receiver
                .by_name("file")
                .unwrap()
                .set_property("location", file.path().to_str().unwrap());
            receiver.set_state(gst::State::Playing).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while !mux.request_keyframe.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("SRT caller did not connect");
            let mut packets = Vec::new();
            for (is_audio, list) in [(false, &video), (true, &audio)] {
                let origin = list[0].buffer().unwrap().pts().unwrap();
                for sample in list {
                    let buffer = sample.buffer().unwrap();
                    packets.push((
                        Duration::from_nanos((buffer.pts().unwrap() - origin).nseconds()),
                        is_audio,
                        sample,
                    ));
                }
            }
            packets.sort_by_key(|(pts, _, _)| *pts);
            let start = tokio::time::Instant::now();
            for (pts, is_audio, sample) in packets {
                tokio::time::sleep_until(start + pts).await;
                let buffer = sample.buffer().unwrap();
                mux.push(
                    is_audio,
                    buffer.map_readable().unwrap().as_slice(),
                    pts,
                    Duration::from_nanos(buffer.duration().unwrap().nseconds()),
                    !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                )
                .unwrap();
            }
            // Give the configured SRT latency and final AAC packet time to drain.
            tokio::time::sleep(Duration::from_millis(150)).await;
            mux.check().unwrap();
            receiver.set_state(gst::State::Null).unwrap();
            drop(mux);
            assert!(file.as_file().metadata().unwrap().len() > 1000);
            verify_decoding(file.path(), hevc, "tsdemux");
        }
    }

    fn verify_decoding(path: &std::path::Path, hevc: bool, demux: &str) {
        let decoder = if hevc {
            "h265parse ! libde265dec"
        } else {
            "h264parse ! openh264dec"
        };
        let decode = gst::parse::launch(&format!("filesrc name=file ! {demux} name=d d. ! queue ! {decoder} ! fakesink name=video sync=false signal-handoffs=true d. ! queue ! aacparse ! avdec_aac ! audioconvert ! audio/x-raw,format=S16LE ! appsink name=audio sync=false")).unwrap().downcast::<gst::Pipeline>().unwrap();
        decode
            .by_name("file")
            .unwrap()
            .set_property("location", path.to_str().unwrap());
        let count = Arc::new(AtomicU64::new(0));
        let frames = count.clone();
        decode
            .by_name("video")
            .unwrap()
            .connect("handoff", false, move |_| {
                frames.fetch_add(1, Ordering::Relaxed);
                None
            });
        let energy = Arc::new(AtomicU64::new(0));
        let total = energy.clone();
        decode
            .by_name("audio")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap()
            .set_callbacks(
                gstreamer_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                        let map = sample.buffer().unwrap().map_readable().unwrap();
                        let sum: u64 = map
                            .as_slice()
                            .chunks_exact(2)
                            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() as u64)
                            .sum();
                        total.fetch_add(sum, Ordering::Relaxed);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        decode.set_state(gst::State::Playing).unwrap();
        let message = decode.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        decode.set_state(gst::State::Null).unwrap();
        assert!(message.is_some_and(|m| matches!(m.view(), gst::MessageView::Eos(..))));
        assert!(count.load(Ordering::Relaxed) >= 20);
        assert!(energy.load(Ordering::Relaxed) > 100_000);
    }

    #[tokio::test]
    async fn rtmp_roundtrip_decodes_h264_and_non_silent_aac() {
        use std::process::{Command, Stdio};
        let video = samples("videotestsrc num-buffers=40 pattern=ball ! video/x-raw,width=160,height=120,framerate=25/1,format=I420 ! openh264enc ! appsink name=sink sync=false");
        let audio = samples("audiotestsrc num-buffers=80 samplesperbuffer=960 wave=sine ! audio/x-raw,rate=48000,channels=2 ! opusenc ! appsink name=sink sync=false");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let url = format!("rtmp://127.0.0.1:{port}/live/test");
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut receiver = ReceiverProcess(
            Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-listen",
                    "1",
                    "-i",
                    &url,
                    "-c",
                    "copy",
                    "-f",
                    "flv",
                    "-y",
                ])
                .arg(file.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("Install ffmpeg for RTMP integration tests"),
        );
        // RTMP's test receiver must bind before the publisher's first handshake.
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let listening = std::fs::read_to_string("/proc/net/tcp")
                    .unwrap()
                    .lines()
                    .any(|line| {
                        let f: Vec<_> = line.split_whitespace().collect();
                        f.get(1)
                            .is_some_and(|address| address.ends_with(&format!(":{port:04X}")))
                            && f.get(3) == Some(&"0A")
                    });
                if listening {
                    break;
                }
                assert!(
                    receiver.0.try_wait().unwrap().is_none(),
                    "RTMP test receiver exited"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "RTMP receiver did not bind");
        let mux = Mux::new(
            Destination::Rtmp(RtmpOutputConfig {
                url,
                ..Default::default()
            }),
            false,
        )
        .unwrap();
        let mut packets = Vec::new();
        for (is_audio, list) in [(false, &video), (true, &audio)] {
            let origin = list[0].buffer().unwrap().pts().unwrap();
            for sample in list {
                let buffer = sample.buffer().unwrap();
                let pts = Duration::from_nanos((buffer.pts().unwrap() - origin).nseconds());
                // Live audio arrives after video because it includes capture,
                // Opus and AAC buffering. Preserve its media clock despite that delay.
                let arrival = pts
                    + if is_audio {
                        Duration::from_millis(60)
                    } else {
                        Duration::ZERO
                    };
                packets.push((arrival, pts, is_audio, sample));
            }
        }
        packets.sort_by_key(|(arrival, _, _, _)| *arrival);
        let start = tokio::time::Instant::now();
        for (arrival, pts, is_audio, sample) in packets {
            tokio::time::sleep_until(start + arrival).await;
            let b = sample.buffer().unwrap();
            mux.push(
                is_audio,
                b.map_readable().unwrap().as_slice(),
                pts,
                Duration::from_nanos(b.duration().unwrap().nseconds()),
                !b.flags().contains(gst::BufferFlags::DELTA_UNIT),
            )
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        mux.check().unwrap();
        assert!(mux.transport().0 > 1000);
        drop(mux);
        let exit = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = receiver.0.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(exit.is_ok(), "RTMP receiver did not finish");
        assert!(file.as_file().metadata().unwrap().len() > 1000);
        verify_decoding(file.path(), false, "flvdemux");
        let packets = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_packets",
                "-show_entries",
                "packet=pts_time",
                "-of",
                "json",
            ])
            .arg(file.path())
            .output()
            .unwrap();
        assert!(packets.status.success());
        let packets: serde_json::Value = serde_json::from_slice(&packets.stdout).unwrap();
        let pts: Vec<f64> = packets["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["pts_time"].as_str().unwrap().parse().unwrap())
            .collect();
        assert!(pts.len() > 30);
        for pair in pts.windows(2).skip(2) {
            let delta = pair[1] - pair[0];
            assert!(
                (0.020..=0.023).contains(&delta),
                "AAC clock was changed by video interleaving: {delta}"
            );
        }
    }
}
