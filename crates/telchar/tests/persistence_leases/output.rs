//! Tests persistence output leases.

use super::*;

#[test]
fn create_request_output_leases_commits_complete_ordered_set() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-batch-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let records = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "output-batch-request",
        Duration::from_secs(3_600),
        &[
            (
                "output-batch-2".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-b".to_owned(),
            ),
            (
                "output-batch-1".to_owned(),
                "/nix/store/33333333333333333333333333333333-output-a".to_owned(),
            ),
        ],
    )
    .expect("output lease batch persists");

    assert_eq!(
        records
            .iter()
            .map(|record| record.lease_id.as_str())
            .collect::<Vec<_>>(),
        vec!["output-batch-2", "output-batch-1"]
    );
    assert!(records.iter().all(|record| {
        record.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && record.owner_id == "output-batch-request"
            && record.purpose == telchar::persistence::StoreLeasePurpose::Output
            && record.state == telchar::persistence::StoreLeaseState::Active
            && record.released_at.is_none()
            && record.expires_at.is_some()
    }));
    assert_eq!(records[0].created_at, records[1].created_at);
    assert_eq!(records[0].expires_at, records[1].expires_at);
    assert_eq!(
        records[0]
            .expires_at
            .expect("output deadline exists")
            .duration_since(records[0].created_at)
            .expect("deadline follows creation"),
        Duration::from_secs(3_600)
    );
}

#[test]
fn create_request_output_leases_rejects_fractional_or_out_of_range_retention() {
    for retention in [
        Duration::from_secs(59),
        Duration::new(60, 1),
        Duration::from_secs(86_401),
    ] {
        assert_eq!(
            telchar::persistence::create_request_output_leases(
                "postgresql://127.0.0.1:1/no-connection",
                "output-duration-request",
                retention,
                &[(
                    "output-duration-lease".to_owned(),
                    "/nix/store/22222222222222222222222222222222-output-duration".to_owned(),
                )],
            )
            .expect_err("invalid retention rejects before connection")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
}

#[test]
fn create_request_output_leases_rolls_back_second_row_conflict() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for request_id in ["output-conflict-request", "output-existing-request"] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-output-test.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
    }
    let existing = telchar::persistence::create_store_lease(
        fixture.url(),
        "output-conflict-2",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "output-existing-request",
        "/nix/store/44444444444444444444444444444444-existing-output",
        telchar::persistence::StoreLeasePurpose::Output,
    )
    .expect("existing lease persists");

    let error = telchar::persistence::create_request_output_leases(
        fixture.url(),
        "output-conflict-request",
        Duration::from_secs(3_600),
        &[
            (
                "output-conflict-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
            ),
            (
                "output-conflict-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-output-b".to_owned(),
            ),
        ],
    )
    .expect_err("second output lease conflicts");

    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Conflict
    );
    assert!(
        telchar::persistence::read_store_lease(fixture.url(), "output-conflict-1")
            .expect("first output lease reads")
            .is_none()
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "output-conflict-2")
            .expect("existing output lease reads"),
        Some(existing)
    );
}

#[test]
fn create_request_output_leases_rejects_invalid_batches_before_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-validation-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let valid = (
        "output-validation-1".to_owned(),
        "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
    );
    let too_many = (0..=nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_OUTPUTS)
        .map(|index| {
            (
                format!("output-limit-{index}"),
                format!("/nix/store/{index:032x}-output-limit-{index}"),
            )
        })
        .collect::<Vec<_>>();

    for leases in [
        vec![valid.clone(), valid.clone()],
        vec![
            valid.clone(),
            ("output-validation-2".to_owned(), valid.1.clone()),
        ],
        vec![(
            "".to_owned(),
            "/nix/store/33333333333333333333333333333333-output-b".to_owned(),
        )],
        vec![("output-invalid-path".to_owned(), "relative".to_owned())],
        too_many,
    ] {
        assert_eq!(
            telchar::persistence::create_request_output_leases(
                fixture.url(),
                "output-validation-request",
                Duration::from_secs(3_600),
                &leases,
            )
            .expect_err("invalid output lease batch rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
    assert_eq!(
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "missing-output-request",
            Duration::from_secs(3_600),
            &[valid],
        )
        .expect_err("missing request rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Missing
    );
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'output-validation-request'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn release_expired_output_leases_obeys_deadline_cursor_bound_and_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "expiry-request",
        "/nix/store/11111111111111111111111111111111-expiry.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "expiry-request",
        Duration::from_secs(60),
        &[
            (
                "expiry-a".to_owned(),
                "/nix/store/22222222222222222222222222222222-expiry-a".to_owned(),
            ),
            (
                "expiry-b".to_owned(),
                "/nix/store/33333333333333333333333333333333-expiry-b".to_owned(),
            ),
            (
                "expiry-c".to_owned(),
                "/nix/store/44444444444444444444444444444444-expiry-c".to_owned(),
            ),
        ],
    )
    .expect("output leases persist");
    let now = std::time::SystemTime::now() + Duration::from_secs(61);

    let first =
        telchar::persistence::release_expired_request_output_leases(fixture.url(), now, None, 1)
            .expect("first expiry page releases");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].lease_id, "expiry-a");
    assert_eq!(
        first[0].state,
        telchar::persistence::StoreLeaseState::Released
    );
    assert!(first[0].released_at.is_some());
    assert!(first[0].expires_at.is_some());

    let second = telchar::persistence::release_expired_request_output_leases(
        fixture.url(),
        now,
        Some("expiry-a"),
        256,
    )
    .expect("second expiry page releases");
    assert_eq!(
        second
            .iter()
            .map(|lease| lease.lease_id.as_str())
            .collect::<Vec<_>>(),
        vec!["expiry-b", "expiry-c"]
    );
    assert!(telchar::persistence::release_expired_request_output_leases(
        fixture.url(),
        now,
        None,
        256,
    )
    .expect("released rows are not selected again")
    .is_empty());
}

#[test]
fn release_expired_output_leases_rejects_invalid_inputs_before_connection() {
    let now = std::time::SystemTime::now();
    for (cursor, maximum_rows) in [(None, 0), (None, 257), (Some(""), 1)] {
        assert_eq!(
            telchar::persistence::release_expired_request_output_leases(
                "postgresql://127.0.0.1:1/no-connection",
                now,
                cursor,
                maximum_rows,
            )
            .expect_err("invalid expiry input rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    }
}

#[test]
fn create_request_output_leases_empty_set_avoids_database_and_redacts_telemetry() {
    let _guard = TELEMETRY_TESTS.lock().expect("telemetry lock holds");
    assert!(telchar::persistence::create_request_output_leases(
        "postgresql://127.0.0.1:1/no-connection",
        "output-empty-request",
        Duration::from_secs(3_600),
        &[],
    )
    .expect("empty output lease batch succeeds")
    .is_empty());

    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-telemetry-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let captured = EventCapture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(captured.clone()));
    tracing::dispatcher::with_default(&dispatch, || {
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "output-telemetry-request",
            Duration::from_secs(3_600),
            &[(
                "output-telemetry-lease".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-telemetry".to_owned(),
            )],
        )
        .expect("output lease batch persists");
        let error = telchar::persistence::create_request_output_leases(
            "postgresql://output-telemetry-sensitive-url",
            "output-telemetry-sensitive-request",
            Duration::from_secs(3_600),
            &[(
                "output-telemetry-sensitive-lease".to_owned(),
                "relative".to_owned(),
            )],
        )
        .expect_err("invalid output lease batch rejects");
        assert_eq!(
            error.failure(),
            telchar::persistence::StoreLeaseFailure::Configuration
        );
    });
    let events = captured.events();
    assert!(events.iter().any(|event| {
        event.contains("database.store_lease.created")
            && event.contains("operation=\"create-output-retention\"")
            && event.contains("result=\"succeeded\"")
    }));
    for forbidden in [
        fixture.url(),
        "output-telemetry-request",
        "output-telemetry-lease",
        "/nix/store/22222222222222222222222222222222-output-telemetry",
        "output-telemetry-sensitive-url",
        "output-telemetry-sensitive-request",
        "output-telemetry-sensitive-lease",
    ] {
        assert!(
            !events.iter().any(|event| event.contains(forbidden)),
            "{events:?}"
        );
    }
}

#[test]
fn create_request_output_leases_rolls_back_deferred_commit_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "output-commit-failure-request",
        "/nix/store/11111111111111111111111111111111-output-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_output_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.purpose = 'output' THEN RAISE EXCEPTION 'reject output commit'; END IF; RETURN NEW; END $$;
             CREATE CONSTRAINT TRIGGER reject_output_commit AFTER INSERT ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_output_commit();",
        )
        .expect("commit failure trigger installs");

    assert_eq!(
        telchar::persistence::create_request_output_leases(
            fixture.url(),
            "output-commit-failure-request",
            Duration::from_secs(3_600),
            &[(
                "output-commit-failure".to_owned(),
                "/nix/store/22222222222222222222222222222222-output-a".to_owned(),
            )],
        )
        .expect_err("commit failure rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'output-commit-failure-request'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}
