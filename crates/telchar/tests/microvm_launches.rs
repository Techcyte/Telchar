//! Exercises durable MicroVM launch identity and ownership with PostgreSQL.

use telchar_test_support::postgres::PostgresFixture;

use std::time::Duration;

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
    assert!(
        telchar::persistence::reserve_microvm_launch(
            fixture.url(),
            &launch_intent(attempt.attempt_id, "intent-one"),
            1,
        )
        .expect("launch reserves durable capacity")
    );
    fixture
}

fn launch_intent(attempt_id: i64, intent_id: &str) -> telchar::persistence::MicrovmLaunchIntent {
    telchar::persistence::MicrovmLaunchIntent {
        intent_id: intent_id.to_owned(),
        attempt_id,
        backend_name: "launch-fixture".to_owned(),
        account_id: "116392764028".to_owned(),
        region: "us-west-2".to_owned(),
        image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".to_owned(),
        image_version: "1".to_owned(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: intent_id.to_owned(),
        credential_verifier: [3; 32],
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    }
}

#[test]
fn runtime_capability_authenticates_one_fenced_session_without_persisting_bearer() {
    let fixture = prepared_launch();
    let capability = telchar::microvm::Capability::generate().expect("OS entropy");
    let other = telchar::microvm::Capability::generate().expect("independent entropy");
    assert_ne!(capability.bearer(), other.bearer());
    assert_eq!(capability.bearer().len(), 43);
    fixture
        .connect()
        .execute(
            "UPDATE microvm_launches SET credential_verifier = $1",
            &[&&capability.verifier()[..]],
        )
        .expect("only verifier persists");
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(fixture.url(), "intent-one", "microvm-exact")
            .expect("bind")
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner");
    for bearer in ["", "invalid", &"a".repeat(4096), other.bearer()] {
        assert!(
            !telchar::microvm::admit_session(&owner, "intent-one", "microvm-exact", bearer)
                .expect("invalid capability denies")
        );
    }
    assert!(
        !telchar::microvm::admit_session(
            &owner,
            "intent-one",
            "microvm-other",
            capability.bearer()
        )
        .expect("wrong execution denies")
    );
    assert!(
        telchar::microvm::admit_session(&owner, "intent-one", "microvm-exact", capability.bearer())
            .expect("capability admits")
    );
    assert!(
        !telchar::microvm::admit_session(
            &owner,
            "intent-one",
            "microvm-exact",
            capability.bearer()
        )
        .expect("replay denies")
    );
    fixture.expire_singleton_ownership("daemon");
    assert!(
        telchar::microvm::admit_session(&owner, "intent-one", "microvm-exact", capability.bearer())
            .is_err()
    );
    let stored: Vec<u8> = fixture
        .connect()
        .query_one("SELECT credential_verifier FROM microvm_launches", &[])
        .expect("verifier reads")
        .get(0);
    assert_eq!(stored, capability.verifier());
    assert_ne!(stored, capability.bearer().as_bytes());
}

#[test]
fn admission_consumes_exact_bound_capability_once_across_restart() {
    let mut fixture = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(fixture.url(), "intent-one", "microvm-exact")
            .expect("bind")
    );
    for (id, verifier) in [("microvm-other", [3; 32]), ("microvm-exact", [4; 32])] {
        assert!(
            !telchar::persistence::admit_microvm_session(
                fixture.url(),
                "intent-one",
                id,
                &verifier
            )
            .expect("wrong credential rejected")
        );
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let url = fixture.url().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                telchar::persistence::admit_microvm_session(
                    &url,
                    "intent-one",
                    "microvm-exact",
                    &[3; 32],
                )
                .expect("admission resolves")
            })
        })
        .collect();
    let results: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("admission joins"))
        .collect();
    assert_eq!(results.into_iter().filter(|admitted| *admitted).count(), 1);
    fixture.restart();
    assert!(
        !telchar::persistence::admit_microvm_session(
            fixture.url(),
            "intent-one",
            "microvm-exact",
            &[3; 32]
        )
        .expect("consumed session cannot reconnect")
    );
}

#[test]
fn admission_rejects_inactive_expired_cancelled_and_fenced_launches() {
    for gate in [
        "prepared",
        "expired",
        "cancelled",
        "released",
        "terminal",
        "fenced",
    ] {
        let fixture = prepared_launch();
        if gate != "prepared" {
            assert!(
                telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
                    .expect("dispatch")
            );
            assert!(
                telchar::persistence::bind_microvm_launch(
                    fixture.url(),
                    "intent-one",
                    "microvm-exact"
                )
                .expect("bind")
            );
        }
        match gate {
            "expired" => {
                fixture.connect().batch_execute("UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 minutes', admission_cutoff = clock_timestamp() - interval '1 minute'").expect("expired fixture");
            }
            "cancelled" => {
                telchar::persistence::cancel_microvm_launch(fixture.url(), "intent-one")
                    .expect("cancel");
            }
            "released" => {
                fixture
                    .connect()
                    .batch_execute(
                        "UPDATE microvm_launches SET reservation_released_at = clock_timestamp()",
                    )
                    .expect("released fixture");
            }
            "terminal" => {
                telchar::persistence::complete_shared_build_failure(
                    fixture.url(),
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv",
                    "fixture-failure",
                    &serde_json::json!({}),
                    Duration::from_secs(3600),
                )
                .expect("terminal build");
            }
            _ => {}
        }
        let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(20),
        )
        .expect("owner");
        if gate == "fenced" {
            fixture.expire_singleton_ownership("daemon");
        }
        let result = telchar::persistence::admit_microvm_session(
            owner.database_url(),
            "intent-one",
            "microvm-exact",
            &[3; 32],
        );
        if gate == "fenced" {
            assert!(result.is_err());
        } else {
            assert!(!result.expect("gate rejects"), "{gate}");
        }
        let consumed: bool = fixture
            .connect()
            .query_one(
                "SELECT session_consumed_at IS NOT NULL FROM microvm_launches",
                &[],
            )
            .expect("consumption reads")
            .get(0);
        assert!(!consumed, "{gate}");
    }
}

#[test]
fn capacity_reservation_serializes_competitors_and_counts_ambiguous_launches() {
    let mut fixture = prepared_launch();
    let first_attempt: i64 = fixture
        .connect()
        .query_one(
            "DELETE FROM microvm_launches WHERE intent_id = 'intent-one' RETURNING attempt_id",
            &[],
        )
        .expect("fixture keeps unreserved attempt")
        .get(0);
    let derivation = "/nix/store/cccccccccccccccccccccccccccccccc-other.drv";
    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation,
        &[2; 32],
        "launch-fixture",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/dddddddddddddddddddddddddddddddd-other"],
    )
    .expect("second build claims");
    telchar::persistence::start_shared_build(fixture.url(), derivation)
        .expect("second attempt starts");
    let second_attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), derivation)
        .expect("second attempt reads")
        .expect("second attempt exists")
        .attempt_id;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = [
        (first_attempt, "reserve-one"),
        (second_attempt, "reserve-two"),
    ]
    .into_iter()
    .map(|(attempt, id)| {
        let url = fixture.url().to_owned();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let intent = launch_intent(attempt, id);
            barrier.wait();
            (
                intent,
                telchar::persistence::reserve_microvm_launch(&url, &launch_intent(attempt, id), 1)
                    .expect("capacity reservation resolves"),
            )
        })
    })
    .collect();
    let results: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("reservation joins"))
        .collect();
    assert_eq!(results.iter().filter(|(_, reserved)| *reserved).count(), 1);
    let winner = &results
        .iter()
        .find(|(_, reserved)| *reserved)
        .expect("one winner")
        .0;
    let loser = &results
        .iter()
        .find(|(_, reserved)| !*reserved)
        .expect("one waiter")
        .0;
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), &winner.intent_id)
            .expect("winner dispatches")
    );
    assert!(
        telchar::persistence::cancel_microvm_launch(fixture.url(), &winner.intent_id)
            .expect("winner becomes ambiguous")
    );
    fixture.restart();
    assert!(
        !telchar::persistence::reserve_microvm_launch(fixture.url(), loser, 1)
            .expect("ambiguous reservation blocks capacity")
    );
    assert!(
        telchar::persistence::reserve_microvm_launch(fixture.url(), loser, 2)
            .expect("explicit second slot available")
    );
    assert!(
        !telchar::persistence::reserve_microvm_launch(fixture.url(), loser, 3)
            .expect("same attempt cannot reserve again")
    );
    assert!(
        telchar::persistence::cancel_microvm_launch(fixture.url(), &loser.intent_id)
            .expect("unused reservation releases")
    );
}

#[test]
fn reservation_rejects_expiry_backend_mismatch_and_stale_ownership() {
    let fixture = prepared_launch();
    let attempt: i64 = fixture
        .connect()
        .query_one(
            "DELETE FROM microvm_launches WHERE intent_id = 'intent-one' RETURNING attempt_id",
            &[],
        )
        .expect("unreserved attempt exists")
        .get(0);
    let mut intent = launch_intent(attempt, "guarded");
    assert!(telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 0).is_err());
    intent.admission_cutoff = std::time::SystemTime::now() - Duration::from_secs(1);
    assert!(
        !telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 1)
            .expect("expired intent rejected")
    );
    intent.admission_cutoff = std::time::SystemTime::now() + Duration::from_secs(60);
    intent.backend_name = "different-backend".to_owned();
    assert!(
        !telchar::persistence::reserve_microvm_launch(fixture.url(), &intent, 1)
            .expect("mismatched backend rejected")
    );
    intent.backend_name = "launch-fixture".to_owned();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    fixture.expire_singleton_ownership("daemon");
    assert!(
        telchar::persistence::reserve_microvm_launch(owner.database_url(), &intent, 1).is_err()
    );
    let count: i64 = fixture
        .connect()
        .query_one("SELECT count(*) FROM microvm_launches", &[])
        .expect("no rejected reservation persisted")
        .get(0);
    assert_eq!(count, 0);
}

#[test]
fn cleanup_obligation_survives_terminal_build_and_releases_only_exact_identity() {
    let mut fixture = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(fixture.url(), "intent-one", "microvm-exact")
            .expect("binding")
    );
    assert!(
        telchar::persistence::pending_microvm_cleanup(fixture.url(), "launch-fixture", 10)
            .expect("active launch reads")
            .is_empty()
    );
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv",
        "fixture-failure",
        &serde_json::json!({}),
        Duration::from_secs(3600),
    )
    .expect("terminal build");
    fixture.restart();
    let cleanup =
        telchar::persistence::pending_microvm_cleanup(fixture.url(), "launch-fixture", 10)
            .expect("cleanup reads");
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].intent_id, "intent-one");
    assert_eq!(cleanup[0].provider_id, "microvm-exact");
    assert_eq!(cleanup[0].account_id, "116392764028");
    assert_eq!(cleanup[0].region, "us-west-2");
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires cleanup");
    fixture.expire_singleton_ownership("daemon");
    assert!(
        telchar::persistence::record_microvm_termination(
            owner.database_url(),
            "intent-one",
            "microvm-exact",
        )
        .is_err(),
        "expired owner cannot release capacity"
    );
    assert!(
        !telchar::persistence::record_microvm_termination(
            fixture.url(),
            "intent-one",
            "microvm-other"
        )
        .expect("wrong identity rejected")
    );
    assert!(
        telchar::persistence::record_microvm_termination(
            fixture.url(),
            "intent-one",
            "microvm-exact"
        )
        .expect("exact observation recorded")
    );
    assert!(
        !telchar::persistence::record_microvm_termination(
            fixture.url(),
            "intent-one",
            "microvm-exact"
        )
        .expect("duplicate observation has no release")
    );
    fixture.restart();
    assert!(
        telchar::persistence::pending_microvm_cleanup(fixture.url(), "launch-fixture", 10)
            .expect("released list reads")
            .is_empty()
    );
    let row = fixture.connect().query_one("SELECT phase, reservation_released_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).expect("release persists");
    assert_eq!(row.get::<_, String>(0), "closed");
    assert!(row.get::<_, bool>(1));
}

#[test]
fn exact_binding_is_single_assignment_and_late_response_is_cleanup_only() {
    for cancelled in [false, true] {
        let mut fixture = prepared_launch();
        assert!(
            !telchar::persistence::bind_microvm_launch(
                fixture.url(),
                "intent-one",
                "microvm-before-dispatch"
            )
            .expect("prepared launch rejects binding")
        );
        assert!(
            telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
                .expect("launch dispatches")
        );
        if cancelled {
            assert!(
                telchar::persistence::cancel_microvm_launch(fixture.url(), "intent-one")
                    .expect("launch cancels before API response")
            );
        }
        assert!(
            telchar::persistence::bind_microvm_launch(fixture.url(), "intent-one", "microvm-exact")
                .expect("exact API response persists")
        );
        assert!(
            !telchar::persistence::bind_microvm_launch(
                fixture.url(),
                "intent-one",
                "microvm-other"
            )
            .expect("different identity cannot replace binding")
        );
        fixture.restart();
        let row = fixture.connect().query_one(
            "SELECT phase, provider_id, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
        ).expect("binding survives restart");
        assert_eq!(
            row.get::<_, String>(0),
            if cancelled { "closed" } else { "bound" }
        );
        assert_eq!(row.get::<_, String>(1), "microvm-exact");
        assert!(row.get::<_, bool>(2));
    }
}

#[test]
fn binding_after_cutoff_or_terminal_build_records_cleanup_without_admission() {
    for terminal in [false, true] {
        let fixture = prepared_launch();
        assert!(
            telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
                .expect("launch dispatches")
        );
        if terminal {
            telchar::persistence::complete_shared_build_failure(
                fixture.url(),
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv",
                "launch-expired",
                &serde_json::json!({}),
                Duration::from_secs(3600),
            )
            .expect("build terminates before response");
        } else {
            fixture.connect().execute(
                "UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 minutes', admission_cutoff = clock_timestamp() - interval '1 minute' WHERE intent_id = 'intent-one'", &[],
            ).expect("admission cutoff expires");
        }
        assert!(
            telchar::persistence::bind_microvm_launch(fixture.url(), "intent-one", "microvm-late")
                .expect("late response retained for cleanup")
        );
        let row = fixture.connect().query_one(
            "SELECT phase, provider_id, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
        ).expect("late binding reads");
        assert_eq!(row.get::<_, String>(0), "closed");
        assert_eq!(row.get::<_, String>(1), "microvm-late");
        assert!(row.get::<_, bool>(2));
    }
}

#[test]
fn concurrent_binding_records_only_one_exact_identity() {
    let fixture = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(fixture.url(), "intent-one")
            .expect("launch dispatches")
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = ["microvm-first", "microvm-second"]
        .into_iter()
        .map(|id| {
            let url = fixture.url().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (
                    id,
                    telchar::persistence::bind_microvm_launch(&url, "intent-one", id)
                        .expect("binding resolves"),
                )
            })
        })
        .collect();
    let results: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("binding joins"))
        .collect();
    assert_eq!(results.iter().filter(|(_, bound)| *bound).count(), 1);
    let row = fixture
        .connect()
        .query_one(
            "SELECT provider_id FROM microvm_launches WHERE intent_id = 'intent-one'",
            &[],
        )
        .expect("winner reads");
    assert_eq!(
        row.get::<_, String>(0),
        results
            .iter()
            .find(|(_, bound)| *bound)
            .expect("one winner")
            .0
    );
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
