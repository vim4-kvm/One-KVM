//! Bounded software resampling after native HDMI DMA capture. ALSA rate can
//! add interpolation frames to its slave ring, which T7 cannot capture safely.
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};

use crate::error::{AppError, Result};

fn error(value: impl std::fmt::Display) -> AppError {
    AppError::AudioError(format!("HDMI resampler: {value}"))
}

pub(super) struct HdmiResampler {
    pipeline: gst::Pipeline,
    input: AppSrc,
    output: AppSink,
    input_rate: u32,
    input_frames: u64,
    queued_limit: u64,
}

impl Drop for HdmiResampler {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl HdmiResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Option<Self>> {
        if input_rate == output_rate {
            return Ok(None);
        }
        gst::init().map_err(error)?;
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=input format=time is-live=true block=false \
             caps=audio/x-raw,format=S16LE,layout=interleaved,channels=2,rate={input_rate} \
             ! audioresample quality=8 \
             ! audio/x-raw,format=S16LE,layout=interleaved,channels=2,rate={output_rate} \
             ! appsink name=output sync=false async=false max-buffers=8"
        ))
        .map_err(error)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| error("expected pipeline"))?;
        let input = pipeline
            .by_name("input")
            .unwrap()
            .downcast::<AppSrc>()
            .unwrap();
        let output = pipeline
            .by_name("output")
            .unwrap()
            .downcast::<AppSink>()
            .unwrap();
        let resampler = Self {
            pipeline,
            input,
            output,
            input_rate,
            input_frames: 0,
            queued_limit: u64::from(input_rate) * 4 / 2,
        };
        resampler
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(error)?;
        Ok(Some(resampler))
    }

    pub fn convert(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() % 4 != 0 {
            return Err(error("partial stereo PCM frame"));
        }
        // Never block the capture thread on a wedged converter or grow its
        // input queue indefinitely. Recovery creates a fresh pipeline.
        if self.input.current_level_bytes() > self.queued_limit {
            return Err(error("input stalled for over half a second"));
        }
        let nanos =
            |frames| (u128::from(frames) * 1_000_000_000 / u128::from(self.input_rate)) as u64;
        let start = nanos(self.input_frames);
        self.input_frames += (data.len() / 4) as u64;
        let end = nanos(self.input_frames);
        let mut buffer = gst::Buffer::from_mut_slice(data.to_vec());
        let metadata = buffer.get_mut().unwrap();
        metadata.set_pts(gst::ClockTime::from_nseconds(start));
        metadata.set_duration(gst::ClockTime::from_nseconds(end - start));
        self.input.push_buffer(buffer).map_err(error)?;
        self.collect(gst::ClockTime::from_mseconds(5))
    }

    fn collect(&self, mut wait: gst::ClockTime) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        while let Some(sample) = self.output.try_pull_sample(wait) {
            let buffer = sample.buffer().ok_or_else(|| error("missing PCM buffer"))?;
            let map = buffer.map_readable().map_err(error)?;
            data.extend_from_slice(map.as_slice());
            wait = gst::ClockTime::ZERO;
        }
        if let Some(message) = self
            .pipeline
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
        {
            if let gst::MessageView::Error(failure) = message.view() {
                return Err(error(failure.error()));
            }
        }
        if self.output.is_eos() {
            return Err(error("unexpected end of capture"));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_native_rates_keep_pitch_and_continuity_across_partial_reads() {
        assert!(HdmiResampler::new(48000, 48000).unwrap().is_none());
        for rate in [32000, 44100, 88200, 96000, 176400, 192000] {
            let mut resampler = HdmiResampler::new(rate, 48000).unwrap().unwrap();
            let mut output = Vec::new();
            let mut offset = 0;
            while offset < rate * 2 {
                let count = (rate / 50 + offset % 17).min(rate * 2 - offset);
                let mut input = Vec::new();
                for i in offset..offset + count {
                    let sample = (3277.0
                        * (std::f64::consts::TAU * 440.0 * f64::from(i) / f64::from(rate)).sin())
                        as i16;
                    input.extend_from_slice(&sample.to_le_bytes());
                    input.extend_from_slice(&(-sample).to_le_bytes());
                }
                output.extend(resampler.convert(&input).unwrap());
                offset += count;
            }
            resampler.input.end_of_stream().unwrap();
            // EOS is expected only in this finite test, never during capture.
            while let Some(sample) = resampler
                .output
                .try_pull_sample(gst::ClockTime::from_mseconds(100))
            {
                output
                    .extend_from_slice(sample.buffer().unwrap().map_readable().unwrap().as_slice());
            }
            // The finite filter tail can end one output sample before the
            // nominal duration; this bound must not grow with input chunks.
            assert!((output.len() / 4).abs_diff(96000) <= 1, "rate {rate}");
            let left: Vec<_> = output
                .chunks_exact(4)
                .map(|x| i16::from_le_bytes([x[0], x[1]]))
                .collect();
            let steady = &left[4800..91200];
            assert!(
                steady
                    .windows(2)
                    .all(|x| (i32::from(x[1]) - i32::from(x[0])).abs() < 300),
                "rate {rate}"
            );
            let crossings = steady.windows(2).filter(|x| x[0] < 0 && x[1] >= 0).count();
            assert!((crossings as f64 / 1.8 - 440.0).abs() < 1.0, "rate {rate}");
            for frame in output[4800 * 4..91200 * 4].chunks_exact(4) {
                let a = i32::from(i16::from_le_bytes([frame[0], frame[1]]));
                let b = i32::from(i16::from_le_bytes([frame[2], frame[3]]));
                assert!((a + b).abs() <= 2, "stereo channels mixed at {rate}");
            }
        }
    }

    #[test]
    fn rejects_partial_pcm_frames_without_advancing_the_clock() {
        let mut resampler = HdmiResampler::new(32000, 48000).unwrap().unwrap();
        assert!(resampler.convert(&[0, 0, 0]).is_err());
        assert_eq!(resampler.input_frames, 0);
    }
}
