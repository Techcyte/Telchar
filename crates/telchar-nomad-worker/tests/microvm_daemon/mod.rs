use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use telchar::nomad::protocol::BuildOutcome;
use telchar::store::daemon::GatewayStoreEndpoint;

mod client;

use super::collector;

struct Daemon(std::process::Child, std::path::PathBuf);

impl Daemon {
    fn spawn(command: &mut Command, home: &Path) -> std::io::Result<Self> {
        let stderr = home.join("daemon.stderr");
        command.stderr(std::fs::File::create(&stderr)?);
        Ok(Self(command.spawn()?, stderr))
    }

    fn stderr(&self) -> std::io::Result<String> {
        std::fs::read_to_string(&self.1)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub fn run(
    root: &Path,
    database: &str,
    store: &GatewayStoreEndpoint,
    address: SocketAddr,
    gateway: SocketAddr,
    request: &telchar::build::BuildRequest,
    binding: std::sync::mpsc::SyncSender<serde_json::Value>,
) -> std::io::Result<BuildOutcome> {
    let case = std::env::var("MICROVM_TEST_CASE").unwrap();
    let collector = (case == "configured-daemon").then(collector::start_collector);
    let crashes = case == "configured-daemon-crash";
    let recovers_outputs = case == "configured-daemon-output-recovery";
    let fails = matches!(
        case.as_str(),
        "configured-daemon-failure"
            | "configured-daemon-corrupt-output"
            | "configured-daemon-crash"
    );
    let (endpoint, provider) = super::provider_server(Some(binding));
    let home = root.join("daemon");
    std::fs::create_dir(&home)?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    let roots = home.join("gcroots");
    std::fs::create_dir(&roots)?;
    let socket = home.join("ipc.sock");
    let config = home.join("service.toml");
    let compiler = std::env::var_os("MICROVM_TEST_COMPILER").is_some();
    let transfer_bytes = if compiler {
        (320 * 1024 * 1024, 512 * 1024 * 1024)
    } else if std::env::var_os("MICROVM_TEST_RUNTIME").is_some() {
        (64 * 1024 * 1024, 128 * 1024 * 1024)
    } else {
        (1048576, 2097152)
    };
    std::fs::write(
        &config,
        format!(
            r#"
[backends]
microvm_bind = "{gateway}"
[[backends.microvm]]
name = "build"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder"
image_version = "1"
vpc_egress_connector_arn = "arn:aws:lambda:us-west-2:116392764028:network-connector:builder"
maximum_concurrent_builds = 1
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 20
provider_timeout_seconds = 2
cleanup_interval_seconds = 60
callback_url = "wss://localhost:{port}/microvm"
callback_address = "{address}"
staging_directory = "{staging}"
maximum_paths = {maximum_paths}
maximum_nar_bytes = {nar_bytes}
maximum_total_bytes = {total_bytes}
metadata_bytes = {metadata_bytes}
chunk_bytes = 262144
"#,
            port = address.port(),
            staging = root.display(),
            nar_bytes = transfer_bytes.0,
            total_bytes = transfer_bytes.1,
            maximum_paths = if compiler { 64 } else { 10 },
            metadata_bytes = if compiler { 65536 } else { 8192 }
        ),
    )?;
    let uid = Command::new("id").arg("-u").output()?;
    assert!(uid.status.success());
    assert!(uid.stderr.is_empty());
    let binary = std::env::var_os("TELCHAR_TEST_DAEMON_BIN")
        .expect("TELCHAR_TEST_DAEMON_BIN must identify the freshly built daemon");
    let mut command = Command::new(binary);
    command
        .args(["daemon", "--socket"])
        .arg(&socket)
        .args([
            "--frontend-uid",
            String::from_utf8(uid.stdout).unwrap().trim(),
        ])
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", &home)
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_ACCESS_KEY_ID", "fixture-key")
        .env("AWS_SECRET_ACCESS_KEY", "fixture-secret")
        .env("TELCHAR_TEST_MICROVM_ENDPOINT", endpoint)
        .env("TELCHAR_CONFIG", &config)
        .env("TELCHAR_DATABASE_URL", database)
        .env("TELCHAR_GATEWAY_STORE_URI", store.to_string())
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &roots)
        .env("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", "67108864")
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    if let Some(collector) = &collector {
        command
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc");
    }
    let mut daemon = Daemon::spawn(&mut command, &home)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "daemon startup deadline");
        if let Some(status) = daemon.0.try_wait()? {
            let diagnostic = daemon.stderr()?;
            panic!("daemon exited {status}: {diagnostic}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let coalesced = matches!(
        case.as_str(),
        "configured-daemon-coalesced" | "configured-daemon-detached"
    );
    let stock_client = case == "configured-daemon-client-derivation";
    let mut terminal_barrier = None;
    if stock_client {
        client::build(root, &socket, request.derivation_path())?;
    } else {
        let mut stream = std::os::unix::net::UnixStream::connect(&socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        use telchar::service::ipc::{IPC_VERSION, IpcEnvelope, IpcListener, RequesterMetadata};
        IpcListener::send_envelope(
            &mut stream,
            &IpcEnvelope {
                version: IPC_VERSION,
                requester: RequesterMetadata {
                    credential_id: "ssh-pubkey:microvm-build".into(),
                    audit_subject: "microvm-build".into(),
                    quota_subject: "microvm-build".into(),
                },
                session_id: "microvm-daemon-build".into(),
                trace_context: telchar_telemetry::TraceContext::default(),
                error: None,
            },
        )?;
        let client = match nix_worker_protocol::WorkerClient::connect(stream.try_clone()?) {
            Ok(client) => client,
            Err(error) => {
                daemon.0.kill()?;
                daemon.0.wait()?;
                let diagnostic = daemon.stderr()?;
                panic!("daemon handshake {error}: {diagnostic}");
            }
        };
        let mut wire = Vec::new();
        use nix_worker_protocol::{
            write_worker_byte_string as bytes, write_worker_integer as word,
        };
        word(&mut wire, 36);
        bytes(&mut wire, request.derivation_path());
        word(&mut wire, request.output_authorities().len() as u64);
        for output in request.output_authorities() {
            for value in [
                output.name(),
                output.path(),
                output.hash_algorithm(),
                output.hash(),
            ] {
                bytes(&mut wire, value);
            }
        }
        word(&mut wire, request.input_sources().len() as u64);
        for input in request.input_sources() {
            bytes(&mut wire, input);
        }
        bytes(&mut wire, request.system().as_bytes());
        bytes(&mut wire, request.builder());
        word(&mut wire, request.arguments().len() as u64);
        for argument in request.arguments() {
            bytes(&mut wire, argument);
        }
        word(&mut wire, request.environment().len() as u64);
        for (key, value) in request.environment() {
            bytes(&mut wire, key);
            bytes(&mut wire, value);
        }
        word(&mut wire, 0);
        terminal_barrier = if recovers_outputs {
            let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
            control.batch_execute("CREATE FUNCTION hold_terminal_result() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state = 'succeeded' THEN PERFORM pg_advisory_xact_lock(78564321); END IF; RETURN NEW; END $$; CREATE TRIGGER hold_terminal_result BEFORE UPDATE ON shared_builds FOR EACH ROW EXECUTE FUNCTION hold_terminal_result(); SELECT pg_advisory_lock(78564321);").unwrap();
            Some(control)
        } else {
            None
        };
        stream.write_all(&wire)?;
        stream.flush()?;
        let mut expected = Vec::new();
        if std::env::var_os("MICROVM_TEST_RUNTIME").is_some() {
            nix_worker_protocol::write_stderr_frame(
                &mut expected,
                nix_worker_protocol::StderrFrame::Next {
                    message: b"sandbox-builder-executing\n".to_vec(),
                },
            )?;
        }
        if fails {
            nix_worker_protocol::write_worker_error(
                &mut expected,
                "BuildDerivation execution failed",
            )?;
        } else {
            nix_worker_protocol::write_build_derivation_success_response(
                &mut expected,
                client.profile().version,
                false,
            )?;
        }
        let follower = if coalesced {
            let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !control
                .query_one("SELECT EXISTS(SELECT 1 FROM shared_builds)", &[])
                .unwrap()
                .get::<_, bool>(0)
            {
                assert!(Instant::now() < deadline, "leader claim deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut follower = std::os::unix::net::UnixStream::connect(&socket)?;
            follower.set_read_timeout(Some(Duration::from_secs(20)))?;
            follower.set_write_timeout(Some(Duration::from_secs(5)))?;
            IpcListener::send_envelope(
                &mut follower,
                &IpcEnvelope {
                    version: IPC_VERSION,
                    requester: RequesterMetadata {
                        credential_id: "ssh-pubkey:microvm-follower".into(),
                        audit_subject: "microvm-follower".into(),
                        quota_subject: "microvm-follower".into(),
                    },
                    session_id: "microvm-daemon-follower".into(),
                    trace_context: telchar_telemetry::TraceContext::default(),
                    error: None,
                },
            )?;
            let client = nix_worker_protocol::WorkerClient::connect(follower.try_clone()?)?;
            follower.write_all(&wire)?;
            follower.flush()?;
            Some((follower, client))
        } else {
            None
        };
        if recovers_outputs {
            let control = terminal_barrier.as_mut().unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !control.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND objid = 78564321 AND NOT granted)", &[]).unwrap().get::<_, bool>(0) {
            assert!(Instant::now() < deadline, "terminal write barrier deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
            let mut gateway = telchar::store::daemon::GatewayStoreConnection::connect(store)?;
            for (_, output) in request.expected_outputs() {
                assert!(
                    gateway.query_path_info(output)?.is_some(),
                    "output registered before crash"
                );
            }
            assert_ne!(
                control
                    .query_one("SELECT state FROM shared_builds", &[])
                    .unwrap()
                    .get::<_, String>(0),
                "succeeded"
            );
        } else if crashes {
            let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !control.query_one("SELECT EXISTS(SELECT 1 FROM microvm_launches WHERE phase = 'bound' AND provider_id = 'microvm-build')", &[]).unwrap().get::<_, bool>(0) {
            assert!(Instant::now() < deadline, "provider binding deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
            assert_eq!(
                control
                    .query_one("SELECT state FROM shared_builds", &[])
                    .unwrap()
                    .get::<_, String>(0),
                "running"
            );
        } else if case == "configured-daemon-detached" {
            let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while control
                .query_one(
                    "SELECT count(*) FROM request_attachments WHERE state = 'attached'",
                    &[],
                )
                .unwrap()
                .get::<_, i64>(0)
                != 2
            {
                assert!(Instant::now() < deadline, "follower attachment deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
            stream.shutdown(std::net::Shutdown::Both)?;
            std::fs::write(root.join("requester-disconnected"), b"")?;
        } else {
            let mut actual = vec![0; expected.len()];
            let read = stream.read_exact(&mut actual);
            if read.is_err() || actual != expected {
                daemon.0.kill()?;
                daemon.0.wait()?;
                let diagnostic = daemon.stderr()?;
                panic!("daemon read {read:?}, result {actual:?}: {diagnostic}");
            }
        }
        if let Some((mut follower, _client)) = follower {
            let mut follower_expected = Vec::new();
            nix_worker_protocol::write_stderr_frame(
                &mut follower_expected,
                nix_worker_protocol::StderrFrame::Next {
                    message: b"identical build already in progress\n".to_vec(),
                },
            )?;
            follower_expected.extend_from_slice(&expected);
            let mut actual = vec![0; follower_expected.len()];
            follower.read_exact(&mut actual)?;
            assert_eq!(
                actual, follower_expected,
                "follower receives shared success"
            );
        }
        drop(stream);
        drop(client);
    }
    if case == "configured-daemon-restart" || crashes || recovers_outputs {
        let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
        let generation: i64 = control
            .query_one(
                "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon'",
                &[],
            )
            .unwrap()
            .get(0);
        daemon.0.kill()?;
        assert!(!daemon.0.wait()?.success());
        if let Some(mut barrier) = terminal_barrier.take() {
            barrier.batch_execute("SELECT pg_advisory_unlock(78564321); DROP TRIGGER hold_terminal_result ON shared_builds; DROP FUNCTION hold_terminal_result();").unwrap();
            assert_ne!(
                barrier
                    .query_one("SELECT state FROM shared_builds", &[])
                    .unwrap()
                    .get::<_, String>(0),
                "succeeded",
                "terminal transaction committed despite crash"
            );
        }
        let mut stdout = String::new();
        let stderr = daemon.stderr()?;
        daemon
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)?;
        assert!(stdout.is_empty());
        assert!(
            !stderr.lines().any(|line| line.contains(" ERROR ")),
            "{stderr}"
        );
        control.batch_execute("UPDATE singleton_ownership SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE owner_kind = 'daemon'").unwrap();
        if crashes {
            let configuration = std::fs::read_to_string(&config)?;
            std::fs::write(
                &config,
                configuration.replace(
                    "cleanup_interval_seconds = 60",
                    "cleanup_interval_seconds = 1",
                ),
            )?;
        }
        std::fs::write(root.join("replacement-starting"), b"")?;
        daemon = Daemon::spawn(&mut command, &home)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = daemon.0.try_wait()? {
                let diagnostic = daemon.stderr()?;
                panic!("replacement exited {status}: {diagnostic}");
            }
            let replaced = control
                .query_opt(
                    "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon'",
                    &[],
                )
                .unwrap()
                .is_some_and(|row| row.get::<_, i64>(0) > generation);
            if replaced && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "replacement startup deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        if crashes {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !control.query_one("SELECT reservation_released_at IS NOT NULL FROM microvm_launches WHERE provider_id = 'microvm-build'", &[]).unwrap().get::<_, bool>(0) {
                assert!(Instant::now() < deadline, "replacement cleanup deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            assert_eq!(control.query_one("SELECT count(*) FROM microvm_launches WHERE provider_id = 'microvm-build' AND reservation_released_at IS NULL", &[]).unwrap().get::<_, i64>(0), 1);
        }
    }
    provider.join().unwrap();
    let signal = Command::new("kill")
        .args(["-TERM", &daemon.0.id().to_string()])
        .output()?;
    assert!(signal.status.success());
    assert!(signal.stderr.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while daemon.0.try_wait()?.is_none() {
        assert!(Instant::now() < deadline, "daemon shutdown deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(daemon.0.wait()?.success());
    let mut stdout = String::new();
    let stderr = daemon.stderr()?;
    daemon
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)?;
    assert!(stdout.is_empty());
    let errors = stderr
        .lines()
        .filter(|line| line.contains(" ERROR "))
        .collect::<Vec<_>>();
    if fails && !crashes {
        assert_eq!(errors.len(), 2, "{stderr}");
        assert!(
            errors[0].contains("event=\"worker.build_derivation.failed\""),
            "{stderr}"
        );
        let reason = if case == "configured-daemon-corrupt-output" {
            "reason=\"invalid-data\""
        } else {
            "reason=\"execution-failure\""
        };
        assert!(errors[0].contains(reason), "{stderr}");
        assert!(
            errors[1].contains("event=\"worker.operation.rejected\""),
            "{stderr}"
        );
        assert!(
            errors[1].contains("reason=\"BuildDerivation execution failed\""),
            "{stderr}"
        );
    } else {
        assert!(errors.is_empty(), "{stderr}");
    }
    let terminal_state = if fails { "failed" } else { "succeeded" };
    let mut control = postgres::Client::connect(database, postgres::NoTls).unwrap();
    let builds = control
        .query(
            "SELECT state, backend_kind, execution_recovery, expected_outputs FROM shared_builds",
            &[],
        )
        .unwrap();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0].get::<_, String>("state"), terminal_state);
    assert_eq!(builds[0].get::<_, String>("backend_kind"), "microvm");
    assert_eq!(
        builds[0].get::<_, String>("execution_recovery"),
        "output-only"
    );
    let mut expected_outputs = request
        .expected_outputs()
        .iter()
        .map(|(_, path)| String::from_utf8(path.clone()).unwrap())
        .collect::<Vec<_>>();
    expected_outputs.sort();
    let mut recorded_outputs = builds[0].get::<_, Vec<String>>("expected_outputs");
    recorded_outputs.sort();
    assert_eq!(recorded_outputs, expected_outputs);
    let attempts = control
        .query("SELECT state, backend_kind FROM shared_build_attempts", &[])
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].get::<_, String>("state"), terminal_state);
    assert_eq!(attempts[0].get::<_, String>("backend_kind"), "microvm");
    if !stock_client {
        let attachment = control
            .query_one(
                "SELECT state FROM request_attachments WHERE session_id = 'microvm-daemon-build'",
                &[],
            )
            .unwrap();
        assert_eq!(attachment.get::<_, String>(0), "detached");
    }
    if coalesced {
        let follower = control.query_one("SELECT state FROM request_attachments WHERE session_id = 'microvm-daemon-follower'", &[]).unwrap();
        assert_eq!(follower.get::<_, String>(0), "detached");
        assert!(
            stderr.contains("event=\"shared_build.coalescing.follower\""),
            "{stderr}"
        );
        assert_eq!(
            control
                .query_one("SELECT count(*) FROM microvm_launches", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
    }
    assert_eq!(
        control
            .query_one("SELECT count(*) FROM singleton_ownership", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(!socket.exists());
    assert!(std::net::TcpListener::bind(gateway).is_ok());
    if let Some(collector) = collector {
        collector.assert_counter("telchar.microvm.launches", "acknowledged", 1);
        collector.assert_counter("telchar.microvm.rendezvous", "connected", 1);
        collector.assert_counter("telchar.microvm.admissions", "accepted", 1);
        collector.assert_histogram_sample("telchar.microvm.rendezvous.duration", "connected", 0.0);
        collector.assert_histogram_sample("telchar.microvm.admission.duration", "accepted", 0.0);
        collector.assert_metric_attributes_are_bounded();
    }
    if fails {
        Err(std::io::Error::other("BuildDerivation execution failed"))
    } else {
        Ok(BuildOutcome::Built)
    }
}
