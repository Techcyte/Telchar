//! Reports bounded phase progress without coupling telemetry to worker I/O.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_secs(10);

struct Cadence {
    next: Instant,
}

impl Cadence {
    fn new(now: Instant) -> Self {
        Self {
            next: now + INTERVAL,
        }
    }

    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + INTERVAL;
        true
    }
}

pub struct Progress {
    stop: mpsc::Sender<()>,
    reporter: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(report: impl Fn(u64) + Send + 'static) -> std::io::Result<Self> {
        let (stop, receiver) = mpsc::channel();
        let span = tracing::Span::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let reporter = std::thread::Builder::new()
            .name("telemetry-progress".to_owned())
            .spawn(move || {
                let _dispatch = tracing::dispatcher::set_default(&dispatch);
                let _entered = span.enter();
                let started = Instant::now();
                let mut cadence = Cadence::new(started);
                loop {
                    match receiver
                        .recv_timeout(cadence.next.saturating_duration_since(Instant::now()))
                    {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if cadence.due(Instant::now()) {
                                report(started.elapsed().as_millis() as u64);
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            reporter: Some(reporter),
        })
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(reporter) = self.reporter.take() {
            let _ = reporter.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn progress_is_periodic_without_catch_up_bursts() {
        let now = Instant::now();
        let mut cadence = Cadence::new(now);
        assert!(!cadence.due(now));
        assert!(!cadence.due(now + Duration::from_secs(9)));
        assert!(cadence.due(now + Duration::from_secs(10)));
        assert!(!cadence.due(now + Duration::from_secs(10)));
        assert!(cadence.due(now + Duration::from_secs(100)));
        assert!(!cadence.due(now + Duration::from_secs(100)));
        assert!(!cadence.due(now + Duration::from_secs(109)));
        assert!(cadence.due(now + Duration::from_secs(110)));
    }

    #[test]
    fn finishing_phase_joins_reporter_without_waiting_for_interval() {
        let mut progress = Progress::start(|_| {}).expect("reporter starts");
        let reporter = progress.reporter.take().expect("reporter exists");
        drop(progress);
        reporter.join().expect("stopping releases reporter");
    }
}
