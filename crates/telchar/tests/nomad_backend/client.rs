//! Tests Nomad client.

use super::*;

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
fn maps_allocation_terminal_states_and_missing_jobs() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    for (status, expected) in [
        ("pending", NomadExecutionState::Placed),
        ("running", NomadExecutionState::Placed),
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
        write_json_response(&mut allocations_request, 200, "[]");
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    assert_eq!(
        client.status(&job_id).expect("pending status reads"),
        NomadExecutionState::Pending
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");

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
