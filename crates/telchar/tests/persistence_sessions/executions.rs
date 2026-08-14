//! Tests executions.

use super::*;

#[test]
fn local_backend_execution_registry_is_idempotent_and_survives_restart() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let specification_digest = [7_u8; 32];

    let created = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-execution-1",
        "request-1:1",
        &specification_digest,
    )
    .expect("execution registers");
    let repeated = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-execution-1",
        "request-1:1",
        &specification_digest,
    )
    .expect("exact duplicate is idempotent");

    assert_eq!(created, repeated);
    assert_eq!(created.backend_execution_id, "local-execution-1");
    assert_eq!(created.idempotency_key, "request-1:1");
    assert_eq!(created.specification_digest, specification_digest);
    assert_eq!(
        created.state,
        telchar::persistence::LocalBackendExecutionState::Accepted
    );
    assert!(created.started_at.is_none());
    assert!(created.completed_at.is_none());

    for (backend_execution_id, idempotency_key, digest) in [
        ("local-execution-1", "request-1:1", [8_u8; 32]),
        ("local-execution-1", "other-request:1", specification_digest),
        ("other-execution", "request-1:1", specification_digest),
    ] {
        assert_eq!(
            telchar::persistence::register_local_backend_execution(
                fixture.url(),
                backend_execution_id,
                idempotency_key,
                &digest,
            )
            .expect_err("conflicting identity rejects")
            .failure(),
            telchar::persistence::LocalBackendExecutionFailure::Conflict
        );
    }

    fixture.restart();
    assert_eq!(
        telchar::persistence::read_local_backend_execution(fixture.url(), "local-execution-1",)
            .expect("execution reads after restart"),
        Some(created)
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM local_backend_executions", &[])
            .expect("registry count reads")
            .get::<_, i64>(0),
        1
    );
}

#[test]
fn local_backend_execution_transitions_to_running_once() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let accepted = telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-running-transition",
        "running-transition:1",
        &[5_u8; 32],
    )
    .expect("backend execution persists");

    let running = telchar::persistence::record_local_backend_running(
        fixture.url(),
        "local-running-transition",
    )
    .expect("backend execution starts");

    assert_eq!(
        running.state,
        telchar::persistence::LocalBackendExecutionState::Running
    );
    assert!(running
        .started_at
        .is_some_and(|started_at| started_at >= accepted.created_at));
    assert!(running.completed_at.is_none());
    assert_eq!(
        telchar::persistence::record_local_backend_running(
            fixture.url(),
            "local-running-transition"
        )
        .expect_err("running transition is immutable")
        .failure(),
        telchar::persistence::LocalBackendExecutionFailure::InvalidState
    );
}

#[test]
fn local_backend_terminal_result_is_atomic_idempotent_and_immutable() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::register_local_backend_execution(
        fixture.url(),
        "local-terminal-result",
        "terminal-result:1",
        &[6_u8; 32],
    )
    .expect("backend execution persists");
    telchar::persistence::record_local_backend_running(fixture.url(), "local-terminal-result")
        .expect("backend execution starts");
    let metadata = serde_json::json!({
        "status": "built",
        "outputs": [{
            "name": "out",
            "path": "/nix/store/33333333333333333333333333333333-output"
        }]
    });

    let completed = telchar::persistence::complete_local_backend_execution(
        fixture.url(),
        "local-terminal-result",
        telchar::persistence::LocalBackendExecutionState::Succeeded,
        "succeeded",
        &metadata,
    )
    .expect("terminal result persists");

    assert_eq!(
        completed.execution.state,
        telchar::persistence::LocalBackendExecutionState::Succeeded
    );
    assert!(completed.execution.completed_at.is_some());
    assert_eq!(completed.result.classification, "succeeded");
    assert_eq!(completed.result.result_metadata, metadata);
    assert_eq!(
        telchar::persistence::complete_local_backend_execution(
            fixture.url(),
            "local-terminal-result",
            telchar::persistence::LocalBackendExecutionState::Succeeded,
            "succeeded",
            &metadata,
        )
        .expect("identical terminal result is idempotent"),
        completed
    );
    assert_eq!(
        telchar::persistence::complete_local_backend_execution(
            fixture.url(),
            "local-terminal-result",
            telchar::persistence::LocalBackendExecutionState::Failed,
            "infrastructure-failure",
            &serde_json::json!({}),
        )
        .expect_err("terminal result cannot be replaced")
        .failure(),
        telchar::persistence::LocalBackendExecutionFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::read_local_backend_execution_result(
            fixture.url(),
            "local-terminal-result"
        )
        .expect("terminal result reads")
        .expect("terminal result exists"),
        completed.result
    );
}
