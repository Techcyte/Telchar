use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::backend::BuildResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedBuildTerminalFailure {
    Backend,
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

impl SharedBuildFollower {
    pub fn wait(self) -> Result<BuildResult, SharedBuildTerminalFailure> {
        let mut state = self
            .active
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match &*state {
                ActiveBuildState::Running => {
                    state = self
                        .active
                        .completed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                ActiveBuildState::Completed(result) => return result.clone(),
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

    pub fn active_build_count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[derive(Default)]
struct ActiveBuild {
    state: Mutex<ActiveBuildState>,
    completed: Condvar,
}

#[derive(Default)]
enum ActiveBuildState {
    #[default]
    Running,
    Completed(Result<BuildResult, SharedBuildTerminalFailure>),
}
