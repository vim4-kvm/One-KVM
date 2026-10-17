//! Mux existing encoded packets; never open another capture device or encoder.
use std::{
    fs::File,
    os::fd::AsRawFd,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::{failure, RecordingErrorCode};
use anyhow::{bail, Context, Result};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;

pub(super) struct Mux {
    pipeline: gst::Pipeline,
    video: AppSrc,
    audio: AppSrc,
    // fdsink borrows this descriptor until the pipeline reaches NULL.
    _file: Option<File>,
    bytes: AtomicU64,
    permit: Option<tokio::sync::SemaphorePermit<'static>>,
}
static PIPELINES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

impl Mux {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn new(file: File, hevc: bool) -> Result<Self> {
        Self::new_inner(file, hevc).map_err(|e| {
            let code = super::error_code(&e);
            failure(code, e)
        })
    }

    fn new_inner(file: File, hevc: bool) -> Result<Self> {
        let permit = PIPELINES.try_acquire().map_err(|_| {
            failure(
                RecordingErrorCode::StorageUnavailable,
                "Storage cleanup is still pending",
            )
        })?;
        gst::init()?;
        let codec = if hevc { "h265" } else { "h264" };
        let pipeline = gst::parse::launch(&format!(
            "matroskamux name=mux ! fdsink name=output sync=false async=false \
             appsrc name=video format=time is-live=true block=false ! {codec}parse ! mux.video_0 \
             appsrc name=audio format=time is-live=true block=false ! opusparse ! mux.audio_0"
        ))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("Invalid recording pipeline"))?;
        let video = pipeline
            .by_name("video")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        let audio = pipeline
            .by_name("audio")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        video.set_caps(Some(
            &gst::Caps::builder(&format!("video/x-{codec}"))
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));
        audio.set_caps(Some(
            &gst::Caps::builder("audio/x-opus")
                .field("rate", 48000i32)
                .field("channels", 2i32)
                .field("channel-mapping-family", 0i32)
                .build(),
        ));
        pipeline
            .by_name("output")
            .unwrap()
            .set_property("fd", file.as_raw_fd());
        let mux = Self {
            pipeline,
            video,
            audio,
            _file: Some(file),
            bytes: AtomicU64::new(0),
            permit: Some(permit),
        };
        mux.pipeline.set_state(gst::State::Playing)?;
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
        let source = if audio { &self.audio } else { &self.video };
        // Never block shared capture on a slow disk or silently lose part of a GOP.
        if source
            .current_level_bytes()
            .saturating_add(data.len() as u64)
            > 16 * 1024 * 1024
            || source.property::<u64>("current-level-time") > 2_000_000_000
        {
            return Err(failure(
                RecordingErrorCode::StorageError,
                "Recording storage cannot keep up",
            ));
        }
        let mut buffer = gst::Buffer::from_mut_slice(data.to_vec());
        let writable = buffer.get_mut().unwrap();
        writable.set_pts(gst::ClockTime::from_nseconds(pts.as_nanos() as u64));
        writable.set_dts(gst::ClockTime::from_nseconds(pts.as_nanos() as u64));
        writable.set_duration(gst::ClockTime::from_nseconds(duration.as_nanos() as u64));
        if !audio && !key {
            writable.set_flags(gst::BufferFlags::DELTA_UNIT);
        }
        source
            .push_buffer(buffer)
            .map_err(|e| failure(RecordingErrorCode::PipelineStopped, e))?;
        self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.check()
    }

    pub fn check(&self) -> Result<()> {
        if let Some(message) = self
            .pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
        {
            if let gst::MessageView::Error(error) = message.view() {
                return Err(failure(
                    RecordingErrorCode::StorageError,
                    format!("Recording mux failed: {}", error.error()),
                ));
            }
        }
        Ok(())
    }

    pub async fn finish(&self) -> Result<()> {
        self.finish_inner()
            .await
            .map_err(|e| failure(RecordingErrorCode::SaveFailed, e))
    }

    async fn finish_inner(&self) -> Result<()> {
        self.video.end_of_stream()?;
        self.audio.end_of_stream()?;
        let bus = self.pipeline.bus().unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(message) =
                    bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])
                {
                    match message.view() {
                        gst::MessageView::Eos(..) => return Ok(()),
                        gst::MessageView::Error(error) => {
                            bail!("Recording finalization failed: {}", error.error())
                        }
                        _ => (),
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("Recording finalization timed out")?
    }
}

impl Drop for Mux {
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone();
        let file = self._file.take();
        let permit = self.permit.take();
        // NULL waits for fdsink's write; keep its borrowed fd alive off-runtime.
        std::thread::spawn(move || {
            let _ = pipeline.set_state(gst::State::Null);
            drop(pipeline);
            drop(file);
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_sink_cannot_accumulate_unbounded_appsrc_data() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mux = Mux::new(file.reopen().unwrap(), false).unwrap();
        // Block the source pad before any packet reaches the parser. The source
        // remains running, unlike a state transition which may flush its queue.
        let pad = mux.video.static_pad("src").unwrap();
        let probe = pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_, _| {
                gst::PadProbeReturn::Ok
            })
            .unwrap();
        let packet = vec![0; 8 * 1024 * 1024];
        let mut rejected = false;
        for i in 0..4 {
            if let Err(error) = mux.push(
                false,
                &packet,
                Duration::from_secs(i),
                Duration::from_secs(1),
                true,
            ) {
                assert_eq!(
                    super::super::error_code(&error),
                    RecordingErrorCode::StorageError
                );
                rejected = true;
                break;
            }
        }
        let queued = mux.video.current_level_bytes();
        pad.remove_probe(probe);
        assert!(rejected);
        assert!(queued <= 16 * 1024 * 1024);
    }
    use gstreamer_app::{AppSink, AppSinkCallbacks};
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn stopped_mux_reports_pipeline_and_finalization_codes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mux = Mux::new(file.reopen().unwrap(), false).unwrap();
        mux.pipeline.set_state(gst::State::Null).unwrap();
        let error = mux
            .push(
                false,
                &[0, 0, 0, 1],
                Duration::ZERO,
                Duration::from_millis(40),
                true,
            )
            .unwrap_err();
        assert_eq!(
            super::super::error_code(&error),
            RecordingErrorCode::PipelineStopped
        );
        let error = mux.finish().await.unwrap_err();
        assert_eq!(
            super::super::error_code(&error),
            RecordingErrorCode::SaveFailed
        );
    }

    fn encoded_samples(description: &str) -> Vec<gst::Sample> {
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
        assert!(sink.is_eos(), "Synthetic encoder failed or timed out");
        pipeline.set_state(gst::State::Null).unwrap();
        samples
    }

    #[tokio::test]
    async fn muxed_recording_decodes_video_and_non_silent_audio() {
        gst::init().unwrap();
        // Real parsers, muxer and decoders: verifies headers, timestamps and EOS
        // finalization without accessing HDMI, ALSA or the hardware encoder.
        for hevc in [false, true] {
            let encoder = if hevc {
                "x265enc tune=zerolatency speed-preset=ultrafast option-string=pools=1"
            } else {
                "openh264enc"
            };
            let video = encoded_samples(&format!("videotestsrc num-buffers=20 ! video/x-raw,width=160,height=120,framerate=25/1,format=I420 ! {encoder} ! appsink name=sink sync=false"));
            let audio = encoded_samples("audiotestsrc num-buffers=40 samplesperbuffer=960 wave=sine ! audio/x-raw,rate=48000,channels=2 ! opusenc ! appsink name=sink sync=false");
            let file = tempfile::NamedTempFile::new().unwrap();
            let mux = Mux::new(file.reopen().unwrap(), hevc).unwrap();
            let mut packets: Vec<_> = video
                .iter()
                .map(|s| (false, s))
                .chain(audio.iter().map(|s| (true, s)))
                .collect();
            packets.sort_by_key(|(_, sample)| sample.buffer().unwrap().pts());
            for (is_audio, sample) in packets {
                let buffer = sample.buffer().unwrap();
                // x265 adds an arbitrary timestamp offset; normalize each track.
                let origin = if is_audio { &audio } else { &video }[0]
                    .buffer()
                    .unwrap()
                    .pts()
                    .unwrap();
                mux.push(
                    is_audio,
                    buffer.map_readable().unwrap().as_slice(),
                    Duration::from_nanos((buffer.pts().unwrap() - origin).nseconds()),
                    Duration::from_nanos(buffer.duration().unwrap().nseconds()),
                    !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                )
                .unwrap();
            }
            mux.finish().await.unwrap();
            drop(mux);
            let decoder = if hevc {
                "h265parse ! libde265dec"
            } else {
                "h264parse ! openh264dec"
            };
            let pipeline = gst::parse::launch(&format!("filesrc name=input ! matroskademux name=d d.video_0 ! queue ! {decoder} ! fakesink name=video sync=false signal-handoffs=true d.audio_0 ! queue ! opusdec ! audioconvert ! audio/x-raw,format=S16LE ! appsink name=audio sync=false")).unwrap().downcast::<gst::Pipeline>().unwrap();
            pipeline
                .by_name("input")
                .unwrap()
                .set_property("location", file.path().to_str().unwrap());
            let video_frames = Arc::new(AtomicU64::new(0));
            let count = video_frames.clone();
            pipeline
                .by_name("video")
                .unwrap()
                .connect("handoff", false, move |_| {
                    count.fetch_add(1, Ordering::Relaxed);
                    None
                });
            let audio_energy = Arc::new(AtomicU64::new(0));
            let energy = audio_energy.clone();
            pipeline
                .by_name("audio")
                .unwrap()
                .downcast::<AppSink>()
                .unwrap()
                .set_callbacks(
                    AppSinkCallbacks::builder()
                        .new_sample(move |sink| {
                            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                            let map = sample.buffer().unwrap().map_readable().unwrap();
                            let sum: u64 = map
                                .as_slice()
                                .chunks_exact(2)
                                .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs() as u64)
                                .sum();
                            energy.fetch_add(sum, Ordering::Relaxed);
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
            pipeline.set_state(gst::State::Playing).unwrap();
            let message = pipeline
                .bus()
                .unwrap()
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(10),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                )
                .unwrap();
            pipeline.set_state(gst::State::Null).unwrap();
            assert!(
                matches!(message.view(), gst::MessageView::Eos(..)),
                "{message:?}"
            );
            assert_eq!(video_frames.load(Ordering::Relaxed), 20);
            assert!(audio_energy.load(Ordering::Relaxed) > 100_000);
        }
    }
}
