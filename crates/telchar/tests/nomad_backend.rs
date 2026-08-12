use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use telchar::config::ServiceConfig;
use telchar::nomad_backend::{deterministic_job_name, render_job, NomadClient};

#[test]
fn renders_operator_selected_driver_and_stable_backend_bound_job() {
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

    let job = render_job(backend, b"shared-build-key");
    assert_eq!(job["Job"]["ID"], first);
    assert_eq!(job["Job"]["Namespace"], "telchar");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Driver"],
        "raw_exec"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Config"]["command"],
        "/opt/telchar/bin/worker"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][0]["Resources"]["CPU"],
        2000
    );
    assert_eq!(job["Job"]["Meta"]["telchar_backend"], "nomad-arm");
    assert_eq!(job["Job"]["Meta"]["telchar_system"], "aarch64-linux");

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

fn load_nomad_config(
    root: &std::path::Path,
    endpoint: &str,
    token_file: Option<&std::path::Path>,
) -> telchar::config::NomadBackendConfig {
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
    config
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
