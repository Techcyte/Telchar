//! Coordinates durable subject-fair queue admission and active-execution limits.

use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::persistence::{self, SharedBuild, SharedBuildFailure, SharedBuildState};
use crate::service::config::SchedulingLimits;

const MAXIMUM_SCHEDULING_SUBJECTS: usize = 256;
const RECHECK_INTERVAL: Duration = Duration::from_millis(250);

type LimitsForSubject = dyn Fn(&str) -> SchedulingLimits + Send + Sync;

pub struct SharedBuildScheduler {
    database_url: String,
    limits_for_subject: Box<LimitsForSubject>,
    state: Mutex<SchedulerState>,
    changed: Condvar,
}

struct SchedulerState {
    last_admitted_subject: Option<String>,
}

impl SharedBuildScheduler {
    pub fn new(
        database_url: impl Into<String>,
        limits_for_subject: impl Fn(&str) -> SchedulingLimits + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let database_url = database_url.into();
        let last_admitted_subject = persistence::read_shared_build_scheduler_subject(&database_url)
            .map_err(shared_build_error)?;
        Ok(Self {
            database_url,
            limits_for_subject: Box::new(limits_for_subject),
            state: Mutex::new(SchedulerState {
                last_admitted_subject,
            }),
            changed: Condvar::new(),
        })
    }

    pub fn wait_for_admission(&self, derivation_path: &str) -> io::Result<SharedBuild> {
        let started = Instant::now();
        tracing::trace!(
            event = "shared_build.scheduler.wait.started",
            "shared build scheduler admission wait started"
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            self.admit_eligible_builds(&mut state)?;
            let build = persistence::read_shared_build(&self.database_url, derivation_path)
                .map_err(shared_build_error)?
                .ok_or_else(|| io::Error::other("queued shared build is unavailable"))?;
            match build.state {
                SharedBuildState::Running => {
                    tracing::debug!(
                        event = "shared_build.scheduler.admitted",
                        duration_ms = started.elapsed().as_millis(),
                        "shared build admitted by scheduler"
                    );
                    return Ok(build);
                }
                SharedBuildState::Claimed => {}
                SharedBuildState::Collecting
                | SharedBuildState::Succeeded
                | SharedBuildState::Failed => {
                    return Err(io::Error::other("queued shared build cannot be admitted"));
                }
            }
            let (next_state, _) = self
                .changed
                .wait_timeout(state, RECHECK_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
        }
    }

    pub fn capacity_changed(&self) {
        self.changed.notify_all();
    }

    fn admit_eligible_builds(&self, state: &mut SchedulerState) -> io::Result<()> {
        let started = Instant::now();
        let mut examined_subjects = 0;
        let mut admitted_builds = 0_usize;
        tracing::trace!(
            event = "shared_build.scheduler.examination.started",
            "shared build scheduler examination started"
        );
        let mut cursor = state.last_admitted_subject.clone();
        while examined_subjects < MAXIMUM_SCHEDULING_SUBJECTS {
            let Some(entry) = persistence::read_next_queued_shared_build(
                &self.database_url,
                cursor.as_deref(),
                MAXIMUM_SCHEDULING_SUBJECTS,
            )
            .map_err(shared_build_error)?
            else {
                tracing::trace!(
                    event = "shared_build.scheduler.examination.completed",
                    examined_subject_count = examined_subjects,
                    admitted_build_count = admitted_builds,
                    duration_ms = started.elapsed().as_millis(),
                    "shared build scheduler examination completed"
                );
                return Ok(());
            };
            if examined_subjects > 0
                && state.last_admitted_subject.as_deref() == Some(entry.quota_subject.as_str())
            {
                tracing::trace!(
                    event = "shared_build.scheduler.examination.completed",
                    examined_subject_count = examined_subjects,
                    admitted_build_count = admitted_builds,
                    duration_ms = started.elapsed().as_millis(),
                    "shared build scheduler examination completed"
                );
                return Ok(());
            }
            cursor = Some(entry.quota_subject.clone());
            examined_subjects += 1;
            let limits = (self.limits_for_subject)(&entry.quota_subject);
            match persistence::start_queued_shared_build(
                &self.database_url,
                &entry.derivation_path,
                limits.maximum_active_builds(),
            ) {
                Ok(_) => {
                    persistence::record_shared_build_scheduler_subject(
                        &self.database_url,
                        &entry.quota_subject,
                    )
                    .map_err(shared_build_error)?;
                    state.last_admitted_subject = Some(entry.quota_subject);
                    admitted_builds += 1;
                    self.changed.notify_all();
                }
                Err(error) if error.failure() == SharedBuildFailure::Quota => {}
                Err(error) if error.failure() == SharedBuildFailure::InvalidState => {}
                Err(error) => return Err(shared_build_error(error)),
            }
        }
        tracing::trace!(
            event = "shared_build.scheduler.examination.completed",
            examined_subject_count = examined_subjects,
            admitted_build_count = admitted_builds,
            duration_ms = started.elapsed().as_millis(),
            "shared build scheduler examination completed"
        );
        Ok(())
    }
}

fn shared_build_error(error: persistence::SharedBuildError) -> io::Error {
    let kind = if error.failure() == SharedBuildFailure::Quota {
        io::ErrorKind::WouldBlock
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, "shared build scheduling failed")
}
