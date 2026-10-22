//! Exercises root administration through the active daemon and isolated PostgreSQL.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use telchar::backend::BackendKind;
use telchar_test_support::postgres::PostgresFixture;

#[path = "../../../tests/microvm/image.rs"]
mod image;

fn attachment_provider(
    config: &std::path::Path,
    lookup_started: Option<std::sync::mpsc::Sender<()>>,
    terminal: bool,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = callback.local_addr().unwrap();
    let mut configuration = std::fs::read_to_string(config).unwrap();
    configuration.push_str(&format!(
        r#"
[backends]
microvm_bind = "{address}"
[[backends.microvm]]
name = "arm"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture"
image_version = "1"
maximum_concurrent_builds = 1
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 15
provider_timeout_seconds = 10
cleanup_interval_seconds = 300
callback_url = "wss://gateway.example/microvm"
callback_address = "192.0.2.1:443"
staging_directory = "/unused"
maximum_paths = 10
maximum_nar_bytes = 1024
maximum_total_bytes = 2048
metadata_bytes = 1024
chunk_bytes = 1024
"#
    ));
    std::fs::write(config, configuration).unwrap();
    drop(callback);
    let server = std::thread::spawn(move || {
        for identity in [true, false] {
            if !identity {
                image::serve_image(
                    &listener,
                    "arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture",
                );
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider request missing");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 32768);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.contains("credential=fixture-key/"));
            let (body, content_type) = if identity {
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(length < 4096);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                assert!(
                    String::from_utf8(body)
                        .unwrap()
                        .contains("Action=GetCallerIdentity")
                );
                ("<GetCallerIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><GetCallerIdentityResult><Account>116392764028</Account><Arn>arn:aws:iam::116392764028:user/fixture</Arn><UserId>fixture</UserId></GetCallerIdentityResult><ResponseMetadata><RequestId>fixture</RequestId></ResponseMetadata></GetCallerIdentityResponse>".to_owned(), "text/xml")
            } else {
                assert!(headers.starts_with("get /2025-09-09/microvms/microvm-exact "));
                if let Some(started) = &lookup_started {
                    started.send(()).unwrap();
                    std::thread::sleep(Duration::from_secs(3));
                    continue;
                }
                (serde_json::json!({"microvmId":"microvm-exact", "state":if terminal { "TERMINATED" } else { "RUNNING" }, "endpoint":"https://unused.invalid", "imageArn":"arn:aws:lambda:us-west-2:116392764028:microvm-image:fixture", "imageVersion":"1", "maximumDurationInSeconds":600, "startedAt":1}).to_string(), "application/json")
            };
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (endpoint, server)
}

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

#[test]
fn shutdown_bounds_stalled_administrative_transport() {
    root_contract(
        "shutdown_bounds_stalled_administrative_transport",
        Scenario::StalledTransport,
    );
}

#[test]
fn daemon_attachment_preserves_cleanup_and_denies_admission() {
    root_contract(
        "daemon_attachment_preserves_cleanup_and_denies_admission",
        Scenario::Attachment,
    );
}

#[test]
fn shutdown_rejects_running_administrative_provider_lookup() {
    root_contract(
        "shutdown_rejects_running_administrative_provider_lookup",
        Scenario::ProviderLookup,
    );
}

#[test]
fn daemon_termination_records_exact_observation() {
    root_contract(
        "daemon_termination_records_exact_observation",
        Scenario::Termination {
            released: false,
            terminal: true,
        },
    );
}

#[test]
fn daemon_termination_preserves_prior_risk_release() {
    root_contract(
        "daemon_termination_preserves_prior_risk_release",
        Scenario::Termination {
            released: true,
            terminal: true,
        },
    );
}

#[test]
fn daemon_termination_rejects_running_provider() {
    root_contract(
        "daemon_termination_rejects_running_provider",
        Scenario::Termination {
            released: false,
            terminal: false,
        },
    );
}

#[derive(Clone, Copy)]
enum Scenario {
    Termination { released: bool, terminal: bool },
    Attachment,
    ProviderLookup,
    Success,
    OwnershipLoss,
    RunningSql,
    StalledTransport,
}

struct ResumeBackend(i32);

impl Drop for ResumeBackend {
    fn drop(&mut self) {
        assert_eq!(unsafe { libc::kill(self.0, libc::SIGCONT) }, 0);
    }
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
    let (lookup_started, lookup_received) = std::sync::mpsc::channel();
    let provider = matches!(
        scenario,
        Scenario::Attachment | Scenario::ProviderLookup | Scenario::Termination { .. }
    )
    .then(|| {
        attachment_provider(
            &config,
            matches!(scenario, Scenario::ProviderLookup).then_some(lookup_started),
            matches!(scenario, Scenario::Termination { terminal: true, .. }),
        )
    });
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
            .env("HOME", root.path())
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ACCESS_KEY_ID", "fixture-key")
            .env("AWS_SECRET_ACCESS_KEY", "fixture-secret")
            .env(
                "TELCHAR_TEST_MICROVM_ENDPOINT",
                provider.as_ref().map_or("", |(endpoint, _)| endpoint),
            )
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
    if matches!(scenario, Scenario::Termination { .. }) {
        assert!(
            telchar::persistence::bind_microvm_launch(&fenced, "admin-intent", "microvm-exact")
                .unwrap()
        );
    }
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
    let mut stopped_backend = None;
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
    } else if let Scenario::Termination { released, terminal } = scenario {
        if released {
            let output = command
                .args(["--provider-id", "microvm-exact"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_eq!(
                output.stdout,
                b"{\"released\":true,\"attached\":false,\"terminated\":false,\"rejected\":false}\n"
            );
        }
        let release_before: Option<std::time::SystemTime> = inspector.query_one(
            "SELECT reservation_released_at FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]
        ).unwrap().get(0);
        assert_eq!(release_before.is_some(), released);
        let mut verification = Command::new(env!("CARGO_BIN_EXE_telchar"));
        verification
            .env_clear()
            .args(["admin", "microvm", "verify-termination", "--socket"])
            .arg(&admin)
            .args([
                "--intent",
                "admin-intent",
                "--provider-id",
                "microvm-exact",
                "--reason",
                "Confirm exact terminal state",
                "--evidence",
                "incident:daemon-termination",
            ]);
        let output = verification.output().unwrap();
        assert_eq!(output.status.success(), terminal, "{output:?}");
        if terminal {
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_eq!(
                output.stdout,
                b"{\"released\":false,\"attached\":false,\"terminated\":true,\"rejected\":false}\n"
            );
            let repeated = verification.output().unwrap();
            assert!(!repeated.status.success(), "{repeated:?}");
            assert!(repeated.stdout.is_empty(), "{repeated:?}");
            assert_eq!(
                repeated.stderr,
                b"telchar: administrative mutation outcome unknown; inspect durable state and audit before retrying\n"
            );
            let audit = inspector.query_one("SELECT actor_uid, owner_generation, provider_id, evidence_reference FROM microvm_resolutions WHERE intent_id = 'admin-intent' AND action = 'verify-termination'", &[]).unwrap();
            assert_eq!(audit.get::<_, i64>(0), 0);
            assert_eq!(audit.get::<_, i64>(1), generation);
            assert_eq!(audit.get::<_, String>(2), "microvm-exact");
            assert_eq!(audit.get::<_, String>(3), "incident:daemon-termination");
        } else {
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(output.stderr, b"telchar: administrative mutation outcome unknown; inspect durable state and audit before retrying\n");
        }
        let state = inspector.query_one("SELECT reservation_released_at, termination_observed_at IS NOT NULL, phase, (SELECT count(*) FROM microvm_resolutions) FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        let release_after: Option<std::time::SystemTime> = state.get(0);
        assert_eq!(release_after.is_some(), released || terminal);
        if released {
            assert_eq!(release_before, release_after);
        }
        assert_eq!(state.get::<_, bool>(1), terminal);
        assert_eq!(state.get::<_, String>(2), "closed");
        assert_eq!(
            state.get::<_, i64>(3),
            i64::from(released) + i64::from(terminal)
        );
        assert_eq!(
            telchar::persistence::pending_microvm_cleanup_identity(
                &database,
                "admin-intent",
                "microvm-exact"
            )
            .unwrap(),
            !terminal
        );
        assert!(
            !telchar::persistence::admit_microvm_session(
                &fenced,
                "admin-intent",
                "microvm-exact",
                &[3; 32]
            )
            .unwrap()
        );
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
            0
        );
    } else if matches!(scenario, Scenario::Attachment) {
        let output = Command::new(env!("CARGO_BIN_EXE_telchar"))
            .env_clear()
            .args(["admin", "microvm", "attach-identity", "--socket"])
            .arg(&admin)
            .args([
                "--intent",
                "admin-intent",
                "--provider-id",
                "microvm-exact",
                "--reason",
                "Recovered launch response",
                "--evidence",
                "incident:daemon-attachment",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_eq!(
            output.stdout,
            b"{\"released\":false,\"attached\":true,\"terminated\":false,\"rejected\":false}\n"
        );
        let state = inspector.query_one("SELECT provider_id, phase, reservation_released_at IS NULL, termination_observed_at IS NULL FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert_eq!(state.get::<_, String>(0), "microvm-exact");
        assert_eq!(state.get::<_, String>(1), "closed");
        assert!(state.get::<_, bool>(2));
        assert!(state.get::<_, bool>(3));
        let audit = inspector.query_one("SELECT actor_uid, owner_generation, evidence_reference, action FROM microvm_resolutions WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert_eq!(audit.get::<_, i64>(0), 0);
        assert_eq!(audit.get::<_, i64>(1), generation);
        assert_eq!(audit.get::<_, String>(2), "incident:daemon-attachment");
        assert_eq!(audit.get::<_, String>(3), "attach-identity");
        assert!(
            telchar::persistence::pending_microvm_cleanup_identity(
                &database,
                "admin-intent",
                "microvm-exact"
            )
            .unwrap()
        );
        assert!(
            !telchar::persistence::admit_microvm_session(
                &fenced,
                "admin-intent",
                "microvm-exact",
                &[3; 32]
            )
            .unwrap()
        );
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
            0
        );
    } else if matches!(scenario, Scenario::ProviderLookup) {
        let stream = incomplete.insert(std::os::unix::net::UnixStream::connect(&admin).unwrap());
        telchar::service::admin::write_request(
            stream,
            &telchar::service::admin::Request::AttachIdentity {
                intent_id: "admin-intent".into(),
                provider_id: "microvm-exact".into(),
                reason: "Recovered launch response".into(),
                evidence_reference: "incident:provider-shutdown".into(),
            },
        )
        .unwrap();
        lookup_received
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(daemon.0.id() as i32, libc::SIGTERM) },
            0
        );
    } else if matches!(scenario, Scenario::RunningSql | Scenario::StalledTransport) {
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
            let running = inspector.query_opt("SELECT pid FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND wait_event = 'PgSleep' AND query LIKE '%UPDATE microvm_launches%'", &[]).unwrap();
            if let Some(running) = running {
                if matches!(scenario, Scenario::StalledTransport) {
                    let pid = running.get::<_, i32>(0);
                    stopped_backend = Some(ResumeBackend(pid));
                    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while !std::fs::read_to_string(format!("/proc/{pid}/status"))
                        .unwrap()
                        .lines()
                        .any(|line| line.starts_with("State:\tT"))
                    {
                        assert!(Instant::now() < deadline, "backend did not stop");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
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
            b"{\"released\":true,\"attached\":false,\"terminated\":false,\"rejected\":false}\n"
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
            b"{\"released\":false,\"attached\":false,\"terminated\":false,\"rejected\":false}\n"
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
    let deadline = Instant::now()
        + if matches!(scenario, Scenario::ProviderLookup) {
            Duration::from_millis(2750)
        } else {
            Duration::from_secs(2)
        };
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
    drop(stopped_backend);
    let mut inspector = database.connection().unwrap();
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
    if matches!(
        scenario,
        Scenario::RunningSql | Scenario::StalledTransport | Scenario::ProviderLookup
    ) {
        let state = inspector.query_one("SELECT reservation_released_at IS NULL, termination_observed_at IS NULL, (SELECT count(*) FROM microvm_resolutions), provider_id IS NULL, phase FROM microvm_launches WHERE intent_id = 'admin-intent'", &[]).unwrap();
        assert!(state.get::<_, bool>(0));
        assert!(state.get::<_, bool>(1));
        assert_eq!(state.get::<_, i64>(2), 0);
        assert!(state.get::<_, bool>(3));
        assert_eq!(state.get::<_, String>(4), "ambiguous");
    }
    if let Some((_, provider)) = provider {
        provider.join().unwrap();
    }
    assert!(!admin.exists());
    assert!(!ipc.exists());
    if matches!(scenario, Scenario::RunningSql | Scenario::StalledTransport) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let lease = inspector.query_opt("SELECT generation, lease_expires_at > clock_timestamp() FROM singleton_ownership WHERE owner_kind = 'daemon'", &[]).unwrap();
            let Some(lease) = lease else { break };
            assert_eq!(lease.get::<_, i64>(0), generation);
            if !lease.get::<_, bool>(1) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retired daemon lease did not expire"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let replacement = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            url,
            Duration::from_secs(3),
        )
        .unwrap();
        replacement.verify().unwrap();
        replacement.renew().unwrap();
        drop(replacement);
    }
    assert_eq!(
        inspector
            .query_one("SELECT count(*) FROM singleton_ownership", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
