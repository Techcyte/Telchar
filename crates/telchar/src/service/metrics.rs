//! Defines bounded-cardinality OpenTelemetry instruments for gateway operations.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::{global, KeyValue};

struct Instruments {
    service_sessions: Gauge<u64>,
    service_session_limit: Gauge<u64>,
    build_requests: Counter<u64>,
    build_request_duration: Histogram<f64>,
    build_executions: Counter<u64>,
    build_execution_duration: Histogram<f64>,
    build_output_count: Histogram<u64>,
    shared_build_leaders: Counter<u64>,
    shared_build_followers: Counter<u64>,
    shared_build_reused_results: Counter<u64>,
    shared_build_queue_depth: Gauge<u64>,
    shared_build_queue_wait_duration: Histogram<f64>,
    shared_build_queue_admissions: Counter<u64>,
    backend_permits_active: Gauge<u64>,
    backend_permits_limit: Gauge<u64>,
    backend_permit_wait_duration: Histogram<f64>,
    backend_selections: Counter<u64>,
    backend_executions: Counter<u64>,
    backend_execution_duration: Histogram<f64>,
    cache_substitutions: Counter<u64>,
    cache_substitution_duration: Histogram<f64>,
    cache_publications: Counter<u64>,
    cache_publication_duration: Histogram<f64>,
    store_validations: Counter<u64>,
    store_validation_duration: Histogram<f64>,
    retention_paths: Gauge<u64>,
    retention_bytes: Gauge<u64>,
    transfer_objects: Counter<u64>,
    transfer_bytes: Counter<u64>,
    transfer_object_size: Histogram<u64>,
    transfer_duration: Histogram<f64>,
    transfer_rejections: Counter<u64>,
    nomad_submissions: Counter<u64>,
    nomad_submission_duration: Histogram<f64>,
    nomad_pending: Gauge<u64>,
    nomad_placement_duration: Histogram<f64>,
    nomad_executions: Counter<u64>,
    nomad_execution_duration: Histogram<f64>,
    nomad_callback_connections: Gauge<u64>,
    nomad_callback_outcomes: Counter<u64>,
}

#[derive(Default)]
struct GaugeState {
    service_sessions: u64,
    shared_build_queue_depth: u64,
    retained_paths: u64,
    retained_bytes: u64,
    backend_permits: BTreeMap<String, (String, u64, u64)>,
    nomad_pending: BTreeMap<String, u64>,
    nomad_callback_connections: u64,
}

fn instruments() -> &'static Instruments {
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(|| {
        let meter = global::meter("telchar");
        Instruments {
            service_sessions: meter
                .u64_gauge("telchar.service.sessions")
                .with_unit("{session}")
                .build(),
            service_session_limit: meter
                .u64_gauge("telchar.service.session.limit")
                .with_unit("{session}")
                .build(),
            build_requests: meter
                .u64_counter("telchar.build.requests")
                .with_unit("{request}")
                .build(),
            build_request_duration: meter
                .f64_histogram("telchar.build.request.duration")
                .with_unit("s")
                .build(),
            build_executions: meter
                .u64_counter("telchar.build.executions")
                .with_unit("{execution}")
                .build(),
            build_execution_duration: meter
                .f64_histogram("telchar.build.execution.duration")
                .with_unit("s")
                .build(),
            build_output_count: meter
                .u64_histogram("telchar.build.output.count")
                .with_unit("{output}")
                .build(),
            shared_build_leaders: meter
                .u64_counter("telchar.shared_build.leaders")
                .with_unit("{build}")
                .build(),
            shared_build_followers: meter
                .u64_counter("telchar.shared_build.followers")
                .with_unit("{build}")
                .build(),
            shared_build_reused_results: meter
                .u64_counter("telchar.shared_build.reused_results")
                .with_unit("{build}")
                .build(),
            shared_build_queue_depth: meter
                .u64_gauge("telchar.shared_build.queue.depth")
                .with_unit("{build}")
                .build(),
            shared_build_queue_wait_duration: meter
                .f64_histogram("telchar.shared_build.queue.wait.duration")
                .with_unit("s")
                .build(),
            shared_build_queue_admissions: meter
                .u64_counter("telchar.shared_build.queue.admissions")
                .with_unit("{build}")
                .build(),
            backend_permits_active: meter
                .u64_gauge("telchar.backend.permits.active")
                .with_unit("{permit}")
                .build(),
            backend_permits_limit: meter
                .u64_gauge("telchar.backend.permits.limit")
                .with_unit("{permit}")
                .build(),
            backend_permit_wait_duration: meter
                .f64_histogram("telchar.backend.permit.wait.duration")
                .with_unit("s")
                .build(),
            backend_selections: meter
                .u64_counter("telchar.backend.selections")
                .with_unit("{selection}")
                .build(),
            backend_executions: meter
                .u64_counter("telchar.backend.executions")
                .with_unit("{execution}")
                .build(),
            backend_execution_duration: meter
                .f64_histogram("telchar.backend.execution.duration")
                .with_unit("s")
                .build(),
            cache_substitutions: meter
                .u64_counter("telchar.cache.substitutions")
                .with_unit("{attempt}")
                .build(),
            cache_substitution_duration: meter
                .f64_histogram("telchar.cache.substitution.duration")
                .with_unit("s")
                .build(),
            cache_publications: meter
                .u64_counter("telchar.cache.publications")
                .with_unit("{attempt}")
                .build(),
            cache_publication_duration: meter
                .f64_histogram("telchar.cache.publication.duration")
                .with_unit("s")
                .build(),
            store_validations: meter
                .u64_counter("telchar.store.validations")
                .with_unit("{validation}")
                .build(),
            store_validation_duration: meter
                .f64_histogram("telchar.store.validation.duration")
                .with_unit("s")
                .build(),
            retention_paths: meter
                .u64_gauge("telchar.retention.paths")
                .with_unit("{path}")
                .build(),
            retention_bytes: meter
                .u64_gauge("telchar.retention.bytes")
                .with_unit("By")
                .build(),
            transfer_objects: meter
                .u64_counter("telchar.transfer.objects")
                .with_unit("{object}")
                .build(),
            transfer_bytes: meter
                .u64_counter("telchar.transfer.bytes")
                .with_unit("By")
                .build(),
            transfer_object_size: meter
                .u64_histogram("telchar.transfer.object.size")
                .with_unit("By")
                .build(),
            transfer_duration: meter
                .f64_histogram("telchar.transfer.duration")
                .with_unit("s")
                .build(),
            transfer_rejections: meter
                .u64_counter("telchar.transfer.rejections")
                .with_unit("{rejection}")
                .build(),
            nomad_submissions: meter
                .u64_counter("telchar.nomad.submissions")
                .with_unit("{submission}")
                .build(),
            nomad_submission_duration: meter
                .f64_histogram("telchar.nomad.submission.duration")
                .with_unit("s")
                .build(),
            nomad_pending: meter
                .u64_gauge("telchar.nomad.pending")
                .with_unit("{allocation}")
                .build(),
            nomad_placement_duration: meter
                .f64_histogram("telchar.nomad.placement.duration")
                .with_unit("s")
                .build(),
            nomad_executions: meter
                .u64_counter("telchar.nomad.executions")
                .with_unit("{execution}")
                .build(),
            nomad_execution_duration: meter
                .f64_histogram("telchar.nomad.execution.duration")
                .with_unit("s")
                .build(),
            nomad_callback_connections: meter
                .u64_gauge("telchar.nomad.callback.connections")
                .with_unit("{connection}")
                .build(),
            nomad_callback_outcomes: meter
                .u64_counter("telchar.nomad.callback.outcomes")
                .with_unit("{connection}")
                .build(),
        }
    })
}

fn gauge_state() -> &'static Mutex<GaugeState> {
    static STATE: OnceLock<Mutex<GaugeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(GaugeState::default()))
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn backend_attributes(name: &str, kind: &str) -> [KeyValue; 2] {
    [
        KeyValue::new("backend.name", name.to_owned()),
        KeyValue::new("backend.kind", kind.to_owned()),
    ]
}

pub fn record_service_session_limit(limit: u64) {
    instruments().service_session_limit.record(limit, &[]);
}

pub fn session_started() {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.service_sessions = state.service_sessions.saturating_add(1);
    instruments()
        .service_sessions
        .record(state.service_sessions, &[]);
}

pub fn session_finished() {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.service_sessions = state.service_sessions.saturating_sub(1);
    instruments()
        .service_sessions
        .record(state.service_sessions, &[]);
}

pub fn build_admitted(build_mode: &str, fixed_output: bool, output_count: usize) {
    let attributes = [
        KeyValue::new("build_mode", build_mode.to_owned()),
        KeyValue::new("fixed_output", fixed_output),
    ];
    instruments().build_requests.add(1, &attributes);
    instruments()
        .build_output_count
        .record(output_count as u64, &attributes);
}

pub fn build_request_finished(duration: Duration, outcome: &str, failure_class: Option<&str>) {
    let mut attributes = vec![KeyValue::new("outcome", outcome.to_owned())];
    if let Some(failure_class) = failure_class {
        attributes.push(KeyValue::new("failure_class", failure_class.to_owned()));
    }
    instruments()
        .build_request_duration
        .record(seconds(duration), &attributes);
}

pub fn build_execution_finished(duration: Duration, outcome: &str, failure_class: Option<&str>) {
    let mut attributes = vec![KeyValue::new("outcome", outcome.to_owned())];
    if let Some(failure_class) = failure_class {
        attributes.push(KeyValue::new("failure_class", failure_class.to_owned()));
    }
    instruments().build_executions.add(1, &attributes);
    instruments()
        .build_execution_duration
        .record(seconds(duration), &attributes);
}

pub fn shared_build_leader() {
    instruments().shared_build_leaders.add(1, &[]);
}

pub fn shared_build_follower() {
    instruments().shared_build_followers.add(1, &[]);
}

pub fn shared_build_reused_result() {
    instruments().shared_build_reused_results.add(1, &[]);
}

pub fn shared_build_enqueued() {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.shared_build_queue_depth = state.shared_build_queue_depth.saturating_add(1);
    instruments()
        .shared_build_queue_depth
        .record(state.shared_build_queue_depth, &[]);
}

pub fn shared_build_admitted(wait: Duration) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.shared_build_queue_depth = state.shared_build_queue_depth.saturating_sub(1);
    instruments()
        .shared_build_queue_depth
        .record(state.shared_build_queue_depth, &[]);
    instruments().shared_build_queue_admissions.add(1, &[]);
    instruments()
        .shared_build_queue_wait_duration
        .record(seconds(wait), &[]);
}

pub fn backend_configured(name: &str, kind: &str, limit: u64) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state
        .backend_permits
        .insert(name.to_owned(), (kind.to_owned(), 0, limit));
    let attributes = backend_attributes(name, kind);
    instruments().backend_permits_active.record(0, &attributes);
    instruments()
        .backend_permits_limit
        .record(limit, &attributes);
}

pub fn backend_selection(
    name: Option<&str>,
    kind: Option<&str>,
    outcome: &str,
    failure_class: Option<&str>,
) {
    let mut attributes = vec![KeyValue::new("outcome", outcome.to_owned())];
    if let (Some(name), Some(kind)) = (name, kind) {
        attributes.extend(backend_attributes(name, kind));
    }
    if let Some(failure_class) = failure_class {
        attributes.push(KeyValue::new("failure_class", failure_class.to_owned()));
    }
    instruments().backend_selections.add(1, &attributes);
}

pub fn backend_permit_acquired(name: &str, kind: &str, wait: Duration) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = state
        .backend_permits
        .entry(name.to_owned())
        .or_insert_with(|| (kind.to_owned(), 0, 0));
    entry.1 = entry.1.saturating_add(1);
    let attributes = backend_attributes(name, kind);
    instruments()
        .backend_permits_active
        .record(entry.1, &attributes);
    instruments()
        .backend_permit_wait_duration
        .record(seconds(wait), &attributes);
}

pub fn backend_permit_released(name: &str, kind: &str) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = state
        .backend_permits
        .entry(name.to_owned())
        .or_insert_with(|| (kind.to_owned(), 0, 0));
    entry.1 = entry.1.saturating_sub(1);
    instruments()
        .backend_permits_active
        .record(entry.1, &backend_attributes(name, kind));
}

pub fn backend_execution_finished(
    name: &str,
    kind: &str,
    duration: Duration,
    outcome: &str,
    failure_class: Option<&str>,
) {
    let mut attributes = backend_attributes(name, kind).to_vec();
    attributes.push(KeyValue::new("outcome", outcome.to_owned()));
    if let Some(failure_class) = failure_class {
        attributes.push(KeyValue::new("failure_class", failure_class.to_owned()));
    }
    instruments().backend_executions.add(1, &attributes);
    instruments()
        .backend_execution_duration
        .record(seconds(duration), &attributes);
}

pub fn cache_substitution_finished(duration: Duration, outcome: &str) {
    let attributes = [KeyValue::new("outcome", outcome.to_owned())];
    instruments().cache_substitutions.add(1, &attributes);
    instruments()
        .cache_substitution_duration
        .record(seconds(duration), &attributes);
}

pub fn cache_publication_finished(duration: Duration, outcome: &str) {
    let attributes = [KeyValue::new("outcome", outcome.to_owned())];
    instruments().cache_publications.add(1, &attributes);
    instruments()
        .cache_publication_duration
        .record(seconds(duration), &attributes);
}

pub fn store_validation_finished(duration: Duration, outcome: &str, authority: &str) {
    let attributes = [
        KeyValue::new("outcome", outcome.to_owned()),
        KeyValue::new("authority", authority.to_owned()),
    ];
    instruments().store_validations.add(1, &attributes);
    instruments()
        .store_validation_duration
        .record(seconds(duration), &attributes);
}

pub fn retention_changed(path_delta: i64, byte_delta: i64) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.retained_paths = state.retained_paths.saturating_add_signed(path_delta);
    state.retained_bytes = state.retained_bytes.saturating_add_signed(byte_delta);
    instruments()
        .retention_paths
        .record(state.retained_paths, &[]);
    instruments()
        .retention_bytes
        .record(state.retained_bytes, &[]);
}

pub fn transfer_finished(direction: &str, purpose: &str, bytes: u64, duration: Duration) {
    let attributes = [
        KeyValue::new("direction", direction.to_owned()),
        KeyValue::new("purpose", purpose.to_owned()),
    ];
    instruments().transfer_objects.add(1, &attributes);
    instruments().transfer_bytes.add(bytes, &attributes);
    instruments()
        .transfer_object_size
        .record(bytes, &attributes);
    instruments()
        .transfer_duration
        .record(seconds(duration), &attributes);
}

pub fn transfer_rejected(direction: &str, reason: &str) {
    instruments().transfer_rejections.add(
        1,
        &[
            KeyValue::new("direction", direction.to_owned()),
            KeyValue::new("reason", reason.to_owned()),
        ],
    );
}

pub fn nomad_submission_finished(backend: &str, duration: Duration, outcome: &str) {
    let attributes = [
        KeyValue::new("backend.name", backend.to_owned()),
        KeyValue::new("outcome", outcome.to_owned()),
    ];
    instruments().nomad_submissions.add(1, &attributes);
    instruments()
        .nomad_submission_duration
        .record(seconds(duration), &attributes);
}

pub fn nomad_pending_changed(backend: &str, delta: i64) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pending = state.nomad_pending.entry(backend.to_owned()).or_default();
    *pending = pending.saturating_add_signed(delta);
    instruments().nomad_pending.record(
        *pending,
        &[KeyValue::new("backend.name", backend.to_owned())],
    );
}

pub fn nomad_placed(backend: &str, duration: Duration) {
    instruments().nomad_placement_duration.record(
        seconds(duration),
        &[KeyValue::new("backend.name", backend.to_owned())],
    );
}

pub fn nomad_execution_finished(backend: &str, duration: Duration, outcome: &str) {
    let attributes = [
        KeyValue::new("backend.name", backend.to_owned()),
        KeyValue::new("outcome", outcome.to_owned()),
    ];
    instruments().nomad_executions.add(1, &attributes);
    instruments()
        .nomad_execution_duration
        .record(seconds(duration), &attributes);
}

pub fn nomad_callback_started() {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.nomad_callback_connections = state.nomad_callback_connections.saturating_add(1);
    instruments()
        .nomad_callback_connections
        .record(state.nomad_callback_connections, &[]);
}

pub fn nomad_callback_finished(outcome: &str) {
    let mut state = gauge_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.nomad_callback_connections = state.nomad_callback_connections.saturating_sub(1);
    instruments()
        .nomad_callback_connections
        .record(state.nomad_callback_connections, &[]);
    instruments()
        .nomad_callback_outcomes
        .add(1, &[KeyValue::new("outcome", outcome.to_owned())]);
}

pub fn emit_smoke_metrics() {
    record_service_session_limit(8);
    session_started();
    session_finished();
    build_admitted("normal", false, 1);
    build_request_finished(Duration::from_millis(20), "succeeded", None);
    build_execution_finished(Duration::from_millis(15), "succeeded", None);
    shared_build_leader();
    shared_build_follower();
    shared_build_reused_result();
    shared_build_enqueued();
    shared_build_admitted(Duration::from_millis(5));
    backend_configured("smoke", "local", 2);
    backend_selection(Some("smoke"), Some("local"), "selected", None);
    backend_permit_acquired("smoke", "local", Duration::from_millis(2));
    backend_permit_released("smoke", "local");
    backend_execution_finished(
        "smoke",
        "local",
        Duration::from_millis(10),
        "succeeded",
        None,
    );
    cache_substitution_finished(Duration::from_millis(1), "miss");
    cache_publication_finished(Duration::from_millis(1), "succeeded");
    store_validation_finished(Duration::from_millis(1), "succeeded", "input_addressed");
    retention_changed(1, 1024);
    retention_changed(-1, -1024);
    transfer_finished("outbound", "output", 1024, Duration::from_millis(1));
    transfer_rejected("inbound", "limit");
    nomad_submission_finished("smoke", Duration::from_millis(1), "succeeded");
    nomad_pending_changed("smoke", 1);
    nomad_placed("smoke", Duration::from_millis(3));
    nomad_pending_changed("smoke", -1);
    nomad_execution_finished("smoke", Duration::from_millis(9), "succeeded");
    nomad_callback_started();
    nomad_callback_finished("succeeded");
}
