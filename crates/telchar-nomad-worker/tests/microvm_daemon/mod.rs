use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use telchar::nomad::protocol::BuildOutcome;
use telchar::store::daemon::GatewayStoreEndpoint;

struct Daemon(std::process::Child);

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
    let (endpoint, provider) = super::provider_server(Some(binding));
    let home = root.join("daemon");
    std::fs::create_dir(&home)?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    let roots = home.join("gcroots");
    std::fs::create_dir(&roots)?;
    let socket = home.join("ipc.sock");
    let config = home.join("service.toml");
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
maximum_concurrent_builds = 1
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 20
provider_timeout_seconds = 2
cleanup_interval_seconds = 60
callback_url = "wss://localhost:{port}/microvm"
callback_address = "{address}"
staging_directory = "{staging}"
maximum_paths = 10
maximum_nar_bytes = 1048576
maximum_total_bytes = 2097152
metadata_bytes = 8192
chunk_bytes = 262144
"#,
            port = address.port(),
            staging = root.display()
        ),
    )?;
    let uid = Command::new("id").arg("-u").output()?;
    assert!(uid.status.success());
    assert!(uid.stderr.is_empty());
    let binary = std::env::var_os("TELCHAR_TEST_DAEMON_BIN")
        .expect("TELCHAR_TEST_DAEMON_BIN must identify the freshly built daemon");
    let mut daemon = Daemon(
        Command::new(binary)
            .args(["daemon", "--socket"])
            .arg(&socket)
            .args([
                "--frontend-uid",
                String::from_utf8(uid.stdout).unwrap().trim(),
            ])
            .env_clear()
            .env("HOME", &home)
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_ACCESS_KEY_ID", "fixture-key")
            .env("AWS_SECRET_ACCESS_KEY", "fixture-secret")
            .env("TELCHAR_TEST_MICROVM_ENDPOINT", endpoint)
            .env("TELCHAR_CONFIG", config)
            .env("TELCHAR_DATABASE_URL", database)
            .env("TELCHAR_GATEWAY_STORE_URI", store.to_string())
            .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &roots)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "daemon startup deadline");
        if let Some(status) = daemon.0.try_wait()? {
            let mut diagnostic = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut diagnostic)?;
            panic!("daemon exited {status}: {diagnostic}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let client = nix_worker_protocol::WorkerClient::connect(stream.try_clone()?)?;
    let mut wire = Vec::new();
    use nix_worker_protocol::{write_worker_byte_string as bytes, write_worker_integer as word};
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
    stream.write_all(&wire)?;
    stream.flush()?;
    let mut expected = Vec::new();
    nix_worker_protocol::write_build_derivation_success_response(
        &mut expected,
        client.profile().version,
        false,
    )?;
    let mut actual = vec![0; expected.len()];
    stream.read_exact(&mut actual)?;
    if actual != expected {
        daemon.0.kill()?;
        daemon.0.wait()?;
        let mut diagnostic = String::new();
        daemon
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostic)?;
        panic!("daemon result {actual:?}: {diagnostic}");
    }
    drop(stream);
    drop(client);
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
    let mut stderr = String::new();
    daemon
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)?;
    daemon
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)?;
    assert!(stdout.is_empty());
    assert!(!stderr.contains("ERROR"), "{stderr}");
    Ok(BuildOutcome::Built)
}
