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
