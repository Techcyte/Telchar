//! Tracks process-local SSH and Nix readiness for pre-dispatch routing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::service::config::StaticSshBackendConfig;

use super::verify_backend;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaticSshHealthState {
    Ready,
    Unavailable,
}

#[derive(Clone)]
pub struct StaticSshHealth {
    inner: Arc<StaticSshHealthInner>,
}

struct StaticSshHealthInner {
    backends: Vec<StaticSshBackendConfig>,
    states: Mutex<BTreeMap<String, BackendHealth>>,
}

#[derive(Clone, Copy)]
struct BackendHealth {
    state: StaticSshHealthState,
    checked_at: Instant,
}

impl StaticSshHealth {
    pub fn probe_all(backends: &[StaticSshBackendConfig]) -> Self {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(30);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let states = Mutex::new(BTreeMap::new());
        std::thread::scope(|scope| {
            for _ in 0..backends.len().min(4) {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(backend) = backends.get(index) else {
                            break;
                        };
                        let started = Instant::now();
                        let remaining = deadline.saturating_duration_since(started);
                        let state = if remaining.is_zero() {
                            StaticSshHealthState::Unavailable
                        } else {
                            probe_backend(backend, backend.check_timeout().min(remaining))
                        };
                        record_probe(backend, state, started.elapsed());
                        states.lock().expect("probe state lock").insert(
                            backend.target().name().to_owned(),
                            BackendHealth {
                                state,
                                checked_at: started,
                            },
                        );
                    }
                });
            }
        });
        let states = states.into_inner().expect("probe state lock");
        let health = Self {
            inner: Arc::new(StaticSshHealthInner {
                backends: backends.to_vec(),
                states: Mutex::new(states),
            }),
        };
        health.record_metrics();
        health
    }

    pub fn is_ready(&self, backend_name: &str) -> bool {
        self.state(backend_name) == Some(StaticSshHealthState::Ready)
    }

    pub fn state(&self, backend_name: &str) -> Option<StaticSshHealthState> {
        self.inner
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(backend_name)
            .map(|health| health.state)
    }

    pub fn counts(&self) -> StaticSshHealthCounts {
        let states = self
            .inner
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut counts = StaticSshHealthCounts::default();
        for health in states.values() {
            match health.state {
                StaticSshHealthState::Ready => counts.ready += 1,
                StaticSshHealthState::Unavailable => counts.unavailable += 1,
            }
        }
        counts
    }

    pub fn check_due(&self, now: Instant) -> usize {
        self.check_due_with(
            &mut |backend| probe_backend(backend, backend.check_timeout()),
            now,
        )
    }

    #[doc(hidden)]
    pub fn from_states<'a>(
        backends: &[StaticSshBackendConfig],
        states: impl IntoIterator<Item = (&'a str, StaticSshHealthState)>,
    ) -> Self {
        let now = Instant::now();
        let states = states
            .into_iter()
            .map(|(name, state)| {
                (
                    name.to_owned(),
                    BackendHealth {
                        state,
                        checked_at: now
                            .checked_sub(Duration::from_secs(24 * 60 * 60))
                            .unwrap_or(now),
                    },
                )
            })
            .collect();
        Self {
            inner: Arc::new(StaticSshHealthInner {
                backends: backends.to_vec(),
                states: Mutex::new(states),
            }),
        }
    }

    #[doc(hidden)]
    pub fn check_due_with(
        &self,
        probe: &mut dyn FnMut(&StaticSshBackendConfig) -> StaticSshHealthState,
        now: Instant,
    ) -> usize {
        let due = {
            let states = self
                .inner
                .states
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner
                .backends
                .iter()
                .filter(|backend| {
                    states.get(backend.target().name()).is_none_or(|health| {
                        let interval = match health.state {
                            StaticSshHealthState::Ready => backend.ready_check_interval(),
                            StaticSshHealthState::Unavailable => {
                                backend.unavailable_check_interval()
                            }
                        };
                        now.saturating_duration_since(health.checked_at) >= interval
                    })
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if due.is_empty() {
            return 0;
        }
        let mut updates = Vec::with_capacity(due.len());
        for backend in &due {
            let started = Instant::now();
            let state = probe(backend);
            record_probe(backend, state, started.elapsed());
            updates.push((backend.target().name().to_owned(), state));
        }
        let mut states = self
            .inner
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (name, state) in updates {
            states.insert(
                name,
                BackendHealth {
                    state,
                    checked_at: now,
                },
            );
        }
        drop(states);
        self.record_metrics();
        due.len()
    }

    fn record_metrics(&self) {
        let counts = self.counts();
        crate::service::metrics::record_static_ssh_health(
            counts.ready as u64,
            counts.unavailable as u64,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StaticSshHealthCounts {
    pub ready: usize,
    pub unavailable: usize,
}

fn probe_backend(config: &StaticSshBackendConfig, timeout: Duration) -> StaticSshHealthState {
    match verify_backend(config, timeout) {
        Ok(()) => StaticSshHealthState::Ready,
        Err(_) => StaticSshHealthState::Unavailable,
    }
}

fn record_probe(config: &StaticSshBackendConfig, state: StaticSshHealthState, duration: Duration) {
    let state_name = match state {
        StaticSshHealthState::Ready => "ready",
        StaticSshHealthState::Unavailable => "unavailable",
    };
    crate::service::metrics::static_ssh_health_check(duration, state_name);
    tracing::info!(
        event = "backend.static_ssh.health_checked",
        backend = config.target().name(),
        system = config.target().system(),
        state = state_name,
        "static SSH backend health checked"
    );
}
