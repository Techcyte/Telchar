use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::backend::BuildResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedBuildTerminalFailure {
    Backend,
    Internal,
}

#[derive(Default)]
pub struct SharedBuildRegistry {
    active: Mutex<HashMap<String, Arc<ActiveBuild>>>,
}

impl SharedBuildRegistry {
    pub fn new() -> Self {
        Self::default()
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
        let (active, leader) = {
            let mut active_builds = self
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(active) = active_builds.get(build_key) {
                (Arc::clone(active), false)
            } else {
                let active = Arc::new(ActiveBuild::default());
                active_builds.insert(build_key.to_owned(), Arc::clone(&active));
                (active, true)
            }
        };

        if leader {
            let result = execute();
            {
                let mut state = active
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *state = ActiveBuildState::Completed(result.clone());
                active.completed.notify_all();
            }
            let mut active_builds = self
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            active_builds.remove(build_key);
            result
        } else {
            notify_follower();
            let mut state = active
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                match &*state {
                    ActiveBuildState::Running => {
                        state = active
                            .completed
                            .wait(state)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                    ActiveBuildState::Completed(result) => return result.clone(),
                }
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
