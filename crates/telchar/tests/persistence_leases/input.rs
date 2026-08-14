//! Tests persistence input leases.

use super::*;

#[test]
fn request_input_lease_batch_is_atomic_and_typed() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-batch-request",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");

    let records = telchar::persistence::create_request_input_leases_with_limit(
        fixture.url(),
        "input-batch-request",
        30,
        &[
            (
                "input-batch-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
                10,
            ),
            (
                "input-batch-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-input-b".to_owned(),
                20,
            ),
        ],
    )
    .expect("input lease batch persists");

    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| {
        record.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && record.owner_id == "input-batch-request"
            && record.purpose == telchar::persistence::StoreLeasePurpose::Input
            && record.state == telchar::persistence::StoreLeaseState::Active
            && record.released_at.is_none()
    }));
}

#[test]
fn request_input_lease_batch_rejects_duplicate_paths_before_connection() {
    let error = telchar::persistence::create_request_input_leases(
        "postgresql://invalid",
        "input-batch-request",
        &[
            (
                "input-batch-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
            (
                "input-batch-2".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
        ],
    )
    .expect_err("duplicate path rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Configuration
    );
}

#[test]
fn request_input_lease_batch_rolls_back_statement_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-statement-failure",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_input_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.lease_id = 'input-failure-2' THEN RAISE EXCEPTION 'reject input'; END IF; RETURN NEW; END $$;
             CREATE TRIGGER reject_input_lease BEFORE INSERT ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_input_lease();",
        )
        .expect("failure trigger installs");

    let error = telchar::persistence::create_request_input_leases(
        fixture.url(),
        "input-statement-failure",
        &[
            (
                "input-failure-1".to_owned(),
                "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
            ),
            (
                "input-failure-2".to_owned(),
                "/nix/store/33333333333333333333333333333333-input-b".to_owned(),
            ),
        ],
    )
    .expect_err("statement failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'input-statement-failure'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn request_input_lease_batch_rolls_back_deferred_commit_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "input-commit-failure",
        "/nix/store/11111111111111111111111111111111-test.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE FUNCTION reject_input_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject input commit'; END $$;
             CREATE CONSTRAINT TRIGGER reject_input_commit AFTER INSERT ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_input_commit();",
        )
        .expect("commit trigger installs");

    let error = telchar::persistence::create_request_input_leases(
        fixture.url(),
        "input-commit-failure",
        &[(
            "input-commit-1".to_owned(),
            "/nix/store/22222222222222222222222222222222-input-a".to_owned(),
        )],
    )
    .expect_err("commit failure rejects");
    assert_eq!(
        error.failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM store_leases WHERE owner_id = 'input-commit-failure'",
                &[],
            )
            .expect("lease count reads")
            .get::<_, i64>(0),
        0
    );
}
