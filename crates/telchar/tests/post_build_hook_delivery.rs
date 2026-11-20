//! Tests atomic hook delivery scheduling for durable successful executions.

use std::time::Duration;

use telchar::backend::BackendKind;
use telchar::persistence::{self, Database, SharedBuildState};
use telchar_test_support::postgres::PostgresFixture;

const DERIVATION: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hook_delivery.drv";
const OUTPUT: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hook_delivery";

fn collecting(database: &Database) {
    persistence::claim_shared_build(
        database,
        DERIVATION,
        &[1; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[OUTPUT],
    )
    .unwrap();
    persistence::start_shared_build(database, DERIVATION).unwrap();
    persistence::collect_shared_build(database, DERIVATION).unwrap();
    for root in persistence::prepare_post_build_hook_roots(database, DERIVATION).unwrap() {
        persistence::retain_post_build_hook_root(database, &root).unwrap();
    }
}

#[test]
fn full_backlog_abandons_hook_delivery_without_changing_success() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    fixture
        .connect()
        .batch_execute(
            "INSERT INTO shared_build_attempts
             (derivation_path, ordinal, backend_name, backend_kind, state, completed_at)
         SELECT '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hook_delivery.drv', n + 1,
             'local', 'local', 'succeeded', now() FROM generate_series(1,1024) n;
         INSERT INTO post_build_hooks_outbox (attempt_id, output_paths)
         SELECT attempt_id, ARRAY['/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hook_delivery']
         FROM shared_build_attempts WHERE ordinal > 1;",
        )
        .unwrap();
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({"status":"built"}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        persistence::post_build_hooks_backlog(&database).unwrap().0,
        1024
    );
    let row = fixture
        .connect()
        .query_one(
            "SELECT p.state, p.last_failure_class FROM post_build_hooks_outbox p
         JOIN shared_build_attempts a USING (attempt_id) WHERE a.ordinal = 1",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "abandoned");
    assert_eq!(row.get::<_, String>(1), "backlog-full");
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
    assert_eq!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn disabling_hook_delivery_abandons_pending_and_releases_roots() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let claim =
        persistence::claim_post_build_hooks(&database, "interrupted", Duration::from_secs(30))
            .unwrap()
            .unwrap();
    assert_eq!(
        persistence::abandon_disabled_post_build_hooks(&database).unwrap(),
        1
    );
    assert_eq!(
        persistence::abandon_disabled_post_build_hooks(&database).unwrap(),
        0
    );
    assert!(!persistence::acknowledge_post_build_hooks(&database, &claim).unwrap());
    assert_eq!(
        persistence::post_build_hooks_backlog(&database).unwrap().0,
        0
    );
    assert_eq!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn success_schedules_exact_outputs_atomically_and_rejects_repeated_completion() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        persistence::complete_shared_build_success(
            &database,
            DERIVATION,
            &serde_json::json!({"status": "built"}),
            Duration::from_secs(60),
        )
        .is_err()
    );
    let rows = fixture
        .connect()
        .query("SELECT output_paths FROM post_build_hooks_outbox", &[])
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, Vec<String>>(0), [OUTPUT]);
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn hook_delivery_claim_is_exclusive_and_retry_cannot_change_build_result() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(60),
    )
    .unwrap();
    let first = persistence::claim_post_build_hooks(&database, "first", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(first.output_paths, [OUTPUT]);
    assert_eq!(first.attempt_count, 1);
    assert!(
        persistence::claim_post_build_hooks(&database, "second", Duration::from_secs(30))
            .unwrap()
            .is_none()
    );
    let mut stale = first.clone();
    stale.claim_token = "stale".to_owned();
    assert!(!persistence::acknowledge_post_build_hooks(&database, &stale).unwrap());
    assert!(
        persistence::retry_post_build_hooks(
            &database,
            &first,
            "hook-failed",
            Duration::from_secs(60),
        )
        .unwrap()
    );
    assert!(
        persistence::claim_post_build_hooks(&database, "second", Duration::from_secs(30))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
    fixture.connect().execute(
        "UPDATE post_build_hooks_outbox SET next_attempt_at = clock_timestamp() - interval '1 second'",
        &[],
    ).unwrap();
    let second = persistence::claim_post_build_hooks(&database, "second", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(second.attempt_count, 2);
    assert!(persistence::acknowledge_post_build_hooks(&database, &second).unwrap());
    assert!(
        persistence::claim_post_build_hooks(&database, "third", Duration::from_secs(30))
            .unwrap()
            .is_none()
    );
}

#[test]
fn hook_delivery_insert_failure_rolls_back_success_and_attempt_outcome() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    fixture
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_hook_delivery() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'hook_delivery unavailable'; END $$;
         CREATE TRIGGER reject_hook_delivery BEFORE INSERT ON post_build_hooks_outbox
         FOR EACH ROW EXECUTE FUNCTION reject_hook_delivery()",
        )
        .unwrap();
    assert!(
        persistence::complete_shared_build_success(
            &database,
            DERIVATION,
            &serde_json::json!({}),
            Duration::from_secs(60),
        )
        .is_err()
    );
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Collecting,
    );
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT count(*) FROM shared_build_attempt_outcomes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0,
    );
}

#[test]
fn hook_delivery_root_intent_survives_restart_and_cleanup_waits_for_terminal_result() {
    let mut fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    let roots = persistence::prepare_post_build_hook_roots(&database, DERIVATION).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].store_path, OUTPUT);
    persistence::retain_post_build_hook_root(&database, &roots[0]).unwrap();
    drop(database);
    fixture.restart();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    assert_eq!(
        persistence::prepare_post_build_hook_roots(&database, DERIVATION).unwrap(),
        roots
    );
    assert!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .is_empty()
    );
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .is_empty()
    );
    let hook_delivery =
        persistence::claim_post_build_hooks(&database, "publisher", Duration::from_secs(30))
            .unwrap()
            .unwrap();
    persistence::acknowledge_post_build_hooks(&database, &hook_delivery).unwrap();
    assert_eq!(
        persistence::read_post_build_hook_root_cleanup(&database, 256).unwrap(),
        roots
    );
    assert!(persistence::release_post_build_hook_root(&database, &roots[0]).unwrap());
    assert!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn expired_owner_cannot_claim_or_acknowledge_hook_delivery() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    let database = owner.database().clone().with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let claim = persistence::claim_post_build_hooks(&database, "first", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    fixture.expire_singleton_ownership("daemon");
    let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(persistence::acknowledge_post_build_hooks(&database, &claim).is_err());
    assert!(
        persistence::claim_post_build_hooks(&database, "stale", Duration::from_secs(30)).is_err()
    );
    replacement.verify().unwrap();
}

#[test]
fn uploaded_hook_delivery_is_repeated_after_unacknowledged_claim_expires() {
    let mut fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(60),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let published = directory.path().join("uploads");
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/sh",
        [
            "-c",
            "cat >> \"$1\"",
            "publish",
            published.to_str().unwrap(),
        ],
        Duration::from_secs(5),
        1024,
    )
    .unwrap();
    let interrupted =
        persistence::claim_post_build_hooks(&database, "interrupted", Duration::from_secs(30))
            .unwrap()
            .unwrap();
    hook.run(&interrupted.output_paths).unwrap();
    // Upload succeeded; the durable acknowledgment has not happened.
    drop(database);
    fixture.restart();
    fixture.connect().execute(
        "UPDATE post_build_hooks_outbox SET claim_expires_at = clock_timestamp() - interval '1 second'", &[],
    ).unwrap();
    assert!(
        telchar::service::hook_delivery::run_attempt(
            fixture.url(),
            &hook,
            Duration::from_secs(30),
            telchar::service::hook_delivery::HookDeliveryPolicy::default(),
            None,
        )
        .unwrap()
    );
    let uploads = std::fs::read_to_string(published).unwrap();
    let uploads: Vec<Vec<String>> = uploads
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(uploads, [vec![OUTPUT.to_owned()], vec![OUTPUT.to_owned()]]);
    assert!(!persistence::acknowledge_post_build_hooks(fixture.url(), &interrupted).unwrap());
    assert_eq!(
        persistence::read_shared_build(fixture.url(), DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn publisher_runs_real_hook_after_database_restart() {
    let mut fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    drop(database);
    fixture.restart();
    let directory = tempfile::tempdir().unwrap();
    let published = directory.path().join("published.json");
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/sh",
        ["-c", "cat > \"$1\"", "publish", published.to_str().unwrap()],
        Duration::from_secs(5),
        1024,
    )
    .unwrap();
    assert!(
        telchar::service::hook_delivery::run_attempt(
            fixture.url(),
            &hook,
            Duration::from_secs(30),
            telchar::service::hook_delivery::HookDeliveryPolicy::default(),
            None,
        )
        .unwrap()
    );
    let outputs: Vec<String> = serde_json::from_slice(&std::fs::read(&published).unwrap()).unwrap();
    assert_eq!(outputs, [OUTPUT]);
    assert!(
        !telchar::service::hook_delivery::run_attempt(
            fixture.url(),
            &hook,
            Duration::from_secs(30),
            telchar::service::hook_delivery::HookDeliveryPolicy::default(),
            None,
        )
        .unwrap()
    );
    assert_eq!(
        persistence::read_shared_build(fixture.url(), DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn startup_abandons_missing_output_without_changing_success() {
    use telchar::store::retention::{
        ReleasedRetentionEntry, RetainedPath, RetentionEntry, StoreRetentionBackend,
    };
    struct Store {
        directory: std::path::PathBuf,
    }
    impl StoreRetentionBackend for Store {
        fn hook_delivery_lock(&self) -> std::io::Result<std::fs::File> {
            std::fs::File::open(&self.directory)
        }
        fn retain(&mut self, _: &[RetentionEntry]) -> std::io::Result<Vec<RetainedPath>> {
            Err(std::io::Error::other("path unavailable"))
        }
        fn rollback(&mut self, _: &[RetainedPath]) -> std::io::Result<()> {
            unreachable!()
        }
        fn release(&mut self, _: &[ReleasedRetentionEntry]) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Missing;
    impl telchar::store::query::QueryValidPathsStore for Missing {
        fn query_valid_paths(&mut self, _: &[Vec<u8>], _: bool) -> std::io::Result<Vec<Vec<u8>>> {
            Ok(Vec::new())
        }
        fn query_missing(
            &mut self,
            _: &[Vec<u8>],
        ) -> std::io::Result<telchar::store::query::MissingPaths> {
            unreachable!()
        }
    }
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut retention = Store {
        directory: directory.path().to_owned(),
    };
    telchar::service::hook_delivery::reconcile_pending_roots(
        &database,
        &mut retention,
        Some(&mut Missing),
    )
    .unwrap();
    assert_eq!(
        persistence::post_build_hooks_backlog(&database).unwrap().0,
        0
    );
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT last_failure_class FROM post_build_hooks_outbox",
                &[]
            )
            .unwrap()
            .get::<_, String>(0),
        "missing-path"
    );
    telchar::service::hook_delivery::reconcile_released_roots(&database, &mut retention).unwrap();
    assert!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn missing_output_abandons_without_running_hook() {
    struct Missing;
    impl telchar::store::query::QueryValidPathsStore for Missing {
        fn query_valid_paths(&mut self, _: &[Vec<u8>], _: bool) -> std::io::Result<Vec<Vec<u8>>> {
            Ok(Vec::new())
        }
        fn query_missing(
            &mut self,
            _: &[Vec<u8>],
        ) -> std::io::Result<telchar::store::query::MissingPaths> {
            unreachable!()
        }
    }
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("hook-ran");
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/sh",
        ["-c", "touch \"$1\"", "publish", marker.to_str().unwrap()],
        Duration::from_secs(1),
        1024,
    )
    .unwrap();
    assert!(
        telchar::service::hook_delivery::run_attempt(
            &database,
            &hook,
            Duration::from_secs(30),
            Default::default(),
            Some(&mut Missing)
        )
        .unwrap()
    );
    assert!(!marker.exists());
    let row = fixture
        .connect()
        .query_one(
            "SELECT state, last_failure_class FROM post_build_hooks_outbox",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "abandoned");
    assert_eq!(row.get::<_, String>(1), "missing-path");
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn failing_hook_backs_off_then_abandons_without_changing_success() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/false",
        [] as [&str; 0],
        Duration::from_secs(1),
        1024,
    )
    .unwrap();
    let policy = telchar::service::hook_delivery::HookDeliveryPolicy {
        maximum_attempts: 1,
        ..Default::default()
    };
    assert!(
        telchar::service::hook_delivery::run_attempt(
            &database,
            &hook,
            Duration::from_secs(30),
            policy,
            None
        )
        .unwrap()
    );
    assert!(
        !telchar::service::hook_delivery::run_attempt(
            &database,
            &hook,
            Duration::from_secs(30),
            policy,
            None
        )
        .unwrap()
    );
    let row = fixture.connect().query_one(
        "SELECT state, next_attempt_at > clock_timestamp(), last_failure_class FROM post_build_hooks_outbox", &[],
    ).unwrap();
    assert_eq!(row.get::<_, String>(0), "pending");
    assert!(row.get::<_, bool>(1));
    assert_eq!(row.get::<_, String>(2), "hook-failed");
    fixture.connect().execute("UPDATE post_build_hooks_outbox SET next_attempt_at = clock_timestamp() - interval '1 second'", &[]).unwrap();
    assert!(
        telchar::service::hook_delivery::run_attempt(
            &database,
            &hook,
            Duration::from_secs(30),
            policy,
            None
        )
        .unwrap()
    );
    let row = fixture
        .connect()
        .query_one(
            "SELECT state, last_failure_class FROM post_build_hooks_outbox",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "abandoned");
    assert_eq!(row.get::<_, String>(1), "retry-exhausted");
    assert_eq!(
        persistence::read_shared_build(&database, DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn hook_delivery_cleanup_waits_for_root_preparation() {
    use telchar::store::retention::{
        ReleasedRetentionEntry, RetainedPath, RetentionEntry, StoreRetentionBackend,
    };
    struct Retention {
        directory: std::path::PathBuf,
        entered: Option<std::sync::mpsc::Sender<()>>,
        resume: Option<std::sync::mpsc::Receiver<()>>,
    }
    impl StoreRetentionBackend for Retention {
        fn hook_delivery_lock(&self) -> std::io::Result<std::fs::File> {
            let file = std::fs::File::open(&self.directory)?;
            rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
            Ok(file)
        }
        fn retain(&mut self, _: &[RetentionEntry]) -> std::io::Result<Vec<RetainedPath>> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
            }
            if let Some(resume) = self.resume.take() {
                resume.recv().unwrap();
            }
            std::fs::write(self.directory.join("root"), b"protected")?;
            Ok(Vec::new())
        }
        fn rollback(&mut self, _: &[RetainedPath]) -> std::io::Result<()> {
            Ok(())
        }
        fn release(&mut self, _: &[ReleasedRetentionEntry]) -> std::io::Result<()> {
            match std::fs::remove_file(self.directory.join("root")) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }
    }
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url()).unwrap();
    collecting(&database);
    let directory = tempfile::tempdir().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let preparation_database = database.clone();
    let mut preparation_retention = Retention {
        directory: directory.path().to_owned(),
        entered: Some(entered_tx),
        resume: Some(resume_rx),
    };
    let preparation = std::thread::spawn(move || {
        telchar::service::hook_delivery::prepare_outputs(
            &preparation_database,
            DERIVATION,
            &mut preparation_retention,
            &[OUTPUT.to_owned()],
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    persistence::complete_shared_build_failure(
        &database,
        DERIVATION,
        "cancelled",
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let cleanup_database = database.clone();
    let mut cleanup_retention = Retention {
        directory: directory.path().to_owned(),
        entered: None,
        resume: None,
    };
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        let result = telchar::service::hook_delivery::reconcile_released_roots(
            &cleanup_database,
            &mut cleanup_retention,
        );
        finished_tx.send(()).unwrap();
        result
    });
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    resume_tx.send(()).unwrap();
    preparation.join().unwrap().unwrap();
    cleanup.join().unwrap().unwrap();
    assert!(!directory.path().join("root").exists());
    assert!(
        persistence::read_post_build_hook_root_cleanup(&database, 256)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn disabled_completion_releases_prepared_roots_without_creating_hook_delivery() {
    let fixture = PostgresFixture::start();
    persistence::migrate(fixture.url()).unwrap();
    let database = Database::connect(fixture.url()).unwrap();
    collecting(&database);
    persistence::complete_shared_build_success(
        &database,
        DERIVATION,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let cleanup = persistence::read_post_build_hook_root_cleanup(&database, 256).unwrap();
    assert_eq!(cleanup.len(), 1);
    assert!(persistence::release_post_build_hook_root(&database, &cleanup[0]).unwrap());
    assert_eq!(
        persistence::post_build_hooks_backlog(&database).unwrap().0,
        0
    );
}

#[test]
fn disabled_hook_delivery_and_failed_builds_schedule_nothing() {
    for enabled in [false, true] {
        let fixture = PostgresFixture::start();
        persistence::migrate(fixture.url()).unwrap();
        let database = Database::connect(fixture.url())
            .unwrap()
            .with_post_build_hooks(enabled);
        collecting(&database);
        if enabled {
            persistence::complete_shared_build_failure(
                &database,
                DERIVATION,
                "build-failure",
                &serde_json::json!({}),
                Duration::from_secs(60),
            )
            .unwrap();
        } else {
            persistence::complete_shared_build_success(
                &database,
                DERIVATION,
                &serde_json::json!({}),
                Duration::from_secs(60),
            )
            .unwrap();
        }
        assert_eq!(
            fixture
                .connect()
                .query_one("SELECT count(*) FROM post_build_hooks_outbox", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}
