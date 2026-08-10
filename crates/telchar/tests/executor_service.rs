use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod support;

use support::postgres::PostgresFixture;
use telchar::executor_service::{
    send_request, ExecutorExecutionState, ExecutorRequest, ExecutorResult,
    EXECUTOR_PROTOCOL_VERSION,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn executor_service_persists_idempotent_submit_and_status_across_restart() {
    let database = PostgresFixture::start();
    let root = temporary_root();
    let socket = root.join("executor.sock");
    let mut first = executor_command(&socket, database.url())
        .spawn()
        .expect("executor starts");
    wait_for_socket(&socket, &mut first);
    let mut contended = executor_command(&root.join("contended.sock"), database.url())
        .spawn()
        .expect("contended executor starts");
    let status = wait_with_deadline(&mut contended, Duration::from_secs(2));
    assert!(!status.success(), "contended executor acquired ownership");
    assert!(!root.join("contended.sock").exists());

    let submit = ExecutorRequest::Submit {
        version: EXECUTOR_PROTOCOL_VERSION,
        backend_execution_id: "local-execution-1".into(),
        idempotency_key: "request-1:1".into(),
        specification: b"bounded-execution-specification".to_vec(),
    };
    let created = request(&socket, &submit);
    let repeated = request(&socket, &submit);
    assert_eq!(created, repeated);
    assert_eq!(created.result, ExecutorResult::Accepted);
    assert_eq!(
        created.execution.as_ref().expect("execution returns").state,
        ExecutorExecutionState::Accepted
    );

    let conflicting = request(
        &socket,
        &ExecutorRequest::Submit {
            version: EXECUTOR_PROTOCOL_VERSION,
            backend_execution_id: "local-execution-1".into(),
            idempotency_key: "request-1:1".into(),
            specification: b"different-specification".to_vec(),
        },
    );
    assert_eq!(conflicting.result, ExecutorResult::Conflict);
    assert!(conflicting.execution.is_none());

    first.kill().expect("first executor stops");
    let _ = first.wait();
    let _ = fs::remove_file(&socket);
    let mut second = executor_command(&socket, database.url())
        .spawn()
        .expect("replacement executor starts");
    wait_for_socket(&socket, &mut second);

    let status = request(
        &socket,
        &ExecutorRequest::Status {
            version: EXECUTOR_PROTOCOL_VERSION,
            backend_execution_id: "local-execution-1".into(),
        },
    );
    assert_eq!(status.result, ExecutorResult::Found);
    assert_eq!(status.execution, created.execution);
    assert_eq!(
        database
            .connect()
            .query_one("SELECT count(*) FROM local_backend_executions", &[])
            .expect("registry count reads")
            .get::<_, i64>(0),
        1
    );

    second.kill().expect("replacement executor stops");
    let _ = second.wait();
    let _ = fs::remove_dir_all(root);
}

fn request(
    socket: &Path,
    request: &ExecutorRequest,
) -> telchar::executor_service::ExecutorResponse {
    let mut stream = UnixStream::connect(socket).expect("executor connects");
    send_request(&mut stream, request).expect("executor responds")
}

fn executor_command(socket: &Path, database_url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_telchar"));
    command
        .arg("executor")
        .env("TELCHAR_DATABASE_URL", database_url)
        .env("TELCHAR_EXECUTOR_SOCKET", socket)
        .env(
            "TELCHAR_EXECUTOR_UID",
            rustix::process::getuid().as_raw().to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn wait_with_deadline(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("child status reads") {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("timed out child stops");
            return child.wait().expect("timed out child reaps");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
        "telchar-executor-service-{}-{nanos}-{sequence}",
        std::process::id()
    ))
}
