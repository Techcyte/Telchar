//! Operator-owned allocation tasks and worker environment contracts.

use super::*;

fn configured_job(root: &std::path::Path, extra: &str) -> std::io::Result<Value> {
    load_service_config(root, "http://nomad.example:4646", None);
    let path = root.join("client.toml");
    let configured = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{configured}\n{extra}")).unwrap();
    let saved = std::env::var_os("TELCHAR_CONFIG");
    unsafe { std::env::set_var("TELCHAR_CONFIG", &path) };
    let result = ServiceConfig::load()
        .and_then(|config| render_job(&config.nomad_backends()[0], b"sidecar-build"));
    unsafe {
        match saved {
            Some(value) => std::env::set_var("TELCHAR_CONFIG", value),
            None => std::env::remove_var("TELCHAR_CONFIG"),
        }
    }
    result
}

const SIDECAR: &str = r#"
[backends.nomad.nomad-test.environment]
RUST_LOG = "info,telchar_nomad_worker=debug"
OTEL_EXPORTER_OTLP_ENDPOINT = "http://127.0.0.1:4318"

[[backends.nomad.nomad-test.sidecars]]
name = "telemetry"
driver = "docker"
kill_timeout_seconds = 30

[backends.nomad.nomad-test.sidecars.driver_config]
image = "operator.example/collector:1"
args = ["--config=/local/collector.yaml"]

[backends.nomad.nomad-test.sidecars.environment]
COLLECTOR_LOG_LEVEL = "warn"

[backends.nomad.nomad-test.sidecars.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 64

[[backends.nomad.nomad-test.sidecars.templates]]
destination = "local/collector.yaml"
data = "receivers: {}\n"
"#;

#[test]
fn renders_optional_sidecars_and_worker_environment() {
    let _guard = CONFIGURATION_TESTS.lock().unwrap();
    let root = fixture_root();
    let job = configured_job(&root, SIDECAR).expect("operator tasks configure");
    let group = &job["Job"]["TaskGroups"][0];
    assert_eq!(group["Networks"][0]["Mode"], "bridge");
    let tasks = group["Tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["Name"], "build");
    assert_eq!(tasks[0]["Env"]["RUST_LOG"], "info,telchar_nomad_worker=debug");
    assert_eq!(tasks[0]["Env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], "http://127.0.0.1:4318");
    assert_eq!(tasks[0]["Env"]["TELCHAR_TASK"], "build");
    assert_eq!(tasks[1]["Name"], "telemetry");
    assert_eq!(tasks[1]["Driver"], "docker");
    assert_eq!(tasks[1]["Lifecycle"], serde_json::json!({"Hook":"prestart","Sidecar":true}));
    assert_eq!(tasks[1]["KillTimeout"], 30_000_000_000_u64);
    assert_eq!(tasks[1]["Resources"]["MemoryMB"], 128);
    assert_eq!(tasks[1]["Env"]["COLLECTOR_LOG_LEVEL"], "warn");
    assert!(tasks[1]["Env"]["TELCHAR_TRANSFER_CAPABILITY"].is_null());
    assert_eq!(tasks[1]["Templates"][0]["DestPath"], "local/collector.yaml");
    assert_eq!(tasks[1]["Templates"][0]["EmbeddedTmpl"], "receivers: {}\n");
    assert_eq!(tasks[1]["Templates"][0]["ChangeMode"], "noop");
    let plain = configured_job(&root, "").unwrap();
    assert_eq!(plain["Job"]["TaskGroups"][0]["Tasks"].as_array().unwrap().len(), 1);
    assert!(plain["Job"]["TaskGroups"][0]["Networks"].is_null());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_invalid_sidecars_and_protected_worker_environment() {
    let _guard = CONFIGURATION_TESTS.lock().unwrap();
    let root = fixture_root();
    for extra in [
        SIDECAR.replace("name = \"telemetry\"", "name = \"build\""),
        SIDECAR.replace("name = \"telemetry\"", "name = \"prestart\""),
        SIDECAR.replace("name = \"telemetry\"", "name = \"connect-proxy-test\""),
        SIDECAR.replace("kill_timeout_seconds = 30", "kill_timeout_seconds = 0"),
        SIDECAR.replace("memory_mb = 128", "memory_mb = 0"),
        SIDECAR.replace("local/collector.yaml", "local/../secrets/token"),
        SIDECAR.replace("local/collector.yaml", "/tmp/collector.yaml"),
        SIDECAR.replace("RUST_LOG =", "TELCHAR_TASK ="),
        SIDECAR.replace("RUST_LOG =", "TRACEPARENT ="),
        SIDECAR.replace("RUST_LOG =", "NOMAD_TOKEN ="),
        SIDECAR.replace("RUST_LOG =", "INVALID-NAME ="),
        SIDECAR.replace("receivers: {}", &"x".repeat(65_537)),
    ] {
        assert_eq!(configured_job(&root, &extra).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }
    let task = SIDECAR.split_once("[[backends.nomad.nomad-test.sidecars]]").unwrap().1;
    let duplicate = format!("{SIDECAR}\n[[backends.nomad.nomad-test.sidecars]]{task}");
    assert!(configured_job(&root, &duplicate).is_err());
    fs::remove_dir_all(root).unwrap();
}
