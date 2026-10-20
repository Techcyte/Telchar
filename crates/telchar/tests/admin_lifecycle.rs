//! Exercises root administration through the active daemon and isolated PostgreSQL.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use telchar::backend::BackendKind;
use telchar_test_support::postgres::PostgresFixture;

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

#[test]
fn root_administration_commits_audit_and_cleans_up_socket() {
    root_contract(
        "root_administration_commits_audit_and_cleans_up_socket",
        Scenario::Success,
    );
}

#[test]
fn queued_administration_cannot_mutate_after_ownership_loss() {
    root_contract(
        "queued_administration_cannot_mutate_after_ownership_loss",
        Scenario::OwnershipLoss,
    );
}

#[test]
fn shutdown_rolls_back_running_administrative_sql() {
    root_contract(
        "shutdown_rolls_back_running_administrative_sql",
        Scenario::RunningSql,
    );
}

#[derive(Clone, Copy)]
enum Scenario {
    Success,
    OwnershipLoss,
    RunningSql,
}

fn root_contract(test: &str, scenario: Scenario) {
    if let Ok(url) = std::env::var("TELCHAR_ADMIN_FIXTURE_DATABASE") {
        exercise_daemon(&url, scenario);
        return;
    }
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "PostgreSQL fixture requires nonroot"
    );
    let fixture = PostgresFixture::start();
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("TELCHAR_ADMIN_FIXTURE_DATABASE", fixture.url())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "namespace contract failed: {output:?}"
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected diagnostics: {output:?}"
    );
}

fn exercise_daemon(url: &str, scenario: Scenario) {
    let expire_owner = matches!(scenario, Scenario::OwnershipLoss);
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let ipc = root.path().join("ipc.sock");
    let admin = root.path().join("admin.sock");
    let config = root.path().join("service.toml");
    std::fs::write(&config, "[database]\nownership_renewal_seconds = 1\nownership_lease_seconds = 3\n[backends.local]\nname = \"local\"\nsystem = \"aarch64-linux\"\nmaximum_concurrent_builds = 1\n").unwrap();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_telchar"))
            .env_clear()
            .args(["daemon", "--socket"])
            .arg(&ipc)
            .args(["--frontend-uid", "0"])
            .env("TELCHAR_CONFIG", &config)
            .env("TELCHAR_DATABASE_URL", url)
            .env("TELCHAR_ADMIN_SOCKET", &admin)
            .env(
                "TELCHAR_GATEWAY_STORE_URI",
                "unix:///nonexistent-admin-fixture-store",
            )
            .env("TMPDIR", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !admin.exists() {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited before administrative readiness"
        );
        assert!(
            Instant::now() < deadline,
            "administrative socket did not open"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::metadata(&admin).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let database = telchar::persistence::Database::connect(url).unwrap();
    let mut inspector = database.connection().unwrap();
    let owner = inspector
        .query_one(
            "SELECT owner_token, generation FROM singleton_ownership WHERE owner_kind = 'daemon'",
            &[],
        )
        .unwrap();
    let token: String = owner.get(0);
    let generation: i64 = owner.get(1);
    // The fixture seeds an unresolved obligation under the active daemon's fence.
    let derivation = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-admin.drv";
    let fenced_url = format!(
        "{url}{}options=-c%20telchar.owner_kind%3Ddaemon%20-c%20telchar.owner_token%3D{token}%20-c%20telchar.owner_generation%3D{generation}",
        if url.contains('?') { "&" } else { "?" }
    );
    let fenced = telchar::persistence::Database::connect(&fenced_url).unwrap();
    telchar::persistence::claim_shared_build(
        &fenced,
        derivation,
        &[1; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-admin"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(&fenced, derivation).unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(&fenced, derivation)
        .unwrap()
        .unwrap();
    assert!(
        telchar::persistence::reserve_microvm_launch(
            &fenced,
            &telchar::persistence::MicrovmLaunchIntent {
                intent_id: "admin-intent".into(),
                attempt_id: attempt.attempt_id,
                backend_name: "local".into(),
                account_id: "116392764028".into(),
                region: "us-west-2".into(),
                image_arn: "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture".into(),
                image_version: "1".into(),
                configuration_digest: [1; 32],
                request_digest: [2; 32],
                client_token: "admin-intent".into(),
                credential_verifier: [3; 32],
                admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
                maximum_duration_seconds: 600,
            },
            1
        )
        .unwrap()
    );
    assert!(telchar::persistence::dispatch_microvm_launch(&fenced, "admin-intent").unwrap());
    assert!(telchar::persistence::cancel_microvm_launch(&fenced, "admin-intent").unwrap());
    let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
    command
        .env_clear()
        .args(["admin", "microvm", "risk-release", "--socket"])
        .arg(&admin)
        .args([
            "--intent",
            "admin-intent",
            "--reason",
            "Remote execution risk accepted",
            "--evidence",
            "incident:root",
            "--accept-remote-execution-risk",
        ]);
    let mut incomplete = None;
    if expire_owner {
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGSTOP) },
            0
        );
        let mut stopped = 0;
        assert_eq!(
            unsafe { libc::waitpid(daemon.0.id() as i32, &mut stopped, libc::WUNTRACED) },
            daemon.0.id() as i32
        );
        assert!(libc::WIFSTOPPED(stopped));
        let mut queued = std::os::unix::net::UnixStream::connect(&admin).unwrap();
        telchar::service::admin::write_request(
            &mut queued,
            &telchar::service::admin::Request::RiskRelease {
                intent_id: "admin-intent".into(),
                provider_id: None,
                reason: "Remote execution risk accepted".into(),
                evidence_reference: "incident:stale-owner".into(),
                accept_remote_execution_risk: true,
            },
        )
        .unwrap();
        inspector.batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").unwrap();
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGCONT) },
            0
        );
        match telchar::service::admin::read_response(&mut queued) {
            Ok(response) => {
                assert!(response.rejected);
                assert!(!response.released);
            }
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ),
                "{error}"
            ),
        }
    } else if matches!(scenario, Scenario::RunningSql) {
        inspector.batch_execute("CREATE FUNCTION delay_admin_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(10); RETURN NEW; END $$; CREATE TRIGGER delay_admin_release BEFORE UPDATE ON microvm_launches FOR EACH ROW EXECUTE FUNCTION delay_admin_release()").unwrap();
        let stream = incomplete.insert(std::os::unix::net::UnixStream::connect(&admin).unwrap());
        telchar::service::admin::write_request(
            stream,
            &telchar::service::admin::Request::RiskRelease {
                intent_id: "admin-intent".into(),
                provider_id: None,
                reason: "Remote execution risk accepted".into(),
                evidence_reference: "incident:running-sql".into(),
                accept_remote_execution_risk: true,
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let running: bool = inspector.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND wait_event = 'PgSleep' AND query LIKE '%UPDATE microvm_launches%')", &[]).unwrap().get(0);
            if running {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "administrative SQL did not reach trigger"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
            0
        );
    } else {
        let output = command.output().unwrap();
        assert!(output.status.success(), "administration failed: {output:?}");
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout,
            b"{\"released\":true,\"attached\":false,\"rejected\":false}\n"
        );
        let audit = inspector.query_one("SELECT actor_uid, owner_generation, evidence_reference FROM microvm_resolutions WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert_eq!(audit.get::<_, i64>(0), 0);
        assert_eq!(audit.get::<_, i64>(1), generation);
        assert_eq!(audit.get::<_, String>(2), "incident:root");
        let repeated = command.output().unwrap();
        assert!(repeated.status.success());
        assert!(repeated.stderr.is_empty());
        assert_eq!(
            repeated.stdout,
            b"{\"released\":false,\"attached\":false,\"rejected\":false}\n"
        );
        let state = inspector.query_one("SELECT reservation_released_at IS NOT NULL, termination_observed_at IS NULL, (SELECT count(*) FROM microvm_resolutions) FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert!(state.get::<_, bool>(0));
        assert!(state.get::<_, bool>(1));
        assert_eq!(state.get::<_, i64>(2), 1);
        use std::io::Write;
        let stream = incomplete.insert(std::os::unix::net::UnixStream::connect(&admin).unwrap());
        stream.write_all(&[1]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
            0
        );
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert_eq!(status.success(), !expire_owner);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon shutdown exceeded deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::io::Read;
    let mut stdout = String::new();
    let mut stderr = String::new();
    daemon
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stdout.is_empty());
    if expire_owner {
        assert!(
            stderr.ends_with("telchar: singleton daemon ownership lost\n"),
            "{stderr}"
        );
        let errors = stderr
            .lines()
            .filter(|line| line.contains("ERROR"))
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 2, "{stderr}");
        assert!(
            errors[0].contains("event=\"database.singleton_ownership.lost\""),
            "{stderr}"
        );
        assert!(
            errors[1].contains("event=\"ipc.daemon.connection_failed\""),
            "{stderr}"
        );
        assert!(!stderr.contains("WARN"), "{stderr}");
        let state = inspector.query_one("SELECT reservation_released_at IS NULL, termination_observed_at IS NULL, (SELECT count(*) FROM microvm_resolutions) FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert!(state.get::<_, bool>(0));
        assert!(state.get::<_, bool>(1));
        assert_eq!(state.get::<_, i64>(2), 0);
    } else {
        assert!(
            !stderr.contains("ERROR") && !stderr.contains("WARN"),
            "{stderr}"
        );
    }
    if matches!(scenario, Scenario::RunningSql) {
        let state = inspector.query_one("SELECT reservation_released_at IS NULL, termination_observed_at IS NULL, (SELECT count(*) FROM microvm_resolutions) FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert!(state.get::<_, bool>(0));
        assert!(state.get::<_, bool>(1));
        assert_eq!(state.get::<_, i64>(2), 0);
    }
    assert!(!admin.exists());
    assert!(!ipc.exists());
    assert_eq!(
        inspector
            .query_one("SELECT count(*) FROM singleton_ownership", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
