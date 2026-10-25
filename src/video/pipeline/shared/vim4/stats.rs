use super::*;

#[derive(Default)]
struct Stage {
    entered: u64,
    completed: u64,
    started: Option<Instant>,
    total_us: u64,
    max_us: u64,
    peak_active: u64,
    overlaps: u64,
    histogram: [[u64; 32]; 4],
}

impl Stage {
    fn enter(&mut self, now: Instant, overlapping: bool) {
        self.entered += 1;
        self.peak_active = self.peak_active.max(self.entered - self.completed);
        self.overlaps += u64::from(overlapping);
        self.started = Some(now);
    }

    fn complete(&mut self, now: Instant) {
        self.completed += 1;
        if let Some(started) = self.started.take() {
            let elapsed = now.duration_since(started).as_micros() as u64;
            self.total_us += elapsed;
            self.max_us = self.max_us.max(elapsed);
            let bucket = (elapsed / 500).min(127) as usize;
            self.histogram[bucket / 32][bucket % 32] += 1;
        }
    }

    fn percentile_upper_us(&self, percent: u64) -> Option<u64> {
        let total: u64 = self.histogram.iter().flatten().sum();
        if total == 0 {
            return None;
        }
        let target = (total * percent).div_ceil(100);
        let mut cumulative = 0;
        for (bucket, count) in self.histogram.iter().flatten().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return (bucket < 127).then_some((bucket as u64 + 1) * 500);
            }
        }
        None
    }
}

#[derive(Default)]
struct Counters {
    captured: u64,
    stages: [Stage; 2],
}

pub(super) struct VideoStats {
    counters: Arc<ParkingMutex<Counters>>,
    previous: ParkingMutex<(Instant, [u64; 5])>,
}

impl VideoStats {
    pub(super) fn attach(pipeline: &gst::Pipeline) -> Result<Self> {
        let counters = Arc::new(ParkingMutex::new(Counters::default()));
        let source = pipeline
            .by_name("source")
            .and_then(|element| element.static_pad("src"))
            .ok_or_else(|| AppError::VideoError("Missing capture statistics pad".into()))?;
        let captured = counters.clone();
        source.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            captured.lock().captured += 1;
            gst::PadProbeReturn::Ok
        });
        for (index, name) in ["scaler", "encoder"].iter().enumerate() {
            let Some(element) = pipeline.by_name(name) else {
                continue;
            };
            for pad_name in ["sink", "src"] {
                let pad = element.static_pad(pad_name).ok_or_else(|| {
                    AppError::VideoError(format!("Missing {name} statistics pad"))
                })?;
                let counters = counters.clone();
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    let now = Instant::now();
                    let mut counters = counters.lock();
                    if pad_name == "sink" {
                        let overlapping = counters.stages[1 - index].started.is_some();
                        counters.stages[index].enter(now, overlapping);
                    } else {
                        counters.stages[index].complete(now);
                    }
                    gst::PadProbeReturn::Ok
                });
            }
        }
        Ok(Self {
            counters,
            previous: ParkingMutex::new((Instant::now(), [0; 5])),
        })
    }

    pub(super) fn report(&self, pipeline: &gst::Pipeline) {
        let now = Instant::now();
        let mut previous = self.previous.lock();
        let mut counters = self.counters.lock();
        let seconds = now.duration_since(previous.0).as_secs_f64();
        let counts = [
            counters.captured,
            counters.stages[0].entered,
            counters.stages[0].completed,
            counters.stages[1].entered,
            counters.stages[1].completed,
        ];
        let rates = std::array::from_fn::<_, 5, _>(|index| {
            (counts[index] - previous.1[index]) as f64 / seconds
        });
        let rate_dropped = pipeline
            .by_name("output_rate")
            .map(|rate| rate.property::<u64>("drop"));
        let encoder_caps = pipeline
            .by_name("encoder")
            .and_then(|encoder| encoder.static_pad("sink"))
            .and_then(|pad| pad.current_caps());
        info!(
            capture_fps = rates[0],
            scale_in_fps = rates[1],
            scale_out_fps = rates[2],
            encoder_in_fps = rates[3],
            encoder_out_fps = rates[4],
            ?rate_dropped,
            captured = counts[0],
            encoder_in = counts[3],
            encoder_out = counts[4],
            ?encoder_caps,
            "VIM4 video rates"
        );
        for (index, name) in ["scale", "encode"].iter().enumerate() {
            let stage = &mut counters.stages[index];
            let completed = counts[2 + index * 2] - previous.1[2 + index * 2];
            info!(
                stage = name,
                completed,
                mean_us = stage.total_us.checked_div(completed),
                max_us = stage.max_us,
                peak_active = stage.peak_active,
                overlaps = stage.overlaps,
                p50_upper_us = stage.percentile_upper_us(50),
                p95_upper_us = stage.percentile_upper_us(95),
                p99_upper_us = stage.percentile_upper_us(99),
                histogram_overflow = stage.histogram[3][31],
                "VIM4 video stage"
            );
            stage.total_us = 0;
            stage.max_us = 0;
            stage.peak_active = stage.entered - stage.completed;
            stage.overlaps = 0;
            stage.histogram = [[0; 32]; 4];
        }
        *previous = (now, counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_timing_survives_a_reporting_boundary() {
        let mut stage = Stage::default();
        let start = Instant::now();
        stage.enter(start, true);
        stage.total_us = 0;
        stage.max_us = 0;
        stage.complete(start + Duration::from_micros(1234));
        assert_eq!(stage.entered, 1);
        assert_eq!(stage.completed, 1);
        assert_eq!(stage.total_us, 1234);
        assert_eq!(stage.max_us, 1234);
        assert_eq!(stage.overlaps, 1);
        assert!(stage.started.is_none());
    }

    #[test]
    fn histogram_does_not_report_an_overflowing_tail_as_a_fast_sample() {
        let mut stage = Stage::default();
        assert_eq!(stage.percentile_upper_us(99), None);
        let start = Instant::now();
        for elapsed in [100, 250, 750, 1250, 35_000, 70_000] {
            stage.enter(start, false);
            stage.complete(start + Duration::from_micros(elapsed));
        }
        assert_eq!(stage.percentile_upper_us(50), Some(1000));
        assert_eq!(stage.percentile_upper_us(99), None);
        assert_eq!(stage.max_us, 70_000);
    }
}
