//! Tests executor execution contracts and failure boundaries, including executor owns running work after submitter disconnects.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};

mod support;

use support::postgres::PostgresFixture;
use telchar::backend::{BackendKind, BackendTarget};
use telchar::build_request::BuildRequest;
use telchar::executor_service::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorExecutionState, ExecutorRequest, ExecutorResult,
    ExecutorSpecification, send_request,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn executor_owns_running_work_after_submitter_disconnects() {
    let database = PostgresFixture::start();
    let root = temporary_root();
    fs::create_dir_all(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("fixture root is private");
    let socket = root.join("executor.sock");
    let helper = root.join("build-helper");
    let release = root.join("release");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-executor-service\"]]}}\\n'\n",
            release.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executes");
    let mut executor = executor_command(&socket, database.url(), &helper)
        .spawn()
        .expect("executor starts");
    wait_for_socket(&socket, &mut executor);

    let submit = ExecutorRequest::Submit {
        version: EXECUTOR_PROTOCOL_VERSION,
        backend_execution_id: "local-owned-running".into(),
        idempotency_key: "owned-running:1".into(),
        specification: Box::new(execution_specification()),
    };
    let response = request(&socket, &submit);
    assert_eq!(response.result, ExecutorResult::Accepted);
    drop(response);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = request(
            &socket,
            &ExecutorRequest::Status {
                version: EXECUTOR_PROTOCOL_VERSION,
                backend_execution_id: "local-owned-running".into(),
            },
        );
        if status.execution.as_ref().map(|execution| execution.state)
            == Some(ExecutorExecutionState::Running)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "execution did not become running"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        executor
            .try_wait()
            .expect("executor status reads")
            .is_none()
    );

    fs::write(&release, b"release").expect("execution releases");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = request(
            &socket,
            &ExecutorRequest::Status {
                version: EXECUTOR_PROTOCOL_VERSION,
                backend_execution_id: "local-owned-running".into(),
            },
        );
        if status.execution.as_ref().map(|execution| execution.state)
            == Some(ExecutorExecutionState::Succeeded)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "execution did not persist terminal result"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = telchar::persistence::read_local_backend_execution_result(
        database.url(),
        "local-owned-running",
    )
    .expect("terminal result reads")
    .expect("terminal result exists");
    assert_eq!(result.classification, "succeeded");
    assert_eq!(
        result.result_metadata,
        serde_json::json!({
            "status": "built",
            "outputs": [{
                "name": "out",
                "path": "/nix/store/11111111111111111111111111111111-executor-service"
            }]
        })
    );
    executor.kill().expect("executor stops");
    let _ = executor.wait();
    let _ = fs::remove_dir_all(root);
}

fn request(
    socket: &Path,
    request: &ExecutorRequest,
) -> telchar::executor_service::ExecutorResponse {
    let mut stream = UnixStream::connect(socket).expect("executor connects");
    send_request(&mut stream, request).expect("executor responds")
}

fn executor_command(socket: &Path, database_url: &str, helper: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
    command
        .arg("executor")
        .env("TELCHAR_DATABASE_URL", database_url)
        .env("TELCHAR_EXECUTOR_SOCKET", socket)
        .env(
            "TELCHAR_EXECUTOR_UID",
            rustix::process::getuid().as_raw().to_string(),
        )
        .env("TELCHAR_TEST_BUILD_HELPER", helper)
        .env("TELCHAR_GATEWAY_STORE_URI", "unix:///fixed-gateway.sock")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn execution_specification() -> ExecutorSpecification {
    let derivation_path = b"/nix/store/00000000000000000000000000000000-executor-service.drv";
    let output_path = b"/nix/store/11111111111111111111111111111111-executor-service";
    let mut wire = Vec::new();
    write_string(&mut wire, derivation_path);
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(&mut wire, output_path);
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 2);
    write_string(&mut wire, b"-c");
    write_string(&mut wire, b"exit 0");
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"executor-service".as_slice()),
        (b"out".as_slice(), output_path),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
    let worker = reader
        .complete_build_derivation()
        .expect("worker request parses");
    ExecutorSpecification {
        request_id: "owned-running".into(),
        timeout_seconds: 30,
        build: BuildRequest::from_worker_request(
            &worker,
            &[BackendTarget::new(
                "fixture",
                BackendKind::Local,
                "x86_64-linux",
                [] as [&str; 0],
            )
            .expect("backend parses")],
        )
        .expect("request admits"),
    }
}

fn write_integer(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_string(bytes: &mut Vec<u8>, value: &[u8]) {
    write_integer(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
    bytes.resize(bytes.len() + (8 - value.len() % 8) % 8, 0);
}

fn wait_for_socket(socket: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if socket.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("executor status reads") {
            let stderr = child
                .stderr
                .take()
                .map(|mut stderr| {
                    let mut bytes = Vec::new();
                    std::io::Read::read_to_end(&mut stderr, &mut bytes)
                        .expect("executor stderr reads");
                    String::from_utf8_lossy(&bytes).into_owned()
                })
                .unwrap_or_default();
            panic!("executor exited before readiness: {status}: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("executor socket did not become ready");
}

fn temporary_root() -> PathBuf {
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock follows epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "tee-{}-{}-{sequence}",
        std::process::id(),
        nanos % 1_000_000
    ))
}
