//! Fixed-memory audio statistics. VIM4 capture reports once per minute by default;
//! ONEKVM_AUDIO_STATS=1 enables five-second continuity reports at additional stages.
//! No PCM or packet payloads are retained. Levels and repeated-block counts do
//! not establish whether an arbitrary source is supposed to be playing sound.
use std::time::{Duration, Instant};

pub(crate) fn enabled() -> bool {
    std::env::var_os("ONEKVM_AUDIO_STATS").is_some_and(|value| value == "1")
}

pub(crate) struct PcmStats {
    stage: &'static str,
    rate: u32,
    detailed: bool,
    interval: Duration,
    period_frames: Option<u64>,
    buffer_frames: Option<u64>,
    previous: [Option<i16>; 2],
    start: Instant,
    last: Instant,
    frames: u64,
    max_step: u32,
    steps_over_500: u64,
    max_gap_us: u128,
    samples: u64,
    nonzero_samples: u64,
    peak: u32,
    sum: i64,
    sum_squares: u64,
    last_peak: u32,
    last_rms: f64,
    previous_hash: Option<u64>,
    blocks: u64,
    repeated_blocks: u64,
    pub xruns: u64,
    pub eagain: u64,
}

impl PcmStats {
    pub fn new(stage: &'static str, rate: u32) -> Option<Self> {
        let detailed = enabled();
        (detailed || (cfg!(feature = "vim4") && stage == "native"))
            .then(|| Self::for_mode(stage, rate, detailed))
    }

    fn for_mode(stage: &'static str, rate: u32, detailed: bool) -> Self {
        Self {
            stage,
            rate,
            detailed,
            interval: Duration::from_secs(if detailed { 5 } else { 60 }),
            period_frames: None,
            buffer_frames: None,
            previous: [None; 2],
            start: Instant::now(),
            last: Instant::now(),
            frames: 0,
            max_step: 0,
            steps_over_500: 0,
            max_gap_us: 0,
            samples: 0,
            nonzero_samples: 0,
            peak: 0,
            sum: 0,
            sum_squares: 0,
            last_peak: 0,
            last_rms: 0.0,
            previous_hash: None,
            blocks: 0,
            repeated_blocks: 0,
            xruns: 0,
            eagain: 0,
        }
    }

    pub fn set_pcm_geometry(&mut self, period_frames: u64, buffer_frames: u64) {
        self.period_frames = Some(period_frames);
        self.buffer_frames = Some(buffer_frames);
    }

    pub fn tick(&mut self) {
        self.report_if_due(Instant::now());
    }

    pub fn observe(&mut self, data: &[u8]) {
        let now = Instant::now();
        self.max_gap_us = self
            .max_gap_us
            .max(now.duration_since(self.last).as_micros());
        self.last = now;
        let mut block_squares = 0u64;
        let mut block_peak = 0;
        let mut block_samples = 0;
        for frame in data.chunks_exact(4) {
            for (channel, bytes) in frame.chunks_exact(2).enumerate() {
                let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
                let value = i64::from(sample);
                let magnitude = value.unsigned_abs() as u32;
                self.nonzero_samples += u64::from(sample != 0);
                self.sum += value;
                block_samples += 1;
                block_squares += (value * value) as u64;
                block_peak = block_peak.max(magnitude);
                if self.detailed {
                    if let Some(previous) = self.previous[channel] {
                        let step = (i32::from(sample) - i32::from(previous)).unsigned_abs();
                        self.max_step = self.max_step.max(step);
                        self.steps_over_500 += u64::from(step > 500);
                    }
                    self.previous[channel] = Some(sample);
                }
            }
            self.frames += 1;
        }
        if block_samples != 0 {
            self.samples += block_samples;
            self.sum_squares += block_squares;
            self.peak = self.peak.max(block_peak);
            self.last_peak = block_peak;
            self.last_rms = (block_squares as f64 / block_samples as f64).sqrt();
            // Only retain a fingerprint of the preceding block; never log it.
            let hash = xxhash_rust::xxh64::xxh64(data, 0);
            self.repeated_blocks += u64::from(self.previous_hash == Some(hash));
            self.previous_hash = Some(hash);
            self.blocks += 1;
        }
        self.report_if_due(now);
    }

    fn report_if_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.start) < self.interval {
            return false;
        }
        self.report(now);
        true
    }

    fn report(&mut self, now: Instant) {
        let rms = (self.sum_squares as f64 / self.samples.max(1) as f64).sqrt();
        tracing::info!(
            stage = self.stage,
            rate = self.rate,
            frames = self.frames,
            elapsed_ms = now.duration_since(self.start).as_millis() as u64,
            period_frames = ?self.period_frames,
            buffer_frames = ?self.buffer_frames,
            samples = self.samples,
            nonzero_samples = self.nonzero_samples,
            peak_s16 = self.peak,
            rms_s16 = rms,
            mean_s16 = self.sum as f64 / self.samples.max(1) as f64,
            last_peak_s16 = self.last_peak,
            last_rms_s16 = self.last_rms,
            blocks = self.blocks,
            repeated_blocks = self.repeated_blocks,
            detailed = self.detailed,
            max_step = self.max_step,
            steps_over_500 = self.steps_over_500,
            max_gap_us = self.max_gap_us as u64,
            xruns = self.xruns,
            eagain = self.eagain,
            "audio continuity"
        );
        self.start = now;
        self.frames = 0;
        self.max_step = 0;
        self.steps_over_500 = 0;
        self.max_gap_us = 0;
        self.samples = 0;
        self.nonzero_samples = 0;
        self.peak = 0;
        self.sum = 0;
        self.sum_squares = 0;
        self.last_peak = 0;
        self.last_rms = 0.0;
        self.blocks = 0;
        self.repeated_blocks = 0;
        self.xruns = 0;
        self.eagain = 0;
    }
}

impl Drop for PcmStats {
    fn drop(&mut self) {
        // Normal capture restarts must not generate extra partial-window logs.
        if self.detailed {
            self.report(Instant::now());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[test]
    fn pcm_levels_handle_full_scale_and_distinguish_last_block_from_window() {
        let mut stats = PcmStats::for_mode("native", 48000, false);
        let samples = [i16::MIN, i16::MAX, 0, -1];
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        stats.observe(&bytes);
        assert_eq!(stats.peak, 32768);
        assert_eq!(stats.nonzero_samples, 3);
        assert_eq!(stats.sum, -2);
        assert_eq!(stats.sum_squares, 2_147_418_114);
        assert!(stats.last_rms > 23000.0 && stats.last_rms < 23200.0);

        stats.observe(&[0; 8]);
        assert_eq!(stats.peak, 32768);
        assert_eq!(stats.last_peak, 0);
        assert_eq!(stats.last_rms, 0.0);
        stats.observe(&[0; 8]);
        assert_eq!(stats.repeated_blocks, 1);
        assert_eq!(stats.samples, 12);
    }

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn normal_capture_logs_once_per_minute_without_restart_or_polling_bursts() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = LogWriter(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut stats = PcmStats::for_mode("native", 48000, false);
            let start = stats.start;
            stats.observe(&[0; 8]);
            let mut reports = 0;
            // Polling at 200 Hz, including periods with no new PCM, must not flood logs.
            for tick in 0..=24_000 {
                reports +=
                    usize::from(stats.report_if_due(start + Duration::from_millis(tick * 5)));
            }
            assert_eq!(reports, 2);
            assert_eq!(stats.samples, 0);
            assert_eq!(stats.sum_squares, 0);
            for _ in 0..20 {
                let mut restarted = PcmStats::for_mode("native", 48000, false);
                restarted.observe(&[0; 8]);
            }
        });
        let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(text.matches("audio continuity").count(), 2);
    }
}
