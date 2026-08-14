//! Tests build requests.

use super::*;

#[test]
fn create_and_read_build_request_persist_immutable_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "request-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
        "release-engineering",
        "build-farm",
    )
    .expect("session opens");

    let created = telchar::persistence::create_build_request(
        fixture.url(),
        "request-1",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "release-engineering",
        "build-farm",
    )
    .expect("build request persists");
    let read = telchar::persistence::read_build_request(fixture.url(), "request-1")
        .expect("build request reads")
        .expect("build request exists");

    assert_eq!(created, read);
    assert_eq!(read.request_id, "request-1");
    assert_eq!(
        read.derivation_path,
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv"
    );
    assert_eq!(read.system, "x86_64-linux");
    assert_eq!(read.audit_subject, "release-engineering");
    assert_eq!(read.quota_subject, "build-farm");
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "absent-request")
            .expect("absent request reads")
            .is_none()
    );
}

#[test]
fn build_request_operation_rejects_unbounded_subjects_before_connection() {
    let path = "/nix/store/11111111111111111111111111111111-bounded-request.drv";
    let long_audit_subject = "a".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1);
    let long_quota_subject = "q".repeat(telchar::service::ipc::MAX_IPC_CREDENTIAL_ID_BYTES + 1);
    for (audit_subject, quota_subject) in [
        ("", "quota"),
        ("audit", ""),
        (long_audit_subject.as_str(), "quota"),
        ("audit", long_quota_subject.as_str()),
    ] {
        assert_eq!(
            telchar::persistence::create_build_request(
                "postgresql://127.0.0.1:1/no-connection",
                "bounded-request",
                path,
                "x86_64-linux",
                audit_subject,
                quota_subject,
            )
            .expect_err("invalid metadata rejects before connection")
            .failure(),
            telchar::persistence::BuildRequestFailure::Configuration
        );
    }
}

#[test]
fn build_request_state_rejects_invalid_inputs_and_conflicts_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let path = "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv";
    let maximum_path = format!(
        "/{}",
        "x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES - 1)
    );
    telchar::persistence::create_build_request(
        fixture.url(),
        "maximum-path",
        &maximum_path,
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("maximum protocol store path persists");

    for (database_url, request_id, derivation_path, system) in [
        (fixture.url(), "", path, "x86_64-linux"),
        (
            fixture.url(),
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
            path,
            "x86_64-linux",
        ),
        (fixture.url(), "invalid-path", "", "x86_64-linux"),
        (
            fixture.url(),
            "oversized-path",
            &"x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES + 1),
            "x86_64-linux",
        ),
        (fixture.url(), "invalid-system", path, ""),
        (
            fixture.url(),
            "oversized-system",
            path,
            &"x".repeat(telchar::service::ipc::MAX_IPC_COMPONENT_BYTES + 1),
        ),
        ("", "missing-url", path, "x86_64-linux"),
    ] {
        let error = telchar::persistence::create_build_request(
            database_url,
            request_id,
            derivation_path,
            system,
            "test-audit",
            "test-quota",
        )
        .expect_err("invalid build request rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::BuildRequestFailure::Configuration
        );
        assert_eq!(error.to_string(), "build request state operation failed");
    }

    let first = telchar::persistence::create_build_request(
        fixture.url(),
        "request-1",
        path,
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("first request persists");
    for (derivation_path, system) in [(path, "x86_64-linux"), ("/nix/store/other.drv", "other")] {
        assert_eq!(
            telchar::persistence::create_build_request(
                fixture.url(),
                "request-1",
                derivation_path,
                system,
                "test-audit",
                "test-quota",
            )
            .expect_err("duplicate request rejects")
            .failure(),
            telchar::persistence::BuildRequestFailure::Conflict
        );
    }
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "request-1")
            .expect("request reads"),
        Some(first)
    );
}

#[test]
fn build_request_state_survives_restart_and_malformed_rows_fail_closed() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let created = telchar::persistence::create_build_request(
        fixture.url(),
        "restart-request",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    fixture.restart();

    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "restart-request")
            .expect("request reads"),
        Some(created)
    );
    fixture
        .connect()
        .execute(
            "INSERT INTO build_requests (request_id, derivation_path, system) VALUES ('malformed', $1, 'x86_64-linux')",
            &[&"x".repeat(nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES + 1)],
        )
        .expect("malformed row inserts");
    assert_eq!(
        telchar::persistence::read_build_request(fixture.url(), "malformed")
            .expect_err("malformed request rejects")
            .failure(),
        telchar::persistence::BuildRequestFailure::Query
    );
}

#[test]
fn failed_build_request_statement_and_commit_do_not_persist_rows() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_build_request_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject insert'; END $$; CREATE TRIGGER reject_build_request_insert BEFORE INSERT ON build_requests FOR EACH ROW EXECUTE FUNCTION reject_build_request_insert();",
        )
        .expect("statement failure trigger installs");

    let error = telchar::persistence::create_build_request(
        fixture.url(),
        "failed-statement",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect_err("statement failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::BuildRequestFailure::Query
    );
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "failed-statement")
            .expect("request reads")
            .is_none()
    );
    client
        .batch_execute("DROP TRIGGER reject_build_request_insert ON build_requests; DROP FUNCTION reject_build_request_insert()")
        .expect("statement failure trigger removes");
    client
        .batch_execute(
            "CREATE FUNCTION reject_build_request_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject commit'; END $$; CREATE CONSTRAINT TRIGGER reject_build_request_commit AFTER INSERT ON build_requests DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_build_request_commit();",
        )
        .expect("commit failure trigger installs");

    let error = telchar::persistence::create_build_request(
        fixture.url(),
        "failed-commit",
        "/nix/store/11111111111111111111111111111111-telchar-gate-3-contract.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect_err("commit failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::BuildRequestFailure::Commit
    );
    assert!(
        telchar::persistence::read_build_request(fixture.url(), "failed-commit")
            .expect("request reads")
            .is_none()
    );
}
