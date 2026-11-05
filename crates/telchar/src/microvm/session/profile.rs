//! Bounded per-assignment totals. Monitor and transfer intervals may overlap.
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STAGES: [&str; 5] = [
    "load",
    "ownership",
    "request_connection",
    "request_query",
    "request_decode_validate",
];

#[derive(Clone, Copy, Default)]
struct Timing {
    count: u64,
    total: Duration,
    maximum: Duration,
}

#[derive(Default)]
pub(super) struct AuthorityProfile(Mutex<[[Timing; 5]; 2]>);

impl AuthorityProfile {
    pub(super) fn observe(&self, monitor: bool, stage: &'static str, elapsed: Duration) {
        let index = STAGES
            .iter()
            .position(|name| *name == stage)
            .expect("fixed authority stage");
        let mut timings = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let timing = &mut timings[usize::from(monitor)][index];
        timing.count += 1;
        timing.total += elapsed;
        timing.maximum = timing.maximum.max(elapsed);
    }

    pub(super) fn measure<T>(
        &self,
        monitor: bool,
        stage: &'static str,
        operation: impl FnOnce() -> T,
    ) -> T {
        let started = Instant::now();
        let result = operation();
        self.observe(monitor, stage, started.elapsed());
        result
    }
}

impl Drop for AuthorityProfile {
    fn drop(&mut self) {
        let timings = self.0.lock().unwrap_or_else(|error| error.into_inner());
        for (lane, stages) in timings.iter().enumerate() {
            for (stage, timing) in STAGES.iter().zip(stages) {
                tracing::info!(
                    event = "microvm.authority.summary",
                    stage,
                    source = if lane == 0 { "transfer" } else { "monitor" },
                    overlapping_intervals = true,
                    count = i64::try_from(timing.count).unwrap_or(i64::MAX),
                    total_ms = timing.total.as_secs_f64() * 1000.0,
                    maximum_ms = timing.maximum.as_secs_f64() * 1000.0,
                );
            }
        }
    }
}
