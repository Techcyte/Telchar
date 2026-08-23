//! Coalesces equivalent in-process requests into one leader execution with bounded follower waiting.

pub mod recovery;
pub mod scheduler;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::backend::BuildResult;

pub const LIVE_LOG_TRUNCATION_MARKER: &[u8] = b"\n[telchar: earlier build logs truncated]\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedBuildTerminalFailure {
    Backend,
    BackendUnavailable,
    Internal,
}

pub enum SharedBuildAccess<'a> {
    Leader(SharedBuildLeader<'a>),
    Follower(SharedBuildFollower),
}

pub struct SharedBuildLeader<'a> {
    registry: &'a SharedBuildRegistry,
    build_key: String,
    active: Arc<ActiveBuild>,
    completed: bool,
}

impl SharedBuildLeader<'_> {
    pub fn subscribe_logs(&self, maximum_bytes: usize) -> SharedBuildLogReceiver {
        self.active.subscribe_logs(maximum_bytes)
    }

    pub fn complete(
        mut self,
        result: Result<BuildResult, SharedBuildTerminalFailure>,
    ) -> Result<BuildResult, SharedBuildTerminalFailure> {
        self.finish(result.clone());
        result
    }

    fn finish(&mut self, result: Result<BuildResult, SharedBuildTerminalFailure>) {
        {
            let mut state = self
                .active
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *state = ActiveBuildState::Completed(result);
            self.active.completed.notify_all();
        }
        let mut active_builds = self
            .registry
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active_builds.remove(&self.build_key);
        crate::service::metrics::shared_build_in_flight_finished();
        self.completed = true;
    }
}

impl Drop for SharedBuildLeader<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.finish(Err(SharedBuildTerminalFailure::Internal));
        }
    }
}

pub struct SharedBuildFollower {
    active: Arc<ActiveBuild>,
}

struct FollowerWaitGuard<'a> {
    active: &'a ActiveBuild,
    started: std::time::Instant,
    outcome: &'static str,
}

impl FollowerWaitGuard<'_> {
    fn new(active: &ActiveBuild) -> FollowerWaitGuard<'_> {
        let mut waiting = active
            .waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *waiting = waiting.saturating_add(1);
        crate::service::metrics::shared_build_follower_wait_started();
        drop(waiting);
        FollowerWaitGuard {
            active,
            started: std::time::Instant::now(),
            outcome: "timed_out",
        }
    }
}

impl Drop for FollowerWaitGuard<'_> {
    fn drop(&mut self) {
        let mut waiting = self
            .active
            .waiting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *waiting = waiting.saturating_sub(1);
        crate::service::metrics::shared_build_follower_wait_finished(
            self.started.elapsed(),
            self.outcome,
        );
    }
}

impl SharedBuildFollower {
    pub fn subscribe_logs(&self, maximum_bytes: usize) -> SharedBuildLogReceiver {
        self.active.subscribe_logs(maximum_bytes)
    }

    pub fn wait_timeout_with_logs<F>(
        self,
        timeout: std::time::Duration,
        logs: &mut SharedBuildLogReceiver,
        mut forward: F,
    ) -> std::io::Result<Option<Result<BuildResult, SharedBuildTerminalFailure>>>
    where
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        let mut wait_guard = FollowerWaitGuard::new(&self.active);
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| std::io::Error::other("shared build wait timeout is invalid"))?;
        loop {
            for chunk in logs.drain() {
                forward(&chunk)?;
            }
            if let Some(result) = self.result() {
                wait_guard.outcome = if result.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                };
                return Ok(Some(result));
            }
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return Ok(None);
            };
            let wait = remaining.min(std::time::Duration::from_millis(50));
            for chunk in logs.wait_and_drain(wait) {
                forward(&chunk)?;
            }
        }
    }

    fn result(&self) -> Option<Result<BuildResult, SharedBuildTerminalFailure>> {
        let state = self
            .active
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*state {
            ActiveBuildState::Running => None,
            ActiveBuildState::Completed(result) => Some(result.clone()),
        }
    }

    pub fn wait(self) -> Result<BuildResult, SharedBuildTerminalFailure> {
        self.wait_until(None)
            .unwrap_or(Err(SharedBuildTerminalFailure::Internal))
    }

    pub fn wait_timeout(
        self,
        timeout: std::time::Duration,
    ) -> Option<Result<BuildResult, SharedBuildTerminalFailure>> {
        self.wait_until(std::time::Instant::now().checked_add(timeout))
    }

    fn wait_until(
        self,
        deadline: Option<std::time::Instant>,
    ) -> Option<Result<BuildResult, SharedBuildTerminalFailure>> {
        let mut wait_guard = FollowerWaitGuard::new(&self.active);
        let mut state = self
            .active
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match &*state {
                ActiveBuildState::Running => {
                    if let Some(deadline) = deadline {
                        let remaining =
                            deadline.checked_duration_since(std::time::Instant::now())?;
                        let (next_state, wait) = self
                            .active
                            .completed
                            .wait_timeout(state, remaining)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state = next_state;
                        if wait.timed_out() && matches!(&*state, ActiveBuildState::Running) {
                            return None;
                        }
                    } else {
                        state = self
                            .active
                            .completed
                            .wait(state)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                }
                ActiveBuildState::Completed(result) => {
                    wait_guard.outcome = if result.is_ok() {
                        "succeeded"
                    } else {
                        "failed"
                    };
                    return Some(result.clone());
                }
            }
        }
    }
}

#[derive(Default)]
pub struct SharedBuildRegistry {
    active: Mutex<HashMap<String, Arc<ActiveBuild>>>,
}

impl SharedBuildRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn acquire(&self, build_key: &str) -> SharedBuildAccess<'_> {
        let mut active_builds = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active) = active_builds.get(build_key) {
            return SharedBuildAccess::Follower(SharedBuildFollower {
                active: Arc::clone(active),
            });
        }

        let active = Arc::new(ActiveBuild::default());
        active_builds.insert(build_key.to_owned(), Arc::clone(&active));
        crate::service::metrics::shared_build_in_flight_started();
        SharedBuildAccess::Leader(SharedBuildLeader {
            registry: self,
            build_key: build_key.to_owned(),
            active,
            completed: false,
        })
    }

    pub fn execute_or_wait<F>(
        &self,
        build_key: &str,
        execute: F,
    ) -> Result<BuildResult, SharedBuildTerminalFailure>
    where
        F: FnOnce() -> Result<BuildResult, SharedBuildTerminalFailure>,
    {
        self.execute_or_wait_with_follower(build_key, || {}, execute)
    }

    pub fn execute_or_wait_with_follower<N, F>(
        &self,
        build_key: &str,
        notify_follower: N,
        execute: F,
    ) -> Result<BuildResult, SharedBuildTerminalFailure>
    where
        N: FnOnce(),
        F: FnOnce() -> Result<BuildResult, SharedBuildTerminalFailure>,
    {
        match self.acquire(build_key) {
            SharedBuildAccess::Leader(leader) => leader.complete(execute()),
            SharedBuildAccess::Follower(follower) => {
                notify_follower();
                follower.wait()
            }
        }
    }

    pub fn subscribe_logs(
        &self,
        build_key: &str,
        maximum_bytes: usize,
    ) -> Option<SharedBuildLogReceiver> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(build_key)
            .map(|active| active.subscribe_logs(maximum_bytes))
    }

    pub fn publish_log(&self, build_key: &str, chunk: &[u8]) -> usize {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(build_key)
            .cloned();
        active.map_or(0, |active| active.publish_log(chunk))
    }

    pub fn active_build_count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn waiting_follower_count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|active| {
                *active
                    .waiting
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            })
            .sum()
    }
}

#[derive(Default)]
struct ActiveBuild {
    state: Mutex<ActiveBuildState>,
    completed: Condvar,
    waiting: Mutex<usize>,
    log_subscribers: Mutex<Vec<Weak<SharedBuildLogSubscription>>>,
}

impl ActiveBuild {
    fn subscribe_logs(&self, maximum_bytes: usize) -> SharedBuildLogReceiver {
        let queue = Arc::new(SharedBuildLogSubscription {
            queue: Mutex::new(SharedBuildLogQueue::new(maximum_bytes)),
            available: Condvar::new(),
        });
        self.log_subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::downgrade(&queue));
        SharedBuildLogReceiver { queue }
    }

    fn publish_log(&self, chunk: &[u8]) -> usize {
        let mut subscribers = self
            .log_subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.retain(|subscriber| {
            let Some(queue) = subscriber.upgrade() else {
                return false;
            };
            let dropped = queue
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(chunk);
            if dropped > 0 {
                crate::service::metrics::shared_build_live_logs_truncated(dropped);
            }
            queue.available.notify_one();
            true
        });
        subscribers.len()
    }
}

struct SharedBuildLogSubscription {
    queue: Mutex<SharedBuildLogQueue>,
    available: Condvar,
}

pub struct SharedBuildLogReceiver {
    queue: Arc<SharedBuildLogSubscription>,
}

impl SharedBuildLogReceiver {
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        self.queue
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
    }

    pub fn wait_and_drain(&mut self, timeout: std::time::Duration) -> Vec<Vec<u8>> {
        let queue = self
            .queue
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut queue = if queue.is_empty() {
            self.queue
                .available
                .wait_timeout(queue, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0
        } else {
            queue
        };
        queue.drain()
    }
}

struct SharedBuildLogQueue {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    maximum_bytes: usize,
    truncated: bool,
}

impl SharedBuildLogQueue {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            maximum_bytes,
            truncated: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.chunks.is_empty() && !self.truncated
    }

    fn push(&mut self, chunk: &[u8]) -> u64 {
        let mut dropped_bytes = 0_u64;
        if self.maximum_bytes == 0 {
            self.truncated = true;
            return chunk.len() as u64;
        }
        while self.bytes.saturating_add(chunk.len()) > self.maximum_bytes {
            let Some(dropped) = self.chunks.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(dropped.len());
            dropped_bytes = dropped_bytes.saturating_add(dropped.len() as u64);
            self.truncated = true;
        }
        if chunk.len() > self.maximum_bytes {
            self.truncated = true;
            return dropped_bytes.saturating_add(chunk.len() as u64);
        }
        self.bytes = self.bytes.saturating_add(chunk.len());
        self.chunks.push_back(chunk.to_vec());
        dropped_bytes
    }

    fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut chunks = Vec::with_capacity(self.chunks.len() + usize::from(self.truncated));
        if self.truncated {
            chunks.push(LIVE_LOG_TRUNCATION_MARKER.to_vec());
            self.truncated = false;
        }
        chunks.extend(self.chunks.drain(..));
        self.bytes = 0;
        chunks
    }
}

#[derive(Default)]
enum ActiveBuildState {
    #[default]
    Running,
    Completed(Result<BuildResult, SharedBuildTerminalFailure>),
}
