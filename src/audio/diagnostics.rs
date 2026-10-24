//! Opt-in, fixed-memory audio continuity statistics. No PCM or packet payloads
//! are retained. Delta counts are measurements, not a verdict for arbitrary audio.
use std::time::{Duration, Instant};

pub(crate) fn enabled() -> bool {
    std::env::var_os("ONEKVM_AUDIO_STATS").is_some_and(|value| value == "1")
}

pub(crate) struct PcmStats {
    stage: &'static str,
    rate: u32,
    previous: [Option<i16>; 2],
    start: Instant,
    last: Instant,
    frames: u64,
    max_step: u32,
    steps_over_500: u64,
    max_gap_us: u128,
    pub xruns: u64,
    pub eagain: u64,
}

impl PcmStats {
    pub fn new(stage: &'static str, rate: u32) -> Option<Self> {
        enabled().then(|| Self {
            stage,
            rate,
            previous: [None; 2],
            start: Instant::now(),
            last: Instant::now(),
            frames: 0,
            max_step: 0,
            steps_over_500: 0,
            max_gap_us: 0,
            xruns: 0,
            eagain: 0,
        })
    }

    pub fn observe(&mut self, data: &[u8]) {
        let now = Instant::now();
        self.max_gap_us = self
            .max_gap_us
            .max(now.duration_since(self.last).as_micros());
        self.last = now;
        for frame in data.chunks_exact(4) {
            for (channel, bytes) in frame.chunks_exact(2).enumerate() {
                let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
                if let Some(previous) = self.previous[channel] {
                    let step = (i32::from(sample) - i32::from(previous)).unsigned_abs();
                    self.max_step = self.max_step.max(step);
                    self.steps_over_500 += u64::from(step > 500);
                }
                self.previous[channel] = Some(sample);
            }
            self.frames += 1;
        }
        if self.start.elapsed() >= Duration::from_secs(5) {
            self.report();
        }
    }

    fn report(&mut self) {
        tracing::info!(
            stage = self.stage,
            rate = self.rate,
            frames = self.frames,
            elapsed_ms = self.start.elapsed().as_millis() as u64,
            max_step = self.max_step,
            steps_over_500 = self.steps_over_500,
            max_gap_us = self.max_gap_us as u64,
            xruns = self.xruns,
            eagain = self.eagain,
            "audio continuity"
        );
        self.start = Instant::now();
        self.frames = 0;
        self.max_step = 0;
        self.steps_over_500 = 0;
        self.max_gap_us = 0;
        self.xruns = 0;
        self.eagain = 0;
    }
}

impl Drop for PcmStats {
    fn drop(&mut self) {
        self.report();
    }
}

pub(crate) struct SendStats {
    start: Instant,
    last: Instant,
    frames: u64,
    dropped: u64,
    errors: u64,
    max_age_us: u128,
    max_write_us: u128,
    max_gap_us: u128,
}

impl SendStats {
    pub fn new() -> Option<Self> {
        enabled().then(|| Self {
            start: Instant::now(),
            last: Instant::now(),
            frames: 0,
            dropped: 0,
            errors: 0,
            max_age_us: 0,
            max_write_us: 0,
            max_gap_us: 0,
        })
    }

    pub fn observe(&mut self, encoded: Instant, started: Instant, dropped: u16, failed: bool) {
        self.frames += 1;
        self.dropped += u64::from(dropped);
        self.errors += u64::from(failed);
        self.max_age_us = self
            .max_age_us
            .max(started.saturating_duration_since(encoded).as_micros());
        self.max_write_us = self.max_write_us.max(started.elapsed().as_micros());
        self.max_gap_us = self
            .max_gap_us
            .max(started.saturating_duration_since(self.last).as_micros());
        self.last = started;
        if self.start.elapsed() >= Duration::from_secs(5) {
            tracing::info!(
                frames = self.frames,
                dropped = self.dropped,
                errors = self.errors,
                elapsed_ms = self.start.elapsed().as_millis() as u64,
                max_age_us = self.max_age_us as u64,
                max_write_us = self.max_write_us as u64,
                max_gap_us = self.max_gap_us as u64,
                "audio RTP continuity"
            );
            self.start = Instant::now();
            self.frames = 0;
            self.dropped = 0;
            self.errors = 0;
            self.max_age_us = 0;
            self.max_write_us = 0;
            self.max_gap_us = 0;
        }
    }
}
