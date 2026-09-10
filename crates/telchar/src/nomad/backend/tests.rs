//! Checks monitoring deadlines independently of log notifications and HTTP duration.

use super::{NomadAllocation, NomadSubmission, StatusPoll};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
struct EventCapture(Arc<Mutex<Vec<(tracing::Level, String)>>>);

impl<S> Layer<S> for EventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .expect("events lock")
            .push((*event.metadata().level(), fields.0));
    }
}

#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&format!("{value:?}"));
    }
}

#[test]
fn dispatched_build_log_correlates_nix_and_nomad_identity() {
    let captured = EventCapture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(captured.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        super::log_dispatched_build(
            "/nix/store/example.drv",
            "job-a",
            "evaluation-a",
            "backend-a",
            "namespace-a",
            2,
            "gpu",
            "x86_64-linux",
            &["benchmark".to_owned(), "cuda".to_owned()],
        );
    });

    let events = captured.0.lock().expect("events lock");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, tracing::Level::INFO);
    for field in [
        "event=\"nomad.build.dispatched\"",
        "derivation_path=\"/nix/store/example.drv\"",
        "job_id=\"job-a\"",
        "evaluation_id=\"evaluation-a\"",
        "backend=\"backend-a\"",
        "namespace=\"namespace-a\"",
        "attempt_ordinal=2",
        "resource_profile=\"gpu\"",
        "system=\"x86_64-linux\"",
        "required_features=[\"benchmark\", \"cuda\"]",
    ] {
        assert!(
            events[0].1.contains(field),
            "missing {field}: {}",
            events[0].1
        );
    }
}

#[test]
fn placed_build_log_identifies_allocation_node() {
    let captured = EventCapture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(captured.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        super::log_placed_build(
            "/nix/store/example.drv",
            &NomadSubmission {
                job_id: "job-a".to_owned(),
                evaluation_id: "evaluation-a".to_owned(),
            },
            &NomadAllocation {
                id: "allocation-a".to_owned(),
                node_name: Some("worker-a".to_owned()),
            },
            "backend-a",
            "namespace-a",
            1,
        );
    });

    let events = captured.0.lock().expect("events lock");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, tracing::Level::INFO);
    for field in [
        "event=\"nomad.build.placed\"",
        "derivation_path=\"/nix/store/example.drv\"",
        "job_id=\"job-a\"",
        "evaluation_id=\"evaluation-a\"",
        "allocation_id=\"allocation-a\"",
        "node_name=\"worker-a\"",
    ] {
        assert!(
            events[0].1.contains(field),
            "missing {field}: {}",
            events[0].1
        );
    }
}

#[test]
fn log_wakes_do_not_advance_status_poll() {
    let start = Instant::now();
    let interval = Duration::from_secs(2);
    let mut poll = StatusPoll::new(start, interval);
    assert!(poll.due(start));
    poll.completed(start);
    for milliseconds in [0, 1, 50, 500, 1999] {
        let now = start + Duration::from_millis(milliseconds);
        assert!(!poll.due(now));
        assert_eq!(
            poll.wait(now, start + interval * 10),
            interval - Duration::from_millis(milliseconds)
        );
    }
    assert!(poll.due(start + interval));
}

#[test]
fn slow_status_request_does_not_create_catch_up_polls() {
    let start = Instant::now();
    let interval = Duration::from_secs(2);
    let mut poll = StatusPoll::new(start, interval);
    let completed = start + interval * 4;
    poll.completed(completed);
    assert!(!poll.due(completed));
    assert_eq!(poll.wait(completed, completed + interval * 10), interval);
    assert!(poll.due(completed + interval));
}

#[test]
fn execution_deadline_caps_wait() {
    let start = Instant::now();
    let mut poll = StatusPoll::new(start, Duration::from_secs(2));
    assert_eq!(
        poll.wait(start, start + Duration::from_secs(1)),
        Duration::ZERO
    );
    poll.completed(start);
    let deadline = start + Duration::from_millis(100);
    assert_eq!(poll.wait(start, deadline), Duration::from_millis(100));
    assert_eq!(poll.wait(deadline, deadline), Duration::ZERO);
    assert_eq!(
        poll.wait(deadline + Duration::from_secs(1), deadline),
        Duration::ZERO
    );
}
