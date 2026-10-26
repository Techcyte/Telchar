//! Exercises durable MicroVM launch identity and ownership with PostgreSQL.

use telchar_test_support::postgres::PostgresFixture;

use std::time::Duration;

use telchar::backend::BackendKind;

fn prepared_launch() -> (PostgresFixture, telchar::persistence::Database) {
    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    telchar::persistence::migrate(&database).expect("database migrates");
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv";
    telchar::persistence::claim_shared_build(
        &database,
        derivation,
        &[1; 32],
        "launch-fixture",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-launch"],
    )
    .expect("fixture build claims");
    telchar::persistence::start_shared_build(&database, derivation)
        .expect("fixture attempt starts");
    let attempt = telchar::persistence::read_shared_build_attempt(&database, derivation)
        .expect("attempt reads")
        .expect("attempt exists");
    assert!(
        telchar::persistence::reserve_microvm_launch(
            &database,
            &launch_intent(attempt.attempt_id, "intent-one"),
            1,
        )
        .expect("launch reserves durable capacity")
    );
    (fixture, database)
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
        vpc_egress_connector_arn: "arn:aws:lambda:us-west-2:116392764028:network-connector:builder"
            .into(),
        configuration_digest: [1; 32],
        request_digest: [2; 32],
        client_token: intent_id.to_owned(),
        credential_verifier: [3; 32],
        admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
        maximum_duration_seconds: 600,
    }
}

#[test]
fn administrative_termination_commits_observation_and_audit_atomically() {
    for risk_released in [false, true] {
        let (fixture, database) = prepared_launch();
        assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
        assert!(
            telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
                .unwrap()
        );
        assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
        let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(60),
        )
        .unwrap();
        if risk_released {
            assert!(
                telchar::persistence::release_microvm_capacity_risk(
                    owner.database(),
                    &telchar::persistence::MicrovmRiskRelease {
                        intent_id: "intent-one",
                        provider_id: Some("microvm-exact"),
                        actor_uid: 0,
                        reason: "Capacity risk accepted",
                        evidence_reference: "incident:risk",
                        accept_remote_execution_risk: true,
                    }
                )
                .unwrap()
            );
        }
        let mut client = fixture.connect();
        let released: Option<std::time::SystemTime> = client.query_one(
            "SELECT reservation_released_at FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
        ).unwrap().get(0);
        let mut observation = telchar::persistence::MicrovmTerminationObservation {
            intent_id: "intent-one",
            provider_id: "microvm-exact",
            account_id: "116392764028",
            region: "us-west-2",
            actor_uid: 0,
            reason: "Exact provider termination verified",
            evidence_reference: "incident:termination",
        };
        assert!(
            telchar::persistence::resolve_microvm_termination(&database, &observation).is_err()
        );
        observation.actor_uid = 1000;
        assert!(
            telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .is_err()
        );
        observation.actor_uid = 0;
        observation.provider_id = "microvm-other";
        assert!(
            !telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .unwrap()
        );
        observation.provider_id = "microvm-exact";
        observation.region = "us-east-1";
        assert!(
            !telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .unwrap()
        );
        observation.region = "us-west-2";
        client.batch_execute("CREATE TRIGGER fixture_reject_audit BEFORE INSERT ON microvm_resolutions FOR EACH STATEMENT EXECUTE FUNCTION preserve_microvm_resolution()").unwrap();
        assert!(
            telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .is_err()
        );
        let row = client.query_one("SELECT reservation_released_at, termination_observed_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).unwrap();
        assert_eq!(row.get::<_, Option<std::time::SystemTime>>(0), released);
        assert!(row.get::<_, bool>(1));
        client
            .batch_execute("DROP TRIGGER fixture_reject_audit ON microvm_resolutions")
            .unwrap();
        assert!(
            telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .unwrap()
        );
        assert!(
            !telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .unwrap()
        );
        let row = client.query_one("SELECT phase, reservation_released_at, termination_observed_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).unwrap();
        assert_eq!(row.get::<_, String>(0), "closed");
        let final_release = row.get::<_, std::time::SystemTime>(1);
        if let Some(released) = released {
            assert_eq!(final_release, released);
        }
        assert!(row.get::<_, bool>(2));
        assert!(telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
        let audits = client.query("SELECT provider_id, actor_uid, owner_generation, reason, evidence_reference FROM microvm_resolutions WHERE action = 'verify-termination'", &[]).unwrap();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].get::<_, String>(0), "microvm-exact");
        assert_eq!(audits[0].get::<_, i64>(1), 0);
        assert_eq!(audits[0].get::<_, i64>(2), owner.generation());
        assert_eq!(audits[0].get::<_, String>(3), observation.reason);
        assert_eq!(
            audits[0].get::<_, String>(4),
            observation.evidence_reference
        );
        client.execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second'", &[]).unwrap();
        assert!(
            telchar::persistence::resolve_microvm_termination(owner.database(), &observation)
                .is_err()
        );
    }
}

#[test]
fn administrative_attachment_preserves_release_and_denies_admission() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        telchar::persistence::release_microvm_capacity_risk(
            owner.database(),
            &telchar::persistence::MicrovmRiskRelease {
                intent_id: "intent-one",
                provider_id: None,
                actor_uid: 0,
                reason: "Unresolved launch risk accepted",
                evidence_reference: "incident:attachment",
                accept_remote_execution_risk: true,
            }
        )
        .unwrap()
    );
    let mut client = fixture.connect();
    let released: std::time::SystemTime = client
        .query_one(
            "SELECT reservation_released_at FROM microvm_launches WHERE intent_id = 'intent-one'",
            &[],
        )
        .unwrap()
        .get(0);
    let mut attachment = telchar::persistence::MicrovmIdentityAttachment {
        intent_id: "intent-one",
        provider_id: "microvm-exact",
        actor_uid: 0,
        account_id: "116392764028",
        region: "us-west-2",
        image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
        image_version: "1",
        reason: "Recovered exact launch response",
        evidence_reference: "incident:response",
    };
    assert!(telchar::persistence::attach_microvm_cleanup_identity(&database, &attachment).is_err());
    attachment.image_version = "2";
    assert!(
        !telchar::persistence::attach_microvm_cleanup_identity(owner.database(), &attachment)
            .unwrap()
    );
    attachment.image_version = "1";
    client.batch_execute("CREATE TRIGGER fixture_reject_audit BEFORE INSERT ON microvm_resolutions FOR EACH STATEMENT EXECUTE FUNCTION preserve_microvm_resolution()").unwrap();
    assert!(
        telchar::persistence::attach_microvm_cleanup_identity(owner.database(), &attachment)
            .is_err()
    );
    assert!(
        client
            .query_one(
                "SELECT provider_id IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'",
                &[]
            )
            .unwrap()
            .get::<_, bool>(0)
    );
    client
        .batch_execute("DROP TRIGGER fixture_reject_audit ON microvm_resolutions")
        .unwrap();
    assert!(
        telchar::persistence::attach_microvm_cleanup_identity(owner.database(), &attachment)
            .unwrap()
    );
    assert!(
        !telchar::persistence::attach_microvm_cleanup_identity(owner.database(), &attachment)
            .unwrap()
    );
    assert!(
        telchar::persistence::pending_microvm_cleanup_identity(
            &database,
            "intent-one",
            "microvm-exact"
        )
        .unwrap()
    );
    assert!(
        !telchar::persistence::admit_microvm_session(
            owner.database(),
            "intent-one",
            "microvm-exact",
            &[3; 32]
        )
        .unwrap()
    );
    let row = client.query_one("SELECT phase, reservation_released_at, termination_observed_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "closed");
    assert_eq!(row.get::<_, std::time::SystemTime>(1), released);
    assert!(row.get::<_, bool>(2));
    let audit = client.query_one("SELECT provider_id, actor_uid, owner_generation, evidence_reference FROM microvm_resolutions WHERE action = 'attach-identity'", &[]).unwrap();
    assert_eq!(audit.get::<_, String>(0), "microvm-exact");
    assert_eq!(audit.get::<_, i64>(1), 0);
    assert_eq!(audit.get::<_, i64>(2), owner.generation());
    assert_eq!(audit.get::<_, String>(3), "incident:response");
    client.execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second'", &[]).unwrap();
    assert!(
        telchar::persistence::attach_microvm_cleanup_identity(owner.database(), &attachment)
            .is_err()
    );
}

#[test]
fn concurrent_identity_attachment_keeps_one_cleanup_obligation() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let attach = |provider_id| {
        telchar::persistence::attach_microvm_cleanup_identity(
            owner.database(),
            &telchar::persistence::MicrovmIdentityAttachment {
                intent_id: "intent-one",
                provider_id,
                actor_uid: 0,
                account_id: "116392764028",
                region: "us-west-2",
                image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
                image_version: "1",
                reason: "Recovered exact launch response",
                evidence_reference: "incident:concurrent-response",
            },
        )
        .unwrap()
    };
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            attach("microvm-first")
        });
        barrier.wait();
        let second = attach("microvm-second");
        [first.join().unwrap(), second]
    });
    assert_eq!(results.into_iter().filter(|changed| *changed).count(), 1);
    let winner = if results[0] {
        "microvm-first"
    } else {
        "microvm-second"
    };
    let mut client = fixture.connect();
    let row = client.query_one(
        "SELECT provider_id, phase, reservation_released_at IS NULL,
                termination_observed_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).unwrap();
    assert_eq!(row.get::<_, String>(0), winner);
    assert_eq!(row.get::<_, String>(1), "closed");
    assert!(row.get::<_, bool>(2));
    assert!(row.get::<_, bool>(3));
    let audits = client
        .query(
            "SELECT provider_id FROM microvm_resolutions WHERE action = 'attach-identity'",
            &[],
        )
        .unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].get::<_, String>(0), winner);
    assert!(
        !telchar::persistence::admit_microvm_session(
            owner.database(),
            "intent-one",
            winner,
            &[3; 32]
        )
        .unwrap()
    );
    assert!(!telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
    assert!(
        telchar::persistence::pending_microvm_cleanup_identity(&database, "intent-one", winner)
            .unwrap()
    );
    assert!(
        telchar::persistence::record_microvm_termination(owner.database(), "intent-one", winner)
            .unwrap()
    );
    assert!(telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
}

#[test]
fn risk_release_bounds_lock_contention_without_releasing_capacity() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let mut blocker = database.connection().unwrap();
    for query in [
        "SELECT intent_id FROM microvm_launches WHERE intent_id = 'intent-one' FOR UPDATE",
        "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon' FOR UPDATE",
    ] {
        let mut transaction = blocker.transaction().unwrap();
        transaction.query(query, &[]).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let result = telchar::persistence::release_microvm_capacity_risk(
                    owner.database(),
                    &telchar::persistence::MicrovmRiskRelease {
                        intent_id: "intent-one",
                        provider_id: None,
                        actor_uid: 0,
                        reason: "Remote execution risk accepted",
                        evidence_reference: "incident:contention",
                        accept_remote_execution_risk: true,
                    },
                );
                sender.send(result).unwrap();
            });
            let result = receiver.recv_timeout(Duration::from_secs(2));
            transaction.rollback().unwrap();
            assert!(
                matches!(result, Ok(Err(_))),
                "request must reject while lock remains held: {result:?}"
            );
        });
        let row = blocker.query_one(
            "SELECT reservation_released_at IS NULL, (SELECT count(*) FROM microvm_resolutions) FROM microvm_launches WHERE intent_id = 'intent-one'",
            &[],
        ).unwrap();
        assert!(row.get::<_, bool>(0));
        assert_eq!(row.get::<_, i64>(1), 0);
        owner
            .renew()
            .expect("timed-out administration releases ownership locks");
    }
}

#[test]
fn risk_release_requires_owner_and_exact_identity_and_preserves_cleanup() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let mut release = telchar::persistence::MicrovmRiskRelease {
        intent_id: "intent-one",
        provider_id: Some("microvm-exact"),
        actor_uid: 0,
        reason: "Provider unavailable; remote cost accepted",
        evidence_reference: "incident:fixture-118",
        accept_remote_execution_risk: true,
    };
    assert!(telchar::persistence::release_microvm_capacity_risk(&database, &release).is_err());
    release.actor_uid = 1000;
    assert!(
        telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).is_err()
    );
    release.actor_uid = 0;
    release.accept_remote_execution_risk = false;
    assert!(
        telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).is_err()
    );
    release.accept_remote_execution_risk = true;
    release.provider_id = None;
    assert!(
        !telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).unwrap()
    );
    release.provider_id = Some("microvm-other");
    assert!(
        !telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).unwrap()
    );
    release.provider_id = Some("microvm-exact");
    assert!(
        telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).unwrap()
    );
    assert!(
        !telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).unwrap()
    );
    assert!(
        telchar::persistence::pending_microvm_cleanup_identity(
            &database,
            "intent-one",
            "microvm-exact"
        )
        .unwrap()
    );
    let mut client = fixture.connect();
    let rows = client.query(
        "SELECT intent_id, provider_id, actor_uid, owner_generation, reason, evidence_reference,
                recorded_at <= clock_timestamp() FROM microvm_resolutions", &[]
    ).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>(0), "intent-one");
    assert_eq!(rows[0].get::<_, String>(1), "microvm-exact");
    assert_eq!(rows[0].get::<_, i64>(2), 0);
    assert_eq!(rows[0].get::<_, i64>(3), owner.generation());
    assert_eq!(rows[0].get::<_, String>(4), release.reason);
    assert_eq!(rows[0].get::<_, String>(5), release.evidence_reference);
    assert!(rows[0].get::<_, bool>(6));
    for statement in [
        "UPDATE microvm_resolutions SET reason = 'changed'",
        "DELETE FROM microvm_resolutions",
        "TRUNCATE microvm_resolutions",
    ] {
        let error = client.batch_execute(statement).unwrap_err();
        assert_eq!(error.as_db_error().unwrap().code().code(), "55000");
    }
    client.execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second'", &[]).unwrap();
    assert!(
        telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).is_err()
    );
}

#[test]
fn ambiguous_risk_release_is_atomic_and_single_assignment() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(60),
    )
    .unwrap();
    let release = telchar::persistence::MicrovmRiskRelease {
        intent_id: "intent-one",
        provider_id: None,
        actor_uid: 0,
        reason: "Unresolved launch risk accepted",
        evidence_reference: "incident:atomic-118",
        accept_remote_execution_risk: true,
    };
    let mut client = fixture.connect();
    client
        .batch_execute(
            "CREATE TRIGGER fixture_reject_audit BEFORE INSERT ON microvm_resolutions
         FOR EACH STATEMENT EXECUTE FUNCTION preserve_microvm_resolution()",
        )
        .unwrap();
    assert!(
        telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).is_err()
    );
    assert!(client.query_one("SELECT reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).unwrap().get::<_, bool>(0));
    client
        .batch_execute("DROP TRIGGER fixture_reject_audit ON microvm_resolutions")
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            telchar::persistence::release_microvm_capacity_risk(owner.database(), &release).unwrap()
        });
        barrier.wait();
        let second =
            telchar::persistence::release_microvm_capacity_risk(owner.database(), &release)
                .unwrap();
        [first.join().unwrap(), second]
    });
    assert_eq!(results.into_iter().filter(|changed| *changed).count(), 1);
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM microvm_resolutions", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(client.query_one("SELECT phase = 'ambiguous' AND provider_id IS NULL AND termination_observed_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[]).unwrap().get::<_, bool>(0));
}

#[test]
fn verified_termination_is_distinct_from_capacity_release() {
    let (fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    fixture.connect().execute(
        "UPDATE microvm_launches SET reservation_released_at = clock_timestamp() WHERE intent_id = 'intent-one'",
        &[],
    ).unwrap();
    assert!(
        telchar::persistence::pending_microvm_cleanup_identity(
            &database,
            "intent-one",
            "microvm-exact"
        )
        .unwrap(),
        "capacity release must not erase a known execution cleanup obligation"
    );
    assert!(!telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
    let pending = telchar::persistence::pending_microvm_cleanup(
        &database,
        "116392764028",
        "us-west-2",
        1,
        None,
    )
    .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        !telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-other")
            .unwrap()
    );
    assert!(
        telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(
        !telchar::persistence::pending_microvm_cleanup_identity(
            &database,
            "intent-one",
            "microvm-exact"
        )
        .unwrap()
    );
    assert!(telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
}

#[test]
fn termination_migration_preserves_observation_and_ownership_fence() {
    let (mut fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(
        telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    fixture
        .connect()
        .batch_execute(
            "DROP TABLE microvm_resolutions;
         DROP FUNCTION preserve_microvm_resolution();
         ALTER TABLE microvm_launches DROP COLUMN termination_observed_at CASCADE;
         DELETE FROM telchar_schema_migrations WHERE version >= 21;",
        )
        .unwrap();
    assert_eq!(
        telchar::persistence::migrate(&database)
            .unwrap()
            .applied_this_run,
        4
    );
    fixture.restart();
    let row = fixture.connect().query_one(
        "SELECT termination_observed_at = reservation_released_at FROM microvm_launches WHERE intent_id = 'intent-one'",
        &[],
    ).unwrap();
    assert!(row.get::<_, bool>(0));
    assert!(
        !telchar::persistence::pending_microvm_cleanup_identity(
            &database,
            "intent-one",
            "microvm-exact"
        )
        .unwrap()
    );
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    fixture.connect().execute(
        "UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second'",
        &[],
    ).unwrap();
    assert!(
        telchar::persistence::record_microvm_termination(
            owner.database(),
            "intent-one",
            "microvm-exact"
        )
        .is_err(),
        "termination writes remain fenced even when no row would change"
    );
}

#[test]
fn cleanup_configuration_covers_unreleased_launches() {
    let (_fixture, database) = prepared_launch();
    for identities in [
        vec![],
        vec![("000000000000", "us-west-2")],
        vec![("116392764028", "us-east-1")],
    ] {
        assert!(
            !telchar::persistence::microvm_cleanup_is_configured(&database, &identities).unwrap()
        );
    }
    assert!(
        telchar::persistence::microvm_cleanup_is_configured(
            &database,
            &[("116392764028", "us-west-2")]
        )
        .unwrap()
    );
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    assert!(telchar::persistence::microvm_cleanup_is_configured(&database, &[]).unwrap());
}

#[test]
fn renamed_backend_retains_account_region_quarantine_across_restart() {
    let (mut fixture, database) = prepared_launch();
    assert!(telchar::persistence::dispatch_microvm_launch(&database, "intent-one").unwrap());
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(telchar::persistence::cancel_microvm_launch(&database, "intent-one").unwrap());
    let derivation = "/nix/store/cccccccccccccccccccccccccccccccc-alias.drv";
    telchar::persistence::claim_shared_build(
        &database,
        derivation,
        &[2; 32],
        "renamed",
        BackendKind::Microvm,
        BackendKind::Microvm.capabilities(),
        None,
        &["/nix/store/dddddddddddddddddddddddddddddddd-alias"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(&database, derivation).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(&database, derivation)
        .unwrap()
        .unwrap();
    let mut intent = launch_intent(attempt.attempt_id, "alias-intent");
    intent.backend_name = "renamed".into();
    fixture.restart();
    assert!(
        !telchar::persistence::reserve_microvm_launch(&database, &intent, 1).unwrap(),
        "renaming cannot bypass retained account/region reservation"
    );
    assert!(
        telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-exact")
            .unwrap()
    );
    assert!(telchar::persistence::reserve_microvm_launch(&database, &intent, 1).unwrap());
}

#[test]
fn runtime_capability_authenticates_one_fenced_session_without_persisting_bearer() {
    let (fixture, database) = prepared_launch();
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
        telchar::persistence::dispatch_microvm_launch(&database, "intent-one").expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
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
    let (mut fixture, database) = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(&database, "intent-one").expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .expect("bind")
    );
    for (id, verifier) in [("microvm-other", [3; 32]), ("microvm-exact", [4; 32])] {
        assert!(
            !telchar::persistence::admit_microvm_session(&database, "intent-one", id, &verifier)
                .expect("wrong credential rejected")
        );
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let database = database.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                telchar::persistence::admit_microvm_session(
                    &database,
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
            &database,
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
        let (fixture, database) = prepared_launch();
        if gate != "prepared" {
            assert!(
                telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
                    .expect("dispatch")
            );
            assert!(
                telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
                    .expect("bind")
            );
        }
        match gate {
            "expired" => {
                fixture.connect().batch_execute("UPDATE microvm_launches SET created_at = clock_timestamp() - interval '2 minutes', admission_cutoff = clock_timestamp() - interval '1 minute'").expect("expired fixture");
            }
            "cancelled" => {
                telchar::persistence::cancel_microvm_launch(&database, "intent-one")
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
                    &database,
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
            owner.database(),
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
    for (backend, account, region, expected) in [
        ("launch-fixture", "116392764028", "us-west-2", 1),
        ("alias", "116392764028", "us-west-2", 1),
        ("alias", "000000000000", "us-west-2", 2),
        ("alias", "116392764028", "us-east-1", 2),
        ("launch-fixture", "000000000000", "us-east-1", 1),
    ] {
        reservation_competitors(backend, account, region, expected);
    }
}

fn reservation_competitors(backend: &str, account: &str, region: &str, expected: usize) {
    let (mut fixture, database) = prepared_launch();
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
        &database,
        derivation,
        &[2; 32],
        backend,
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/dddddddddddddddddddddddddddddddd-other"],
    )
    .expect("second build claims");
    telchar::persistence::start_shared_build(&database, derivation).expect("second attempt starts");
    let second_attempt = telchar::persistence::read_shared_build_attempt(&database, derivation)
        .expect("second attempt reads")
        .expect("second attempt exists")
        .attempt_id;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first = launch_intent(first_attempt, "reserve-one");
    let mut second = launch_intent(second_attempt, "reserve-two");
    second.backend_name = backend.into();
    second.account_id = account.into();
    second.region = region.into();
    let callers: Vec<_> = [first, second]
        .into_iter()
        .map(|intent| {
            let database = database.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let reserved = telchar::persistence::reserve_microvm_launch(&database, &intent, 1)
                    .expect("capacity reservation resolves");
                (intent, reserved)
            })
        })
        .collect();
    let results: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("reservation joins"))
        .collect();
    assert_eq!(
        results.iter().filter(|(_, reserved)| *reserved).count(),
        expected
    );
    if expected == 2 {
        return;
    }
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
        telchar::persistence::dispatch_microvm_launch(&database, &winner.intent_id)
            .expect("winner dispatches")
    );
    assert!(
        telchar::persistence::cancel_microvm_launch(&database, &winner.intent_id)
            .expect("winner becomes ambiguous")
    );
    fixture.restart();
    assert!(
        !telchar::persistence::reserve_microvm_launch(&database, loser, 1)
            .expect("ambiguous reservation blocks capacity")
    );
    assert!(
        telchar::persistence::reserve_microvm_launch(&database, loser, 2)
            .expect("explicit second slot available")
    );
    assert!(
        !telchar::persistence::reserve_microvm_launch(&database, loser, 3)
            .expect("same attempt cannot reserve again")
    );
    assert!(
        telchar::persistence::cancel_microvm_launch(&database, &loser.intent_id)
            .expect("unused reservation releases")
    );
}

#[test]
fn reservation_rejects_expiry_backend_mismatch_and_stale_ownership() {
    let (fixture, database) = prepared_launch();
    let attempt: i64 = fixture
        .connect()
        .query_one(
            "DELETE FROM microvm_launches WHERE intent_id = 'intent-one' RETURNING attempt_id",
            &[],
        )
        .expect("unreserved attempt exists")
        .get(0);
    let mut intent = launch_intent(attempt, "guarded");
    assert!(telchar::persistence::reserve_microvm_launch(&database, &intent, 0).is_err());
    intent.admission_cutoff = std::time::SystemTime::now() - Duration::from_secs(1);
    assert!(
        !telchar::persistence::reserve_microvm_launch(&database, &intent, 1)
            .expect("expired intent rejected")
    );
    intent.admission_cutoff = std::time::SystemTime::now() + Duration::from_secs(60);
    intent.backend_name = "different-backend".to_owned();
    assert!(
        !telchar::persistence::reserve_microvm_launch(&database, &intent, 1)
            .expect("mismatched backend rejected")
    );
    intent.backend_name = "launch-fixture".to_owned();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    fixture.expire_singleton_ownership("daemon");
    assert!(telchar::persistence::reserve_microvm_launch(owner.database(), &intent, 1).is_err());
    let count: i64 = fixture
        .connect()
        .query_one("SELECT count(*) FROM microvm_launches", &[])
        .expect("no rejected reservation persisted")
        .get(0);
    assert_eq!(count, 0);
}

#[test]
fn cleanup_obligation_survives_terminal_build_and_releases_only_exact_identity() {
    let (mut fixture, database) = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(&database, "intent-one").expect("dispatch")
    );
    assert!(
        telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
            .expect("binding")
    );
    assert!(
        telchar::persistence::pending_microvm_cleanup(
            &database,
            "116392764028",
            "us-west-2",
            10,
            None
        )
        .expect("active launch reads")
        .is_empty()
    );
    telchar::persistence::complete_shared_build_failure(
        &database,
        "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv",
        "fixture-failure",
        &serde_json::json!({}),
        Duration::from_secs(3600),
    )
    .expect("terminal build");
    fixture.restart();
    let cleanup = telchar::persistence::pending_microvm_cleanup(
        &database,
        "116392764028",
        "us-west-2",
        10,
        None,
    )
    .expect("cleanup reads");
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].intent_id, "intent-one");
    assert_eq!(cleanup[0].provider_id, "microvm-exact");
    assert_eq!(cleanup[0].account_id, "116392764028");
    assert_eq!(cleanup[0].region, "us-west-2");
    for (account, region) in [("000000000000", "us-west-2"), ("116392764028", "us-east-1")] {
        assert!(
            telchar::persistence::pending_microvm_cleanup(&database, account, region, 10, None,)
                .unwrap()
                .is_empty(),
            "cleanup must not cross account/region authority"
        );
    }
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires cleanup");
    fixture.expire_singleton_ownership("daemon");
    assert!(
        telchar::persistence::record_microvm_termination(
            owner.database(),
            "intent-one",
            "microvm-exact",
        )
        .is_err(),
        "expired owner cannot release capacity"
    );
    assert!(
        !telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-other")
            .expect("wrong identity rejected")
    );
    assert!(
        telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-exact")
            .expect("exact observation recorded")
    );
    assert!(
        !telchar::persistence::record_microvm_termination(&database, "intent-one", "microvm-exact")
            .expect("duplicate observation has no release")
    );
    fixture.restart();
    assert!(
        telchar::persistence::pending_microvm_cleanup(
            &database,
            "116392764028",
            "us-west-2",
            10,
            None
        )
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
        let (mut fixture, database) = prepared_launch();
        assert!(
            !telchar::persistence::bind_microvm_launch(
                &database,
                "intent-one",
                "microvm-before-dispatch"
            )
            .expect("prepared launch rejects binding")
        );
        assert!(
            telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
                .expect("launch dispatches")
        );
        if cancelled {
            assert!(
                telchar::persistence::cancel_microvm_launch(&database, "intent-one")
                    .expect("launch cancels before API response")
            );
        }
        assert!(
            telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-exact")
                .expect("exact API response persists")
        );
        assert!(
            !telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-other")
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
        let (fixture, database) = prepared_launch();
        assert!(
            telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
                .expect("launch dispatches")
        );
        if terminal {
            telchar::persistence::complete_shared_build_failure(
                &database,
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
            telchar::persistence::bind_microvm_launch(&database, "intent-one", "microvm-late")
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
    let (fixture, database) = prepared_launch();
    assert!(
        telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
            .expect("launch dispatches")
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callers: Vec<_> = ["microvm-first", "microvm-second"]
        .into_iter()
        .map(|id| {
            let database = database.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (
                    id,
                    telchar::persistence::bind_microvm_launch(&database, "intent-one", id)
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
    let (fixture, database) = prepared_launch();
    assert!(
        telchar::persistence::cancel_microvm_launch(&database, "intent-one")
            .expect("prepared launch cancels")
    );
    assert!(
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
            .expect("cancelled launch cannot dispatch")
    );
    let row = fixture.connect().query_one(
        "SELECT phase, reservation_released_at IS NOT NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("cancelled launch reads");
    assert_eq!(row.get::<_, String>(0), "closed");
    assert!(row.get::<_, bool>(1));
    assert!(
        !telchar::persistence::cancel_microvm_launch(&database, "intent-one")
            .expect("duplicate cancellation has no transition")
    );
}

#[test]
fn cancellation_retains_dispatched_and_bound_reservations() {
    for provider_id in [None, Some("microvm-exact")] {
        let (mut fixture, database) = prepared_launch();
        assert!(
            telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
                .expect("launch dispatches")
        );
        if let Some(provider_id) = provider_id {
            fixture.connect().execute(
                "UPDATE microvm_launches SET phase = 'bound', provider_id = $1 WHERE intent_id = 'intent-one'", &[&provider_id],
            ).expect("API binding fixture records");
        }
        assert!(
            telchar::persistence::cancel_microvm_launch(&database, "intent-one")
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
    let (fixture, _database) = prepared_launch();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .expect("owner acquires");
    fixture.expire_singleton_ownership("daemon");
    assert!(telchar::persistence::cancel_microvm_launch(owner.database(), "intent-one").is_err());
    let row = fixture.connect().query_one(
        "SELECT phase, reservation_released_at IS NULL FROM microvm_launches WHERE intent_id = 'intent-one'", &[],
    ).expect("fenced launch reads");
    assert_eq!(row.get::<_, String>(0), "prepared");
    assert!(row.get::<_, bool>(1));
}

#[test]
fn cancellation_and_dispatch_race_preserves_ambiguous_capacity() {
    let (fixture, database) = prepared_launch();
    let dispatch_database = database.clone();
    let cancel_database = dispatch_database.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let dispatch_barrier = barrier.clone();
    let dispatch = std::thread::spawn(move || {
        dispatch_barrier.wait();
        telchar::persistence::dispatch_microvm_launch(&dispatch_database, "intent-one")
            .expect("racing dispatch resolves")
    });
    let cancel = std::thread::spawn(move || {
        barrier.wait();
        telchar::persistence::cancel_microvm_launch(&cancel_database, "intent-one")
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
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
            .expect("cancelled execution never redispatches")
    );
}

#[test]
fn launch_reservation_survives_build_failure_and_database_restart() {
    let (mut fixture, database) = prepared_launch();
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-launch.drv";
    fixture.connect().execute(
        "UPDATE microvm_launches SET reservation_released_at = clock_timestamp() WHERE intent_id = 'intent-one'", &[],
    ).expect("reservation releases for negative case");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
            .expect("released reservation cannot dispatch")
    );
    fixture.connect().execute(
        "UPDATE microvm_launches SET reservation_released_at = NULL, created_at = clock_timestamp() - interval '2 minutes', admission_cutoff = clock_timestamp() - interval '1 minute' WHERE intent_id = 'intent-one'", &[],
    ).expect("cutoff expires for negative case");
    assert!(
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
            .expect("expired intent cannot dispatch")
    );
    fixture.connect().execute(
        "UPDATE microvm_launches SET admission_cutoff = clock_timestamp() + interval '1 minute' WHERE intent_id = 'intent-one'", &[],
    ).expect("cutoff restores for concurrency case");
    let first_database = database.clone();
    let second_database = first_database.clone();
    let first = std::thread::spawn(move || {
        telchar::persistence::dispatch_microvm_launch(&first_database, "intent-one")
            .expect("first dispatch resolves")
    });
    let second = std::thread::spawn(move || {
        telchar::persistence::dispatch_microvm_launch(&second_database, "intent-one")
            .expect("second dispatch resolves")
    });
    assert_eq!(
        usize::from(first.join().expect("first caller joins"))
            + usize::from(second.join().expect("second caller joins")),
        1
    );
    telchar::persistence::complete_shared_build_failure(
        &database,
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
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
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
        !telchar::persistence::dispatch_microvm_launch(&database, "intent-one")
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
