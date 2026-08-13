use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use serde_json::Value;
use sha2::{Digest, Sha256};
use telchar::backend::{
    BackendKind, BackendTarget, BuildBackend, BuildExecution, BuildStatus, OutputTrust,
};
use telchar::backend_routing::ConfiguredBackends;
use telchar::config::ServiceConfig;
use telchar::nomad_backend::{
    deterministic_job_name, render_job, NomadClient, NomadExecutionState,
};

static CONFIGURATION_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn renders_operator_selected_driver_and_stable_backend_bound_job() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        r#"
[[backends.nomad]]
name = "nomad-arm"
system = "aarch64-linux"
supported_features = ["big-parallel"]
maximum_concurrent_builds = 4
endpoint = "http://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "telchar-prod"
poll_interval_seconds = 2
runtime_limit_seconds = 3600
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
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
authentication_lifetime_seconds = 300
clock_skew_seconds = 30
nonce_retention_seconds = 600
reconnect_timeout_seconds = 30
maximum_diagnostic_bytes = 65536

[backends.nomad.prestart]
driver = "raw_exec"
timeout_seconds = 120

[backends.nomad.prestart.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 256

[backends.nomad.prestart.driver_config]
command = "/opt/operator/bin/configure-nix"
args = ["/alloc/data/nix"]

[backends.nomad.resources]
cpu_mhz = 2000
memory_mb = 4096
disk_mb = 16384

[backends.nomad.driver_config]
command = "/opt/telchar/bin/worker"
args = ["--stdio"]
"#,
    )
    .expect("configuration writes");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("configuration permissions set");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");
    let backend = &config.nomad_backends()[0];

    let first = deterministic_job_name(backend, b"shared-build-key");
    let second = deterministic_job_name(backend, b"shared-build-key");
    let other = deterministic_job_name(backend, b"other-build-key");
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert!(first.starts_with("telchar-prod-"));

    let job = render_job(backend, b"shared-build-key").expect("job renders");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_TRANSFER_AUTHENTICATION"],
        "workload-identity"
    );
    assert_eq!(job["Job"]["ID"], first);
    assert_eq!(job["Job"]["Namespace"], "telchar");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Driver"],
        "raw_exec"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Config"]["command"],
        "/opt/telchar/bin/worker"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Resources"]["CPU"],
        2000
    );
    assert_eq!(job["Job"]["Meta"]["telchar_backend"], "nomad-arm");
    assert_eq!(job["Job"]["Meta"]["telchar_system"], "aarch64-linux");
    assert_eq!(job["Job"]["TaskGroups"][0]["Tasks"][0]["Name"], "prestart");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Lifecycle"]["Hook"],
        "prestart"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Lifecycle"]["Sidecar"],
        false
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Config"]["command"],
        "/opt/operator/bin/configure-nix"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["KillTimeout"],
        120_000_000_000_u64
    );
    assert_eq!(job["Job"]["TaskGroups"][0]["Tasks"][1]["Name"], "build");
    let environment = &job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"];
    assert_eq!(environment["TELCHAR_BACKEND"], "nomad-arm");
    assert_eq!(environment["TELCHAR_NAMESPACE"], "telchar");
    assert_eq!(environment["TELCHAR_JOB_ID"], first);
    assert_eq!(environment["TELCHAR_TASK"], "build");
    assert_eq!(
        environment["TELCHAR_SHARED_BUILD_DIGEST"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(b"shared-build-key"))
    );
    assert_eq!(
        environment["TELCHAR_TRANSFER_ENDPOINT"],
        "ws://telchar.example:7443"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_NIX_STORE_URI"],
        "unix:///nix/var/nix/daemon-socket/socket"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_TRANSFER_CHUNK_BYTES"],
        "262144"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Identity"]["Env"],
        true
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Identity"]["File"],
        false
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Identity"]["Audiences"][0],
        "telchar-transfer"
    );
    assert!(job["Job"]["TaskGroups"][0]["Tasks"][1]["Identity"]["TTL"].is_null());

    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn renders_short_lived_hmac_capability_without_backend_secret() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let secret_path = root.join("transfer.key");
    fs::write(&secret_path, "backend-signing-secret\n").expect("secret writes");
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600))
        .expect("secret permissions set");
    let config_path = root.join("telchar.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad-hmac"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "http://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "telchar-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "hmac"
key_id = "primary"
secret_file = {secret_path:?}

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
command = "/opt/telchar/bin/worker"
"#
        ),
    )
    .expect("configuration writes");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("configuration permissions set");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load().expect("configuration loads");

    let job =
        render_job(&config.nomad_backends()[0], b"shared-build-key").expect("HMAC job renders");
    let environment = &job["Job"]["TaskGroups"][0]["Tasks"][0]["Env"];
    assert_eq!(environment["TELCHAR_TRANSFER_AUTHENTICATION"], "hmac");
    assert!(environment["TELCHAR_BACKEND"].is_null());
    assert!(environment["TELCHAR_NAMESPACE"].is_null());
    assert!(environment["TELCHAR_JOB_ID"].is_null());
    assert!(environment["TELCHAR_SHARED_BUILD_DIGEST"].is_null());
    assert!(environment["TELCHAR_TASK"].is_null());
    let capability = environment["TELCHAR_TRANSFER_CAPABILITY"]
        .as_str()
        .expect("capability is rendered");
    let (encoded_claims, encoded_signature) = capability
        .split_once('.')
        .expect("capability has claims and signature");
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded_claims)
            .expect("claims decode"),
    )
    .expect("claims parse");
    assert_eq!(claims["version"], 1);
    assert_eq!(claims["key_id"], "primary");
    assert_eq!(claims["backend"], "nomad-hmac");
    assert_eq!(claims["namespace"], "telchar");
    assert_eq!(
        claims["job_id"],
        deterministic_job_name(&config.nomad_backends()[0], b"shared-build-key")
    );
    assert_eq!(
        claims["shared_build_digest"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(b"shared-build-key"))
    );
    assert!(claims["allocation_id"].is_null());
    assert!(claims["request_key"]
        .as_str()
        .is_some_and(|key| !key.is_empty()));
    assert!(
        claims["expires_at"].as_u64().expect("expiry is numeric")
            > claims["issued_at"].as_u64().expect("issue time is numeric")
    );
    let mut verifier = Hmac::<Sha256>::new_from_slice(b"backend-signing-secret\n")
        .expect("fixture signing key is valid");
    verifier.update(encoded_claims.as_bytes());
    verifier
        .verify_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded_signature)
                .expect("signature decodes"),
        )
        .expect("capability signature verifies");
    assert!(!capability.contains("backend-signing-secret"));
    assert!(job["Job"]["TaskGroups"][0]["Tasks"][0]["Identity"].is_null());

    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn submits_one_deterministic_job_with_operator_authentication() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let token_root = fixture_root();
    let token_path = token_root.join("nomad.token");
    fs::write(&token_path, "fixture-token\n").expect("token writes");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))
        .expect("token permissions set");
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let server = thread::spawn(move || {
        let mut stream = listener.accept().expect("request accepts").0;
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
                let headers =
                    std::str::from_utf8(&request[..header_end]).expect("headers are UTF-8");
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .expect("content length is present")
                    .parse::<usize>()
                    .expect("content length parses");
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        let request = String::from_utf8(request).expect("request is UTF-8");
        assert!(request.starts_with("POST /v1/jobs?namespace=telchar HTTP/1.1\r\n"));
        assert!(request.contains("x-nomad-token: fixture-token\r\n"));
        assert!(request.contains("\"ID\":\"telchar-test-"));
        assert!(request.contains("\"Driver\":\"raw_exec\""));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 39\r\nConnection: close\r\n\r\n{\"EvalID\":\"evaluation-1\",\"Warnings\":\"\"}")
            .expect("response writes");
    });
    let config = load_nomad_config(&token_root, &endpoint, Some(&token_path));
    let client = NomadClient::new(config.clone()).expect("Nomad client constructs");
    let submission = client
        .submit(b"shared-build-key")
        .expect("Nomad job submits");
    assert_eq!(
        submission.job_id(),
        deterministic_job_name(&config, b"shared-build-key")
    );
    assert_eq!(submission.evaluation_id(), "evaluation-1");
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(token_root).expect("fixture removes");
}

#[test]
fn rejects_invalid_configured_tls_material() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let ca_path = root.join("ca.pem");
    fs::write(&ca_path, "not a certificate\n").expect("CA writes");
    fs::set_permissions(&ca_path, fs::Permissions::from_mode(0o644)).expect("CA permissions set");
    let config_path = root.join("tls.toml");
    fs::write(
        &config_path,
        format!(
            r#"
[[backends.nomad]]
name = "nomad-tls"
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "https://nomad.example:4646"
namespace = "telchar"
ca_certificate_file = {:?}
driver = "raw_exec"
job_name_scope = "telchar-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "wss://telchar.example:7443"

[backends.nomad.transfer_authentication]
mode = "workload-identity"
issuer = "https://nomad.example:4646"
jwks_url = "https://nomad.example:4646/.well-known/jwks.json"
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
"#,
            ca_path
        ),
    )
    .expect("configuration writes");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("configuration permissions set");
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &config_path) };
    let config = ServiceConfig::load()
        .expect("configuration loads")
        .nomad_backends()[0]
        .clone();
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    let error = NomadClient::new(config).err().expect("invalid CA rejects");
    assert_eq!(error.to_string(), "Nomad client configuration failed");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn monitors_only_the_exact_backend_bound_job() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let job_id = deterministic_job_name(&config, b"shared-build-key");
    let expected_job_id = job_id.clone();
    let server = thread::spawn(move || {
        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let request = read_http_request(&mut job_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{expected_job_id}?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );

        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let request = read_http_request(&mut allocations_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{expected_job_id}/allocations?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    assert_eq!(
        client.status(&job_id).expect("Nomad job status reads"),
        NomadExecutionState::Monitoring
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn verifies_exact_callback_allocation_identity() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let client = NomadClient::new(config).expect("Nomad client constructs");
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("allocation request accepts");
        assert!(read_http_request(&mut request)
            .starts_with("GET /v1/allocation/allocation-1?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(
            &mut request,
            200,
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"running","TaskStates":{"prestart":{"State":"dead"},"build":{"State":"running"}}}"#,
        );
    });

    client
        .verify_allocation("allocation-1", "job-1", "build")
        .expect("exact allocation verifies");
    server.join().expect("server joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn rejects_foreign_or_terminal_callback_allocation_identity() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    for (name, body) in [
        (
            "foreign-job",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"foreign","TaskGroup":"build","ClientStatus":"running","TaskStates":{"build":{"State":"running"}}}"#,
        ),
        (
            "foreign-task",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"running","TaskStates":{"foreign":{"State":"running"}}}"#,
        ),
        (
            "terminal",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"complete","TaskStates":{"build":{"State":"dead"}}}"#,
        ),
    ] {
        let root = fixture_root();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let config = load_nomad_config(&root, &endpoint, None);
        let client = NomadClient::new(config).expect("Nomad client constructs");
        let server = thread::spawn(move || {
            let (mut request, _) = listener.accept().expect("allocation request accepts");
            read_http_request(&mut request);
            write_json_response(&mut request, 200, body);
        });
        assert!(
            client
                .verify_allocation("allocation-1", "job-1", "build")
                .is_err(),
            "{name} allocation must reject"
        );
        server.join().expect("server joins");
        fs::remove_dir_all(root).expect("fixture removes");
    }
}

#[test]
fn maps_allocation_terminal_states_and_missing_jobs() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    for (status, expected) in [
        ("complete", NomadExecutionState::Succeeded),
        ("failed", NomadExecutionState::Failed),
    ] {
        let root = fixture_root();
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("fixture address reads")
        );
        let config = load_nomad_config(&root, &endpoint, None);
        let job_id = deterministic_job_name(&config, b"shared-build-key");
        let expected_job_id = job_id.clone();
        let server = thread::spawn(move || {
            let (mut job_request, _) = listener.accept().expect("job request accepts");
            let _ = read_http_request(&mut job_request);
            write_json_response(
                &mut job_request,
                200,
                &format!(
                    r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
                ),
            );
            let (mut allocations_request, _) =
                listener.accept().expect("allocations request accepts");
            let _ = read_http_request(&mut allocations_request);
            write_json_response(
                &mut allocations_request,
                200,
                &format!(r#"[{{"ID":"allocation-1","ClientStatus":"{status}"}}]"#),
            );
        });
        let client = NomadClient::new(config).expect("Nomad client constructs");
        assert_eq!(client.status(&job_id).expect("status reads"), expected);
        server.join().expect("HTTP fixture joins");
        fs::remove_dir_all(root).expect("fixture removes");
    }

    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let job_id = deterministic_job_name(&config, b"shared-build-key");
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("job request accepts");
        let _ = read_http_request(&mut request);
        write_json_response(&mut request, 404, r#"{"error":"job not found"}"#);
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    assert_eq!(
        client.status(&job_id).expect("missing status reads"),
        NomadExecutionState::Missing
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn rejects_foreign_job_at_deterministic_identity() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let job_id = deterministic_job_name(&config, b"shared-build-key");
    let expected_job_id = job_id.clone();
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("job request accepts");
        let _ = read_http_request(&mut request);
        write_json_response(
            &mut request,
            200,
            &format!(
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"other","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    let error = client.status(&job_id).expect_err("foreign job rejects");
    assert_eq!(error.to_string(), "Nomad job monitoring failed");
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_exposes_deterministic_execution_identity_before_submission() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let config = load_service_config(&root, "http://127.0.0.1:4646", None);
    let backend = config.nomad_backends()[0].clone();
    let configured = ConfiguredBackends::new(&config).expect("backends configure");
    let executor = configured.executor();
    assert_eq!(
        executor
            .execution_id(backend.target(), b"shared-build-key")
            .expect("execution identity derives")
            .as_deref(),
        Some(deterministic_job_name(&backend, b"shared-build-key").as_str())
    );
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_submits_and_monitors_nomad_execution() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let server = thread::spawn(move || {
        let (mut submit_request, _) = listener.accept().expect("submit request accepts");
        let request = read_http_request_with_body(&mut submit_request);
        assert!(request.starts_with("POST /v1/jobs?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(&mut submit_request, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let request = read_http_request(&mut job_request);
        let job_id = request
            .strip_prefix("GET /v1/job/")
            .and_then(|request| request.split('?').next())
            .expect("job identity reads");
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let _ = read_http_request(&mut allocations_request);
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"complete"}]"#,
        );
    });
    let admitted = admitted_request();
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    let mut executor = ConfiguredBackends::new(&config)
        .expect("backends configure")
        .executor();
    let result = executor
        .execute(&execution)
        .expect("Nomad execution completes");
    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_adopts_exact_nomad_execution() {
    use telchar::shared_build_recovery::{AdoptedExecution, RecoveryBackend};

    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let backend = config.nomad_backends()[0].clone();
    let job_id = deterministic_job_name(&backend, b"shared-build-key");
    let expected_job_id = job_id.clone();
    let server = thread::spawn(move || {
        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let _ = read_http_request(&mut job_request);
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let _ = read_http_request(&mut allocations_request);
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );
    });
    let mut configured = ConfiguredBackends::new(&config).expect("backends configure");
    let build = telchar::persistence::SharedBuild {
        derivation_path: "/nix/store/00000000000000000000000000000000-build.drv".to_owned(),
        request_digest: [7; 32],
        state: telchar::persistence::SharedBuildState::Running,
        backend_name: "nomad-test".to_owned(),
        backend_kind: BackendKind::Nomad,
        capabilities: BackendKind::Nomad.capabilities(),
        backend_execution_id: Some(job_id),
        expected_outputs: vec!["/nix/store/11111111111111111111111111111111-output".to_owned()],
        build_request: None,
        result_metadata: None,
        failure_classification: None,
        created_at: std::time::SystemTime::now(),
        started_at: Some(std::time::SystemTime::now()),
        collecting_at: None,
        completed_at: None,
        expires_at: None,
    };
    assert_eq!(
        configured.adopt(&build).expect("execution adopts"),
        AdoptedExecution::Monitoring
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

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
) -> telchar::config::NomadBackendConfig {
    load_service_config(root, endpoint, token_file).nomad_backends()[0].clone()
}

fn admitted_request() -> telchar::build_request::BuildRequest {
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
    telchar::build_request::BuildRequest::from_worker_request(&request, &[backend])
        .expect("request admits")
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
