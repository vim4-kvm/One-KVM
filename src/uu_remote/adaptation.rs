//! Conservative RR fallback. Decisions require new receiver observations;
//! polling an old report must never repeatedly decrease or increase the rate.
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Observation {
    pub source: u32,
    pub sequence: u32,
    pub at: Instant,
    pub loss_percent: f64,
    pub rtt_ms: Option<f64>,
}

#[derive(Default)]
pub(super) struct Controller {
    source_sequence: Option<(u32, u32)>,
    last: Option<Instant>,
    baseline_rtt: Option<f64>,
    healthy_since: Option<Instant>,
}

impl Controller {
    pub fn update(
        &mut self,
        sample: Observation,
        now: Instant,
        current: u32,
        ceiling: u32,
    ) -> Option<u32> {
        if self
            .source_sequence
            .is_some_and(|(source, _)| source != sample.source)
        {
            *self = Self::default();
        }
        if now.duration_since(sample.at) > Duration::from_secs(10)
            || self.last.is_some_and(|last| sample.at <= last)
            || self.source_sequence.is_some_and(|(_, seq)| {
                sample.sequence.wrapping_sub(seq) == 0
                    || sample.sequence.wrapping_sub(seq) > i32::MAX as u32
            })
        {
            return None;
        }
        self.last = Some(sample.at);
        self.source_sequence = Some((sample.source, sample.sequence));
        if let Some(rtt) = sample.rtt_ms {
            self.baseline_rtt = Some(self.baseline_rtt.map_or(rtt, |base| base.min(rtt)));
        }
        let delayed = sample
            .rtt_ms
            .zip(self.baseline_rtt)
            .is_some_and(|(rtt, base)| rtt > base + 50.0 && rtt > base * 1.5);
        let next = if sample.loss_percent >= 10.0 || (sample.loss_percent >= 2.0 && delayed) {
            self.healthy_since = None;
            current.saturating_mul(80) / 100
        } else if sample.loss_percent < 1.0 && !delayed {
            let since = self.healthy_since.get_or_insert(now);
            if now.duration_since(*since) < Duration::from_secs(5) {
                return None;
            }
            *since = now;
            current.saturating_add((current / 20).max(100))
        } else {
            self.healthy_since = None;
            return None;
        }
        .clamp(1000, ceiling.max(1000));
        (next != current).then_some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_feedback_cannot_drive_repeated_reductions() {
        let now = Instant::now();
        let sample = Observation {
            source: 1,
            sequence: 1,
            at: now,
            loss_percent: 12.0,
            rtt_ms: Some(50.0),
        };
        let mut controller = Controller::default();
        assert_eq!(controller.update(sample, now, 10000, 20000), Some(8000));
        assert_eq!(
            controller.update(sample, now + Duration::from_secs(1), 8000, 20000),
            None
        );
        let mut controller = Controller::default();
        assert_eq!(
            controller.update(sample, now + Duration::from_secs(11), 8000, 20000),
            None
        );
    }
    #[test]
    fn recovery_requires_healthy_time_and_respects_ceiling() {
        let now = Instant::now();
        let mut controller = Controller::default();
        let sample = |at| Observation {
            source: 1,
            sequence: if at == now { 1 } else { 2 },
            at,
            loss_percent: 0.0,
            rtt_ms: Some(30.0),
        };
        assert_eq!(controller.update(sample(now), now, 8000, 8200), None);
        let later = now + Duration::from_secs(5);
        assert_eq!(
            controller.update(sample(later), later, 8000, 8200),
            Some(8200)
        );
    }
}
