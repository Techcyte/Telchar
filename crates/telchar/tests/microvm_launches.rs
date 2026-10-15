//! Exercises durable MicroVM launch identity and ownership with PostgreSQL.

mod support;

use std::time::Duration;

use support::postgres::PostgresFixture;
use telchar::backend::BackendKind;

fn prepared_launch() -> PostgresFixture {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("database migrates");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[1; 32],
        "launch-fixture",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-launch"],
    )
    .expect("fixture build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation)
        .expect("fixture attempt starts");
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    fixture.connect().execute(
        "INSERT INTO microvm_launches (intent_id, attempt_id, backend_name, account_id, region, image_arn, image_version, configuration_digest, request_digest, client_token, credential_verifier, admission_cutoff, maximum_duration_seconds) VALUES ('intent-one', $1, 'launch-fixture', '116392764028', 'us-west-2', 'arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture', '1', $2, $2, 'token-one', $2, clock_timestamp() + interval '1 minute', 600)",
        &[&attempt.attempt_id, &&[1_u8; 32][..]],
    ).expect("launch reserves durable capacity");
    fixture
}

#[test]
fn cancellation_closes_prepared_launch_and_releases_only_unused_capacity() {
    let fixture = prepared_launch();
    assert!(
        telchar::persistence::cancel_microvm_launch(fixture.url(), "intent-one")
            .expect("prepared launch cancels")
    );
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("cancelled launch cannot dispatch")
    );
    let row = fixture.connect().query_one(
        "SELECT phase, reservation_released_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("cancelled launch reads");
    assert_eq!(row.get::<_, String>(0), "closed");
    assert!(row.get::<_, bool>(1));
    assert!(
        !telchar::persistence::cancel_microvm_launch(fixture.url(), "intent-one")
            .expect("duplicate cancellation has no transition")
    );
}

#[test]
fn cancellation_retains_dispatched_and_bound_reservations() {
    for provider_id in [None, Some("microvm-exact")] {
        let mut fixture = prepared_launch();
        assert!(
            telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
                .expect("launch dispatches")
        );
        if let Some(provider_id) = provider_id {
            fixture.connect().execute(
                "UPDATE microvm_launches SET phase = 'bound', provider_id = $1 WHERE intent_id = 'intent-one'", &[&provider_id],
            ).expect("API binding fixture records");
        }
        assert!(
            telchar::persistence::cancel_microvm_launch(fixture.url(), "intent-one")
                .expect("dispatched launch cancels")
        );
        fixture.restart();
        let row = fixture.connect().query_one(
            "SELECT phase, reservation_released_at IS NULL, provider_id FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
        ).expect("cleanup obligation survives restart");
        assert_eq!(
            row.get::<_, String>(0),
            if provider_id.is_some() {
                "closed"
            } else {
                "ambiguous"
            }
        );
        assert!(row.get::<_, bool>(1));
        assert_eq!(row.get::<_, Option<String>>(2).as_deref(), provider_id);
    }
}

#[test]
fn cancellation_rejects_expired_ownership() {
    let fixture = prepared_launch();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    fixture.expire_singleton_ownership("daemon");
    assert!(
        telchar::persistence::cancel_microvm_launch(owner.database_url(), "intent-one").is_err()
    );
    let row = fixture.connect().query_one(
        "SELECT phase, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("fenced launch reads");
    assert_eq!(row.get::<_, String>(0), "prepared");
    assert!(row.get::<_, bool>(1));
}

#[test]
fn cancellation_and_dispatch_race_preserves_ambiguous_capacity() {
    let fixture = prepared_launch();
    let dispatch_url = fixture.url().to_owned();
    let cancel_url = dispatch_url.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let dispatch_barrier = barrier.clone();
    let dispatch = std::thread::spawn(move || {
        dispatch_barrier.wait();
        telchar::persistence::dispatch_microvm_launch(&dispatch_url, "intent-one")
            .expect("racing dispatch resolves")
    });
    let cancel = std::thread::spawn(move || {
        barrier.wait();
        telchar::persistence::cancel_microvm_launch(&cancel_url, "intent-one")
            .expect("racing cancellation resolves")
    });
    let dispatched = dispatch.join().expect("dispatch joins");
    assert!(cancel.join().expect("cancellation joins"));
    let row = fixture.connect().query_one(
        "SELECT phase, reservation_released_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("raced launch reads");
    assert_eq!(
        row.get::<_, String>(0),
        if dispatched { "ambiguous" } else { "closed" }
    );
    assert_eq!(row.get::<_, bool>(1), !dispatched);
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("cancelled execution never redispatches")
    );
}

#[test]
fn launch_reservation_survives_build_failure_and_database_restart() {
    let mut fixture = prepared_launch();
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv";
    fixture.connect().execute(
        "UPDATE microvm_launches SET reservation_released_at = clock_timestamp() WHERE intent_id = 'intent-one'", &[],
    ).expect("reservation releases for negative case");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("released reservation cannot dispatch")
    );
    fixture.connect().execute(
        "UPDATE microvm_launches SET reservation_released_at = NULL, created_at = clock_timestamp() - interval '2 minutes', admission_cutoff = clock_timestamp() - interval '1 minute' WHERE intent_id = 'intent-one'", &[],
    ).expect("cutoff expires for negative case");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("expired intent cannot dispatch")
    );
    fixture.connect().execute(
        "UPDATE microvm_launches SET admission_cutoff = clock_timestamp() + interval '1 minute' WHERE intent_id = 'intent-one'", &[],
    ).expect("cutoff restores for concurrency case");
    let first_url = fixture.url().to_owned();
    let second_url = first_url.clone();
    let first = std::thread::spawn(move || {
        telchar::persistence::dispatch_microvm_launch(&first_url, "intent-one")
            .expect("first dispatch resolves")
    });
    let second = std::thread::spawn(move || {
        telchar::persistence::dispatch_microvm_launch(&second_url, "intent-one")
            .expect("second dispatch resolves")
    });
    assert_eq!(
        usize::from(first.join().expect("first caller joins"))
            + usize::from(second.join().expect("second caller joins")),
        1
    );
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation,
        "launch-ambiguous",
        &serde_json::json!({}),
        Duration::from_secs(3600),
    )
    .expect("build fails independently");
    fixture.restart();
    let row = fixture.connect().query_one(
        "SELECT reservation_released_at IS NULL, phase FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("launch survives restart");
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, String>(1), "dispatching");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("restart cannot redispatch")
    );
    fixture
        .connect()
        .execute(
            "UPDATE microvm_launches SET phase = 'prepared' WHERE intent_id = 'intent-one'",
            &[],
        )
        .expect("terminal attempt exercises dispatch gate");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("terminal attempt cannot dispatch")
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    fixture.expire_singleton_ownership("daemon");
    let mut stale = postgres::Client::connect(owner.database_url(), postgres::NoTls)
        .expect("stale connection opens");
    let error = stale
        .execute(
            "UPDATE microvm_launches SET phase = 'ambiguous' WHERE intent_id = 'intent-one'",
            &[],
        )
        .expect_err("expired owner cannot mutate launch");
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
}
