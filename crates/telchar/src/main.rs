mod telemetry;

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use telchar::identity::{IdentityInput, normalize_requester};
use telchar::ipc::{IPC_VERSION, IpcEnvelope, IpcListener, RequesterMetadata};

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("serve-stdio") => serve_stdio(),
        Some("daemon") => daemon(),
        _ => smoke(),
    }
}

fn smoke() {
    let telemetry = telemetry::Telemetry::initialize()
        .expect("telemetry configuration must initialize before application work");

    tracing::info!(event = "application.started", "application started");
    if let Some(request_id) = std::env::var_os("TELCHAR_SMOKE_REQUEST_ID") {
        let request_id = request_id.to_string_lossy();
        let request = tracing::info_span!("request", request_id = %request_id);
        let _entered = request.enter();
        tracing::info!(event = "request.started", request_id = %request_id, "request started");
        drop(_entered);
        drop(request);
        opentelemetry::global::meter("telchar")
            .u64_counter("telchar.smoke.events")
            .build()
            .add(1, &[]);
        if std::env::var_os("TELCHAR_SMOKE_ERROR").is_some() {
            tracing::error!(event = "smoke.error", request_id = %request_id, "smoke error");
        }
    }
    println!("{}", nix_worker_protocol::protocol_name());

    telemetry.shutdown();
}

fn serve_stdio() {
    let telemetry = telemetry::Telemetry::initialize()
        .expect("telemetry configuration must initialize before application work");
    let result = run_frontend();
    if let Err(error) = &result {
        tracing::error!(
            event = "ipc.frontend.failed",
            reason = error_reason(error),
            "stdio frontend failed"
        );
    }
    telemetry.shutdown();
    result.expect("stdio frontend must connect to daemon");
}

fn run_frontend() -> io::Result<()> {
    let socket = required_path("TELCHAR_IPC_SOCKET")?;
    let fingerprint = required_string("TELCHAR_AUTHENTICATED_KEY")?;
    let requester = normalize_requester(IdentityInput::PublicKey {
        fingerprint,
        audit_subject: None,
        quota_subject: None,
        source_address: None,
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut daemon = UnixStream::connect(socket)?;
    let envelope = IpcEnvelope {
        version: IPC_VERSION,
        requester: RequesterMetadata::try_from(&requester)?,
        session_id: session_id(),
        error: None,
    };
    IpcListener::send_envelope(&mut daemon, &envelope)?;

    let mut request = daemon.try_clone()?;
    std::thread::spawn(move || {
        let result = telchar::ipc::copy_bounded(io::stdin().lock(), &mut request);
        let _ = request.shutdown(std::net::Shutdown::Write);
        if let Err(error) = result {
            tracing::warn!(
                event = "ipc.frontend.request_relay_failed",
                reason = error_reason(&error),
                "frontend request relay failed"
            );
        }
    });
    telchar::ipc::copy_bounded(daemon, io::stdout().lock())?;
    Ok(())
}

fn daemon() {
    let telemetry = telemetry::Telemetry::initialize()
        .expect("telemetry configuration must initialize before application work");
    let result = run_daemon();
    if let Err(error) = &result {
        tracing::error!(
            event = "ipc.daemon.connection_failed",
            reason = error_reason(error),
            "daemon connection failed"
        );
    }
    telemetry.shutdown();
    result.expect("daemon must serve accepted frontend");
}

fn run_daemon() -> io::Result<()> {
    let socket = daemon_socket_argument()?;
    let expected_uid = daemon_uid_argument()?;
    prepare_socket_path(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    let socket_guard = SocketGuard(socket);
    let listener = IpcListener::from_listener(listener, expected_uid);
    let envelope_timeout = duration_from_env("TELCHAR_IPC_ENVELOPE_TIMEOUT_MS", 5_000);
    let once = std::env::args().any(|argument| argument == "--once");
    if once {
        return serve_connection(&listener, envelope_timeout);
    }
    loop {
        let connection = listener.accept_pending()?;
        std::thread::spawn(move || {
            let result = connection
                .receive_envelope(envelope_timeout)
                .and_then(serve_accepted_connection);
            if let Err(error) = result {
                tracing::warn!(
                    event = "ipc.daemon.session_failed",
                    reason = error_reason(&error),
                    "frontend session failed"
                );
            }
        });
        let _ = &socket_guard;
    }
}

fn serve_connection(listener: &IpcListener, envelope_timeout: Duration) -> io::Result<()> {
    serve_accepted_connection(listener.accept_with_envelope_timeout(envelope_timeout)?)
}

fn serve_accepted_connection(mut connection: telchar::ipc::IpcConnection) -> io::Result<()> {
    tracing::info!(
        event = "ipc.daemon.session_started",
        "authenticated frontend session started"
    );
    let input = connection.stream_mut().try_clone()?;
    telchar::session::run_worker_session(
        input,
        connection.stream_mut().try_clone()?,
        protocol_session_limits(),
    )
}

fn prepare_socket_path(socket: &std::path::Path) -> io::Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| invalid("daemon socket requires a parent directory"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, Permissions::from_mode(0o700))?;
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(socket),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "daemon socket path exists and is not a socket",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn protocol_session_limits() -> nix_worker_protocol::ProtocolSessionLimits {
    let default = nix_worker_protocol::ProtocolSessionLimits::DEFAULT;
    nix_worker_protocol::ProtocolSessionLimits::new(
        default.maximum_retained_metadata_bytes,
        duration_from_env(
            "TELCHAR_WORKER_IDLE_TIMEOUT_MS",
            default.incomplete_message_idle_timeout.as_millis() as u64,
        ),
    )
}

fn daemon_socket_argument() -> io::Result<PathBuf> {
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        if argument == "--socket" {
            return arguments
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| invalid("--socket requires a path"));
        }
    }
    Err(invalid("daemon requires --socket"))
}

fn daemon_uid_argument() -> io::Result<u32> {
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        if argument == "--frontend-uid" {
            return arguments
                .next()
                .ok_or_else(|| invalid("--frontend-uid requires a value"))?
                .parse()
                .map_err(|_| invalid("--frontend-uid must be an unsigned integer"));
        }
    }
    Err(invalid("daemon requires --frontend-uid"))
}

fn required_path(name: &'static str) -> io::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("required frontend environment is absent"))
}

fn required_string(name: &'static str) -> io::Result<String> {
    std::env::var(name).map_err(|_| {
        let _ = name;
        invalid("required frontend environment is absent")
    })
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn duration_from_env(name: &str, default_ms: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(default_ms))
}

fn error_reason(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission-denied",
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "timeout",
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "invalid-input",
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "unavailable",
        _ => "io-error",
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
