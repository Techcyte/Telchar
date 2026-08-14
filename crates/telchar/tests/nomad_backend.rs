//! Tests nomad backend contracts and failure boundaries, including renders operator selected driver and stable backend bound job.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use serde_json::Value;
use sha2::{Digest, Sha256};
use telchar::backend::routing::ConfiguredBackends;
use telchar::backend::{
    BackendKind, BackendTarget, BuildBackend, BuildExecution, BuildStatus, OutputTrust,
};
use telchar::nomad::backend::{
    deterministic_job_name, render_job, NomadClient, NomadExecutionState,
};
use telchar::service::config::ServiceConfig;

mod support;

static CONFIGURATION_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gateway_store_endpoint() -> telchar::store::daemon::GatewayStoreEndpoint {
    telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///definitely-missing/telchar-gateway.sock",
    )
    .expect("gateway endpoint is valid")
}

#[path = "nomad_backend/client.rs"]
mod client;
#[path = "nomad_backend/execution.rs"]
mod execution;
#[path = "nomad_backend/identity.rs"]
mod identity;
#[path = "nomad_backend/rendering.rs"]
mod rendering;

fn read_http_request_with_body(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout sets");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).expect("request reads");
        assert!(count > 0, "request ended before body");
        request.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&request[..header_end]).expect("headers are UTF-8");
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap_or_default()
                .parse::<usize>()
                .expect("content length parses");
            if request.len() >= header_end + 4 + length {
                return String::from_utf8(request).expect("request is UTF-8");
            }
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout sets");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).expect("request reads");
        assert!(count > 0, "request ended before headers");
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            return String::from_utf8(request).expect("request is UTF-8");
        }
    }
}

fn write_json_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("response writes");
}

fn load_service_config(
    root: &std::path::Path,
    endpoint: &str,
    token_file: Option<&std::path::Path>,
) -> ServiceConfig {
    let config_path = root.join("client.toml");
    let token = token_file
        .map(|path| format!("token_file = {:?}\n", path))
        .unwrap_or_default();
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad-test"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "{endpoint}"
namespace = "telchar"
{token}driver = "raw_exec"
job_name_scope = "telchar-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "{endpoint}"
jwks_url = "{endpoint}/.well-known/jwks.json"
audience = "telchar-transfer"

[backends.nomad.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

[backends.nomad.transfer_limits]
maximum_manifest_paths = 1024
maximum_manifest_bytes = 1048576
maximum_input_nar_bytes = 1073741824
maximum_total_input_bytes = 8589934592
maximum_output_nar_bytes = 1073741824
maximum_total_output_bytes = 8589934592
maximum_frame_metadata_bytes = 65536
stream_buffer_bytes = 262144
maximum_live_log_chunk_bytes = 65536
live_log_queue_bytes = 1048576
transfer_idle_timeout_seconds = 30
setup_timeout_seconds = 300
output_collection_timeout_seconds = 300
maximum_connection_lifetime_seconds = 3600
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.resources]
cpu_mhz = 1000
memory_mb = 512
disk_mb = 1024

[backends.nomad.driver_config]
command = "/bin/true"
"#
        ),
    )
    .expect("configuration writes");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("configuration permissions set");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    config
}

fn load_nomad_config(
    root: &std::path::Path,
    endpoint: &str,
    token_file: Option<&std::path::Path>,
) -> telchar::service::config::NomadBackendConfig {
    load_service_config(root, endpoint, token_file).nomad_backends()[0].clone()
}

fn admitted_request() -> telchar::build::BuildRequest {
    let output = b"/nix/store/11111111111111111111111111111111-output";
    let mut wire = Vec::new();
    write_worker_string(
        &mut wire,
        b"/nix/store/00000000000000000000000000000000-build.drv",
    );
    write_worker_integer(&mut wire, 1);
    write_worker_string(&mut wire, b"out");
    write_worker_string(&mut wire, output);
    write_worker_string(&mut wire, b"");
    write_worker_string(&mut wire, b"");
    write_worker_integer(&mut wire, 0);
    write_worker_string(&mut wire, b"x86_64-linux");
    write_worker_string(&mut wire, b"/bin/sh");
    write_worker_integer(&mut wire, 2);
    write_worker_string(&mut wire, b"-c");
    write_worker_string(&mut wire, b"printf nomad > $out");
    write_worker_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"build".as_slice()),
        (b"out".as_slice(), output.as_slice()),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_worker_string(&mut wire, key);
        write_worker_string(&mut wire, value);
    }
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
    let request = reader
        .complete_build_derivation()
        .expect("worker request parses");
    let backend = BackendTarget::new(
        "nomad-test",
        BackendKind::Nomad,
        "x86_64-linux",
        std::iter::empty::<&str>(),
    )
    .expect("target creates");
    telchar::build::BuildRequest::from_worker_request(&request, &[backend]).expect("request admits")
}

fn write_worker_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_worker_string(output: &mut Vec<u8>, value: &[u8]) {
    write_worker_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len().next_multiple_of(8), 0);
}

fn fixture_root() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("telchar-nomad-backend-{nonce}"));
    fs::create_dir(&root).expect("fixture creates");
    root
}
