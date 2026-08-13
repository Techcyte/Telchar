//! Tests shared build scheduling contracts and failure boundaries, including claim.

mod support;

use std::thread;

use support::postgres::PostgresFixture;
use telchar::backend::BackendKind;

fn claim(fixture: &PostgresFixture, derivation_path: &str, digest: u8) {
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[digest; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/ffffffffffffffffffffffffffffffff-output"],
    )
    .expect("shared build claims");
}

#[test]
fn shared_build_queue_persists_trusted_subject_and_fifo_position() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let first = "/nix/store/11111111111111111111111111111111-first.drv";
    let second = "/nix/store/22222222222222222222222222222222-second.drv";
    claim(&fixture, first, 1);
    claim(&fixture, second, 2);

    let first_entry =
        telchar::persistence::enqueue_shared_build(fixture.url(), first, "release-engineering", 2)
            .expect("first build enqueues");
    thread::sleep(std::time::Duration::from_millis(5));
    let second_entry =
        telchar::persistence::enqueue_shared_build(fixture.url(), second, "release-engineering", 2)
            .expect("second build enqueues");

    assert!(first_entry.queue_position < second_entry.queue_position);
    assert_eq!(first_entry.quota_subject, "release-engineering");
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_queued_shared_builds(fixture.url(), 16).expect("queue reads"),
        [first_entry, second_entry]
    );
}

#[test]
fn shared_build_queue_enforces_subject_bound() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let first = "/nix/store/44444444444444444444444444444444-first.drv";
    let second = "/nix/store/55555555555555555555555555555555-second.drv";
    claim(&fixture, first, 4);
    claim(&fixture, second, 5);

    telchar::persistence::enqueue_shared_build(fixture.url(), first, "alice", 1)
        .expect("first build enqueues");
    assert_eq!(
        telchar::persistence::enqueue_shared_build(fixture.url(), second, "alice", 1)
            .expect_err("subject queue limit rejects")
            .failure(),
        telchar::persistence::SharedBuildFailure::Quota
    );
}

#[test]
fn active_execution_limit_is_atomic_and_releases_on_terminal_failure() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let first = "/nix/store/66666666666666666666666666666666-first.drv";
    let second = "/nix/store/77777777777777777777777777777777-second.drv";
    claim(&fixture, first, 6);
    claim(&fixture, second, 7);
    telchar::persistence::enqueue_shared_build(fixture.url(), first, "alice", 2)
        .expect("first build enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), second, "alice", 2)
        .expect("second build enqueues");

    telchar::persistence::start_queued_shared_build(fixture.url(), first, 1)
        .expect("first build starts");
    assert_eq!(
        telchar::persistence::start_queued_shared_build(fixture.url(), second, 1)
            .expect_err("active subject limit rejects")
            .failure(),
        telchar::persistence::SharedBuildFailure::Quota
    );
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        first,
        "build-failure",
        &serde_json::json!({"stage": "execute"}),
        std::time::Duration::from_secs(3_600),
    )
    .expect("first build fails terminally");
    assert_eq!(
        telchar::persistence::start_queued_shared_build(fixture.url(), second, 1)
            .expect("released allocation permits second build")
            .state,
        telchar::persistence::SharedBuildState::Running
    );
}

#[test]
fn next_eligible_build_round_robins_subjects_and_preserves_subject_fifo() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let alice_first = "/nix/store/88888888888888888888888888888888-alice-first.drv";
    let alice_second = "/nix/store/99999999999999999999999999999999-alice-second.drv";
    let bob_first = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bob-first.drv";
    claim(&fixture, alice_first, 8);
    claim(&fixture, alice_second, 9);
    claim(&fixture, bob_first, 10);
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_first, "alice", 3)
        .expect("Alice first enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), alice_second, "alice", 3)
        .expect("Alice second enqueues");
    telchar::persistence::enqueue_shared_build(fixture.url(), bob_first, "bob", 3)
        .expect("Bob first enqueues");

    assert_eq!(
        telchar::persistence::read_next_queued_shared_build(fixture.url(), None, 16)
            .expect("next build reads")
            .expect("next build exists")
            .derivation_path,
        alice_first
    );
    assert_eq!(
        telchar::persistence::read_next_queued_shared_build(fixture.url(), Some("alice"), 16)
            .expect("next build reads")
            .expect("next build exists")
            .derivation_path,
        bob_first
    );
    assert_eq!(
        telchar::persistence::read_next_queued_shared_build(fixture.url(), Some("bob"), 16)
            .expect("next build reads")
            .expect("next build exists")
            .derivation_path,
        alice_first
    );
}

#[test]
fn shared_build_attempt_records_backend_progress_and_terminal_outcome() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation = "/nix/store/cccccccccccccccccccccccccccccccc-attempt.drv";
    claim(&fixture, derivation, 12);
    telchar::persistence::enqueue_shared_build(fixture.url(), derivation, "alice", 1)
        .expect("shared build enqueues");

    let running = telchar::persistence::start_queued_shared_build(fixture.url(), derivation, 1)
        .expect("shared build starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    assert_eq!(attempt.derivation_path, derivation);
    assert_eq!(attempt.ordinal, 1);
    assert_eq!(attempt.backend_name, running.backend_name);
    assert_eq!(attempt.backend_kind, running.backend_kind);
    assert_eq!(
        attempt.state,
        telchar::persistence::SharedBuildAttemptState::Running
    );
    assert!(attempt.started_at.is_some());
    assert!(attempt.completed_at.is_none());

    telchar::persistence::collect_shared_build(fixture.url(), derivation)
        .expect("shared build collects");
    telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation,
        &serde_json::json!({"status": "built", "outputs": []}),
        std::time::Duration::from_secs(3_600),
    )
    .expect("shared build succeeds");

    let outcome =
        telchar::persistence::read_shared_build_attempt_outcome(fixture.url(), &attempt.attempt_id)
            .expect("outcome reads")
            .expect("outcome exists");
    assert_eq!(outcome.classification, "succeeded");
    assert_eq!(outcome.result_metadata["status"], "built");
    fixture.restart();
    assert_eq!(
        telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
            .expect("attempt reads after restart")
            .expect("attempt exists after restart")
            .state,
        telchar::persistence::SharedBuildAttemptState::Succeeded
    );
}

#[test]
fn shared_build_failure_records_one_terminal_attempt_outcome() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation = "/nix/store/dddddddddddddddddddddddddddddddd-attempt-failure.drv";
    claim(&fixture, derivation, 13);
    telchar::persistence::enqueue_shared_build(fixture.url(), derivation, "alice", 1)
        .expect("shared build enqueues");
    telchar::persistence::start_queued_shared_build(fixture.url(), derivation, 1)
        .expect("shared build starts");

    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation,
        "backend-failure",
        &serde_json::json!({"reason": "fixture"}),
        std::time::Duration::from_secs(3_600),
    )
    .expect("shared build fails");

    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    assert_eq!(
        attempt.state,
        telchar::persistence::SharedBuildAttemptState::Failed
    );
    let outcome =
        telchar::persistence::read_shared_build_attempt_outcome(fixture.url(), &attempt.attempt_id)
            .expect("outcome reads")
            .expect("outcome exists");
    assert_eq!(outcome.classification, "backend-failure");
    assert_eq!(outcome.result_metadata["reason"], "fixture");
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation,
            "backend-failure",
            &serde_json::json!({"reason": "fixture"}),
            std::time::Duration::from_secs(3_600),
        )
        .expect_err("terminal completion cannot repeat")
        .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
}

#[test]
fn coalesced_follower_cannot_replace_the_quota_owner() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation = "/nix/store/33333333333333333333333333333333-shared.drv";
    claim(&fixture, derivation, 3);

    let owner = telchar::persistence::enqueue_shared_build(fixture.url(), derivation, "alice", 1)
        .expect("owner enqueues");
    assert_eq!(owner.quota_subject, "alice");
    assert_eq!(
        telchar::persistence::enqueue_shared_build(fixture.url(), derivation, "bob", 1)
            .expect_err("follower cannot replace owner")
            .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_queued_shared_builds(fixture.url(), 16).expect("queue reads")[0]
            .quota_subject,
        "alice"
    );
}
