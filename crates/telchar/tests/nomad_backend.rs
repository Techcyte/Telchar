use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

use telchar::config::ServiceConfig;
use telchar::nomad_backend::{deterministic_job_name, render_job};

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

fn fixture_root() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("telchar-nomad-backend-{nonce}"));
    fs::create_dir(&root).expect("fixture creates");
    root
}
