//! Tests operation dispatch contracts and failure boundaries, including live set options request returns terminal frame.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{
    CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION, SERVER_WORKER_MAGIC, STDERR_ERROR, STDERR_LAST,
};
use telchar::fixture::nix::{NixFixture, TrustMode};

mod support;

use support::postgres::PostgresFixture;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[path = "operation_dispatch/backpressure.rs"]
mod backpressure;
#[path = "operation_dispatch/build_admission.rs"]
mod build_admission;
#[path = "operation_dispatch/build_cleanup.rs"]
mod build_cleanup;
#[path = "operation_dispatch/build_completion.rs"]
mod build_completion;
#[path = "operation_dispatch/build_logs.rs"]
mod build_logs;
#[path = "operation_dispatch/cancellation.rs"]
mod cancellation;
#[path = "operation_dispatch/coalescing.rs"]
mod coalescing;
#[path = "operation_dispatch/detached_completion.rs"]
mod detached_completion;
#[path = "operation_dispatch/protocol.rs"]
mod protocol;
#[path = "operation_dispatch/queueing.rs"]
mod queueing;
#[path = "operation_dispatch/request_state.rs"]
mod request_state;
#[path = "operation_dispatch/store_transfer.rs"]
mod store_transfer;
#[path = "operation_dispatch/validation.rs"]
mod validation;

struct OperationResponse {
    message: String,
    rejection: &'static str,
}

struct FrontendFixture {
    root: PathBuf,
    frontend: Child,
    daemon: Child,
    database: PostgresFixture,
}

impl FrontendFixture {
    fn spawn(worker_timeout_ms: Option<u64>) -> Self {
        Self::spawn_configured(
            worker_timeout_ms,
            None,
            std::iter::empty::<(&str, String)>(),
            Some("cancel-running"),
        )
    }

    fn spawn_multi_with_store(
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_with_store_policy_and_mode(None, store_uri, environment, None, false)
    }

    fn spawn_multi_with_store_capacity(
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        maximum_concurrent_builds: usize,
    ) -> Self {
        let mut environment = environment.into_iter().collect::<Vec<_>>();
        let config_path = std::env::temp_dir().join(format!(
            "telchar-operation-config-{}-{}",
            std::process::id(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(
            &config_path,
            format!(
                "[scheduling.default]\nmaximum_queued_builds = 64\nmaximum_active_builds = 4\n\n[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nsupported_features = []\nmaximum_concurrent_builds = {maximum_concurrent_builds}\n\n[identity.credentials.\"ssh-pubkey:SHA256:bob\"]\naudit_subject = \"ssh-pubkey:SHA256:bob\"\nquota_subject = \"ssh-pubkey:SHA256:bob\"\n"
            ),
        )
        .expect("service configuration writes");
        environment.push(("TELCHAR_CONFIG", config_path.display().to_string()));
        Self::spawn_with_store_policy_and_mode(None, store_uri, environment, None, false)
    }

    fn spawn_with_store(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_with_store_policy(
            worker_timeout_ms,
            store_uri,
            environment,
            Some("cancel-running"),
        )
    }

    fn spawn_with_store_default_disconnect_policy(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self::spawn_with_store_policy(worker_timeout_ms, store_uri, environment, None)
    }

    fn spawn_with_store_policy(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
    ) -> Self {
        Self::spawn_with_store_policy_and_mode(
            worker_timeout_ms,
            store_uri,
            environment,
            running_disconnect_policy,
            true,
        )
    }

    fn spawn_with_store_policy_and_mode(
        worker_timeout_ms: Option<u64>,
        store_uri: &str,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
        once: bool,
    ) -> Self {
        let environment = environment.into_iter().collect::<Vec<_>>();
        let has_export = environment
            .iter()
            .any(|(name, _)| *name == "TELCHAR_TEST_EXPORT_HELPER");
        let has_build = environment
            .iter()
            .any(|(name, _)| *name == "TELCHAR_TEST_BUILD_HELPER");
        if has_export || !has_build {
            Self::spawn_configured_with_mode(
                worker_timeout_ms,
                Some(store_uri),
                environment,
                running_disconnect_policy,
                once,
            )
        } else {
            let root = std::env::temp_dir().join(format!(
                "telchar-operation-export-{}-{}",
                std::process::id(),
                FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("export fixture root creates");
            let nar_path = root.join("output.nar");
            fs::write(&nar_path, regular_nar(b"telchar-classic-fixture"))
                .expect("export fixture NAR writes");
            let export_helper = root.join("export-helper");
            fs::write(
                &export_helper,
                format!(
                    "#!/bin/sh\nset -eu\ncat >/dev/null\ncat '{}'\n",
                    nar_path.display()
                ),
            )
            .expect("export helper writes");
            fs::set_permissions(&export_helper, fs::Permissions::from_mode(0o700))
                .expect("export helper executable");
            let nix = root.join("nix");
            fs::write(
                &nix,
                "#!/bin/sh\nset -eu\nprintf '{\"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null},\"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-telchar-gate-3-contract.drv\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null},\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\":{\"narHash\":\"sha256-bCvi8SoWhgXry6N4IobDjq8/XXh7eouySlQNImf/aOE=\",\"narSize\":136,\"references\":[],\"deriver\":null,\"ca\":null}}\\n'\n",
            )
            .expect("Nix query helper writes");
            fs::set_permissions(&nix, fs::Permissions::from_mode(0o700))
                .expect("Nix helper executable");
            let mut environment = environment;
            environment.push((
                "TELCHAR_TEST_EXPORT_HELPER",
                export_helper.display().to_string(),
            ));
            environment.push(("TELCHAR_NIX", nix.display().to_string()));
            Self::spawn_configured_with_mode(
                worker_timeout_ms,
                Some(store_uri),
                environment,
                running_disconnect_policy,
                once,
            )
        }
    }

    fn spawn_configured(
        worker_timeout_ms: Option<u64>,
        store_uri: Option<&str>,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
    ) -> Self {
        Self::spawn_configured_with_mode(
            worker_timeout_ms,
            store_uri,
            environment,
            running_disconnect_policy,
            true,
        )
    }

    fn spawn_configured_with_mode(
        worker_timeout_ms: Option<u64>,
        store_uri: Option<&str>,
        environment: impl IntoIterator<Item = (&'static str, String)>,
        running_disconnect_policy: Option<&str>,
        once: bool,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "telchar-operation-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time follows epoch")
                .as_nanos(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture root creates");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("fixture root permissions set");
        let socket = root.join("daemon.sock");
        let gc_roots = store_uri.map(|_| root.join("gc-roots"));
        if let Some(gc_roots) = &gc_roots {
            fs::create_dir(gc_roots).expect("GC root directory creates");
            fs::set_permissions(gc_roots, fs::Permissions::from_mode(0o700))
                .expect("GC root directory permissions set");
        }
        let configured_store_uri = store_uri.map(str::to_owned);
        let config_path = root.join("telchar.toml");
        fs::write(
            &config_path,
            "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n",
        )
        .expect("daemon configuration writes");
        let database = PostgresFixture::start();
        let mut daemon_command = Command::new(env!("CARGO_BIN_EXE_telchar"));
        daemon_command.args([
            "daemon",
            "--socket",
            socket.to_str().expect("UTF-8 socket path"),
            "--frontend-uid",
            &rustix::process::getuid().as_raw().to_string(),
        ]);
        if once {
            daemon_command.arg("--once");
        }
        daemon_command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        daemon_command
            .env("TELCHAR_CONFIG", &config_path)
            .env("TELCHAR_DATABASE_URL", database.url())
            .env_remove("TELCHAR_RUNNING_DISCONNECT_POLICY")
            .env_remove("TELCHAR_TEST_BUILD_HELPER")
            .env_remove("TELCHAR_TEST_EXPORT_HELPER")
            .env_remove("TELCHAR_TEST_PROMOTE_HELPER")
            .env_remove("TELCHAR_GATEWAY_STORE_URI")
            .env_remove("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY");
        if let Some(gc_roots) = gc_roots {
            daemon_command.env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", gc_roots);
        }
        if let Some(running_disconnect_policy) = running_disconnect_policy {
            daemon_command.env(
                "TELCHAR_RUNNING_DISCONNECT_POLICY",
                running_disconnect_policy,
            );
        }
        if let Some(timeout) = worker_timeout_ms {
            daemon_command.env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", timeout.to_string());
        }
        if let Some(store_uri) = configured_store_uri {
            daemon_command.env("TELCHAR_GATEWAY_STORE_URI", store_uri);
        }
        let environment = environment.into_iter().collect::<Vec<_>>();
        if store_uri == Some("unix:///fixed-gateway.sock") {
            daemon_command.env("TELCHAR_TEST_STORE_RETENTION", "filesystem-only");
        }
        daemon_command.envs(environment);
        let mut daemon = daemon_command.spawn().expect("daemon starts");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "daemon socket was not created");
            assert!(
                daemon.try_wait().expect("daemon status").is_none(),
                "daemon exited before binding"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
        command
            .arg("serve-stdio")
            .env("TELCHAR_IPC_SOCKET", &socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(timeout) = worker_timeout_ms {
            command.env("TELCHAR_WORKER_IDLE_TIMEOUT_MS", timeout.to_string());
        }
        let frontend = command.spawn().expect("frontend starts");
        Self {
            root,
            frontend,
            daemon,
            database,
        }
    }

    fn spawn_frontend(&self) -> Child {
        self.spawn_frontend_with_key("SHA256:fixture")
    }

    fn spawn_frontend_with_key(&self, key: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_telchar"))
            .arg("serve-stdio")
            .env("TELCHAR_IPC_SOCKET", self.root.join("daemon.sock"))
            .env("TELCHAR_AUTHENTICATED_KEY", key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("frontend starts")
    }

    fn finish(mut self) -> String {
        let mut frontend_stderr = String::new();
        self.frontend
            .stderr
            .take()
            .expect("frontend stderr")
            .read_to_string(&mut frontend_stderr)
            .expect("frontend stderr reads");
        let terminated = if self
            .daemon
            .try_wait()
            .expect("daemon status reads")
            .is_none()
        {
            self.daemon.kill().expect("daemon terminates");
            true
        } else {
            false
        };
        let daemon_output = self.daemon.wait_with_output().expect("daemon exits");
        let _ = fs::remove_dir_all(self.root);
        assert!(
            terminated || daemon_output.status.success(),
            "daemon failed: {daemon_output:?}"
        );
        format!(
            "{frontend_stderr}{}",
            String::from_utf8_lossy(&daemon_output.stderr)
        )
    }
}

fn shared_build_quota_subject(database: &PostgresFixture, derivation_path: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let quota_subject = database
            .connect()
            .query_one(
                "SELECT quota_subject FROM shared_builds WHERE derivation_path = $1",
                &[&derivation_path],
            )
            .expect("shared build quota subject reads")
            .get::<_, Option<String>>(0);
        if let Some(quota_subject) = quota_subject {
            return quota_subject;
        }
        assert!(
            Instant::now() < deadline,
            "shared build did not acquire quota ownership"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn request_id(database: &mut postgres::Client) -> String {
    database
        .query_one("SELECT request_id FROM build_requests", &[])
        .expect("request ID reads")
        .get(0)
}

fn assert_active_derivation_lease(database: &PostgresFixture, request_id: &str) {
    let lease = database
        .connect()
        .query_one(
            "SELECT owner_kind, owner_id, store_path, purpose, state FROM store_leases WHERE owner_id = $1",
            &[&request_id],
        )
        .expect("active derivation lease reads");
    assert_eq!(lease.get::<_, String>(0), "request");
    assert_eq!(lease.get::<_, String>(1), request_id);
    assert_eq!(
        lease.get::<_, String>(2),
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
    );
    assert_eq!(lease.get::<_, String>(3), "derivation");
    assert_eq!(lease.get::<_, String>(4), "active");
}

fn assert_released_derivation_lease(database: &PostgresFixture, request_id: &str) {
    let lease = database
        .connect()
        .query_one(
            "SELECT state, released_at FROM store_leases WHERE owner_id = $1 AND purpose = 'derivation'",
            &[&request_id],
        )
        .expect("released derivation lease reads");
    assert_eq!(lease.get::<_, String>(0), "released");
    assert!(lease.get::<_, Option<SystemTime>>(1).is_some());
}

fn send_operation(operation: u64) -> OperationResponse {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, operation);
    input.flush().expect("operation flushes");
    drop(input);

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    let message = read_string(&mut output);
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");

    let status = child.wait().expect("Telchar exits");
    assert!(status.success(), "Telchar failed: {status}");
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.operation.rejected"),
        "missing structured rejection event: {stderr}"
    );
    let rejection = if stderr.contains("recognized-unsupported") {
        "recognized-unsupported"
    } else {
        "unknown-operation"
    };
    OperationResponse { message, rejection }
}

fn spawn_closure_daemon(
    socket: &std::path::Path,
    expect_output_registration: bool,
) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("closure daemon socket binds");
    thread::spawn(move || {
        // First retention connection protects the derivation before closure discovery.
        // Worker op 11 is AddTempRoot; op 12 registers Telchar's indirect GC root.
        let (mut stream, _) = listener.accept().expect("closure daemon accepts");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
        stream.flush().expect("closure greeting flushes");
        assert_eq!(read_integer(&mut stream), 0);
        write_integer(&mut stream, 0);
        stream.flush().expect("closure features flush");
        assert_eq!(read_integer(&mut stream), 0);
        assert_eq!(read_integer(&mut stream), 0);
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1);
        write_integer(&mut stream, STDERR_LAST);
        stream.flush().expect("closure handshake flushes");

        assert_eq!(read_integer(&mut stream), 11);
        assert_eq!(
            read_string(&mut stream),
            "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
        );
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        stream.flush().expect("temporary root response flushes");
        assert_eq!(read_integer(&mut stream), 12);
        let indirect_root = read_string(&mut stream);
        assert!(indirect_root.contains("gc-roots"));
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        stream.flush().expect("indirect root response flushes");
        handle_path_info_query(
            &listener,
            "/nix/store/22222222222222222222222222222222-telchar-input",
            136,
        );
        handle_root_registration(&listener); // input root

        if expect_output_registration {
            handle_root_registration(&listener); // verified output root
        }
    })
}

fn handle_path_info_query(listener: &UnixListener, expected_path: &str, nar_size: u64) {
    let (mut stream, _) = listener.accept().expect("path-info query accepts");
    assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
    assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut stream, SERVER_WORKER_MAGIC);
    write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
    stream.flush().expect("path-info greeting flushes");
    assert_eq!(read_integer(&mut stream), 0);
    write_integer(&mut stream, 0);
    stream.flush().expect("path-info features flushes");
    assert_eq!(read_integer(&mut stream), 0);
    assert_eq!(read_integer(&mut stream), 0);
    write_string(&mut stream, b"2.34.8");
    write_integer(&mut stream, 1);
    write_integer(&mut stream, STDERR_LAST);
    stream.flush().expect("path-info handshake flushes");
    assert_eq!(read_integer(&mut stream), 26);
    assert_eq!(read_string(&mut stream), expected_path);
    write_integer(&mut stream, STDERR_LAST);
    write_integer(&mut stream, 1);
    write_string(&mut stream, b"");
    write_string(
        &mut stream,
        b"6c2be2f12a168605ebcba3782286c38eaf3f5d787b7a8bb24a540d2267ff68e1",
    );
    write_integer(&mut stream, 0);
    write_integer(&mut stream, 0);
    write_integer(&mut stream, nar_size);
    write_integer(&mut stream, 0);
    write_integer(&mut stream, 0);
    write_string(&mut stream, b"");
    stream.flush().expect("path-info response flushes");
}

fn handle_root_registration(listener: &UnixListener) {
    // Input retention opens one connection per retained path. It sends AddTempRoot
    // (op 11), creates the symlink locally, then sends AddIndirectRoot (op 12).
    let (mut stream, _) = listener.accept().expect("root registration accepts");
    assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
    assert_eq!(read_integer(&mut stream), LATEST_WORKER_VERSION.to_wire());
    write_integer(&mut stream, SERVER_WORKER_MAGIC);
    write_integer(&mut stream, LATEST_WORKER_VERSION.to_wire());
    stream.flush().expect("root registration greeting flushes");
    assert_eq!(read_integer(&mut stream), 0);
    write_integer(&mut stream, 0);
    stream.flush().expect("root registration features flush");
    assert_eq!(read_integer(&mut stream), 0);
    assert_eq!(read_integer(&mut stream), 0);
    write_string(&mut stream, b"2.34.8");
    write_integer(&mut stream, 1);
    write_integer(&mut stream, STDERR_LAST);
    stream.flush().expect("root registration handshake flushes");
    assert_eq!(read_integer(&mut stream), 11);
    assert!(read_string(&mut stream).starts_with("/nix/store/"));
    write_integer(&mut stream, STDERR_LAST);
    write_integer(&mut stream, 1);
    stream
        .flush()
        .expect("root registration temporary response flushes");
    assert_eq!(read_integer(&mut stream), 12);
    assert!(read_string(&mut stream).contains("gc-roots"));
    write_integer(&mut stream, STDERR_LAST);
    write_integer(&mut stream, 1);
    stream
        .flush()
        .expect("root registration indirect response flushes");
}

fn complete_handshake(input: &mut impl Write, output: &mut impl Read) {
    write_integer(input, CLIENT_WORKER_MAGIC);
    write_integer(input, LATEST_WORKER_VERSION.to_wire());
    write_integer(input, 0);
    input.flush().expect("handshake flushes");

    assert_eq!(read_integer(output), SERVER_WORKER_MAGIC);
    assert_eq!(
        read_integer(output),
        LATEST_WORKER_VERSION.to_wire(),
        "server sends its protocol version"
    );
    assert_eq!(read_integer(output), 0, "server has no features");

    write_integer(input, 0);
    write_integer(input, 0);
    input.flush().expect("post-handshake flushes");

    assert_eq!(read_string(output), "telchar");
    assert_eq!(read_integer(output), 0);
    assert_eq!(read_integer(output), STDERR_LAST);
}

fn wait_for_file_for(path: &std::path::Path, timeout: Duration, failure: &str) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(Instant::now() < deadline, "{failure}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_path_state(
    database_url: &str,
    derivation_path: &str,
    expected: telchar::persistence::SharedBuildState,
) {
    wait_for_path_state_for(
        database_url,
        derivation_path,
        expected,
        Duration::from_secs(2),
    );
}

fn wait_for_path_state_for(
    database_url: &str,
    derivation_path: &str,
    expected: telchar::persistence::SharedBuildState,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let last_state = telchar::persistence::read_shared_build(database_url, derivation_path)
            .expect("shared build reads")
            .map(|build| build.state);
        if last_state == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "shared build did not reach expected state: {last_state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_build_success(mut output: impl Read) -> Result<(), String> {
    loop {
        let frame = try_read_integer(&mut output)?;
        if frame == STDERR_LAST {
            break;
        }
        if frame != nix_worker_protocol::STDERR_NEXT {
            return Err(format!("unexpected stderr frame {frame}"));
        }
        try_read_string(&mut output)?;
    }
    let status = try_read_integer(&mut output)?;
    if status != 0 {
        return Err(format!("unexpected build status {status}"));
    }
    let error_message = try_read_string(&mut output)?;
    if !error_message.is_empty() {
        return Err(format!("unexpected build error {error_message:?}"));
    }
    for _ in 0..7 {
        try_read_integer(&mut output)?;
    }
    Ok(())
}

fn try_read_integer(input: &mut impl Read) -> Result<u64, String> {
    let mut bytes = [0; 8];
    input
        .read_exact(&mut bytes)
        .map_err(|error| format!("worker integer read failed: {error}"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn try_read_string(input: &mut impl Read) -> Result<String, String> {
    let length = try_read_integer(input)? as usize;
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .map_err(|error| format!("worker string read failed: {error}"))?;
    let padding = (8 - length % 8) % 8;
    let mut padding_bytes = vec![0; padding];
    input
        .read_exact(&mut padding_bytes)
        .map_err(|error| format!("worker string padding read failed: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("worker string is invalid UTF-8: {error}"))
}

fn write_integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("worker integer writes");
}

fn write_add_multiple_to_store_metadata(output: &mut impl Write, nar_size: u64) {
    let mut metadata = Vec::new();
    write_integer(&mut metadata, 1);
    write_string(
        &mut metadata,
        b"/nix/store/11111111111111111111111111111111-telchar-disk-reserve",
    );
    write_string(&mut metadata, b"");
    write_string(&mut metadata, b"");
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, nar_size);
    write_integer(&mut metadata, 0);
    write_integer(&mut metadata, 0);
    write_string(&mut metadata, b"");

    write_integer(output, 44);
    write_integer(output, 0);
    write_integer(output, 0);
    write_integer(output, metadata.len() as u64);
    output.write_all(&metadata).expect("metadata frame writes");
}

fn write_input_build_derivation(output: &mut impl Write, system: &str, mode: u64) {
    let source = b"/nix/store/22222222222222222222222222222222-telchar-input";
    let store_output = b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract";
    write_integer(output, 36);
    write_string(
        output,
        b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
    );
    write_integer(output, 1);
    write_string(output, b"out");
    write_string(output, store_output);
    write_string(output, b"");
    write_string(output, b"");
    write_integer(output, 1);
    write_string(output, source);
    write_string(output, system.as_bytes());
    write_string(output, b"/bin/sh");
    write_integer(output, 2);
    write_string(output, b"-c");
    write_string(output, b"printf telchar-remote-build > $out");
    write_integer(output, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"telchar-gate-3-contract".as_slice()),
        (b"out".as_slice(), store_output.as_slice()),
        (b"system".as_slice(), system.as_bytes()),
    ] {
        write_string(output, key);
        write_string(output, value);
    }
    write_integer(output, mode);
}

fn write_gate_3_build_derivation(output: &mut impl Write, system: &str, mode: u64) {
    write_build_derivation(
        output,
        b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv",
        system,
        mode,
    );
}

fn write_build_derivation(
    output: &mut impl Write,
    derivation_path: &[u8],
    system: &str,
    mode: u64,
) {
    let store_output = b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract";
    write_integer(output, 36);
    write_string(output, derivation_path);
    write_integer(output, 1);
    write_string(output, b"out");
    write_string(output, store_output);
    write_string(output, b"");
    write_string(output, b"");
    write_integer(output, 0);
    write_string(output, system.as_bytes());
    write_string(output, b"/bin/sh");
    write_integer(output, 2);
    write_string(output, b"-c");
    write_string(output, b"printf telchar-remote-build > $out");
    write_integer(output, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), derivation_name(derivation_path)),
        (b"out".as_slice(), store_output.as_slice()),
        (b"system".as_slice(), system.as_bytes()),
    ] {
        write_string(output, key);
        write_string(output, value);
    }
    write_integer(output, mode);
}

fn derivation_name(path: &[u8]) -> &[u8] {
    path.rsplit(|byte| *byte == b'/')
        .next()
        .and_then(|name| name.strip_suffix(b".drv"))
        .and_then(|name| name.get(33..))
        .expect("derivation path has a valid name")
}

fn write_string(output: &mut impl Write, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.write_all(value).expect("worker string writes");
    output
        .write_all(&[0; 7][..(8 - value.len() % 8) % 8])
        .expect("worker string padding writes");
}

fn regular_nar(contents: &[u8]) -> Vec<u8> {
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(".as_slice(),
        b"type".as_slice(),
        b"regular".as_slice(),
        b"contents".as_slice(),
        contents,
        b")".as_slice(),
    ] {
        write_string(&mut nar, value);
    }
    nar
}

fn read_integer(input: &mut impl Read) -> u64 {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes).expect("worker integer reads");
    u64::from_le_bytes(bytes)
}

fn read_string(input: &mut impl Read) -> String {
    let length = read_integer(input) as usize;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).expect("worker string reads");
    let padding = (8 - length % 8) % 8;
    let mut padding_bytes = vec![0; padding];
    input
        .read_exact(&mut padding_bytes)
        .expect("worker padding reads");
    assert!(padding_bytes.iter().all(|byte| *byte == 0));
    String::from_utf8(bytes).expect("worker string UTF-8")
}
