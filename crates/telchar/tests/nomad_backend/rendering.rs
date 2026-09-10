//! Tests Nomad rendering.

use super::*;

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
[backends.nomad.nomad-arm]
system = "aarch64-linux"
supported_features = ["big-parallel", "telchar-ci", "telchar-memory"]
maximum_concurrent_builds = 4
endpoint = "http://nomad.example:4646"
namespace = "telchar"
node_pool = "arm-builders"
driver = "raw_exec"
job_name_scope = "telchar-prod"
poll_interval_seconds = 2
runtime_limit_seconds = 3600
transfer_endpoint = "ws://127.0.0.1:17443/callback"

[backends.nomad.nomad-arm.callback_connect]
source_service = "telchar-build-callback"
destination_service = "telchar-callback"
local_bind_port = 17443
sidecar_image = "registry.example/envoy:v1.38.4"

[[backends.nomad.nomad-arm.constraints]]
attribute = "${attr.cpu.arch}"
operator = "="
value = "amd64"

[[backends.nomad.nomad-arm.constraints]]
attribute = "${node.class}"
operator = "="
value = "general"

[backends.nomad.nomad-arm.transfer_authentication]
mode = "workload-identity"
issuer = "http://nomad.example:4646"
jwks_url = "http://nomad.example:4646/.well-known/jwks.json"
audience = "telchar-transfer"

[backends.nomad.nomad-arm.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

[backends.nomad.nomad-arm.transfer_limits]
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

[backends.nomad.nomad-arm.prestart]
driver = "raw_exec"
timeout_seconds = 120

[backends.nomad.nomad-arm.prestart.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 256

[backends.nomad.nomad-arm.prestart.driver_config]
command = "/opt/operator/bin/configure-nix"
args = ["/alloc/data/nix"]

[backends.nomad.nomad-arm.resources]
cpu_mhz = 2000
memory_mb = 4096
disk_mb = 16384

[backends.nomad.nomad-arm.priority]
minimum = 40
default = 50
maximum = 60

[[backends.nomad.nomad-arm.resource_profiles]]
name = "ci"
required_feature = "telchar-ci"
cpu_mhz = 4000
memory_mb = 8192
disk_mb = 32768
priority_minimum = 55
priority_default = 60
priority_maximum = 70

[[backends.nomad.nomad-arm.resource_profiles.constraints]]
attribute = "${node.class}"
operator = "="
value = "overflow-aws"

[[backends.nomad.nomad-arm.resource_profiles]]
name = "memory"
required_feature = "telchar-memory"
cpu_mhz = 2000
memory_mb = 32768
disk_mb = 16384
priority_minimum = 45
priority_default = 55
priority_maximum = 65

[backends.nomad.nomad-arm.driver_config]
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
    let retry = telchar::nomad::backend::deterministic_job_name_for_attempt(
        backend,
        b"shared-build-key",
        2,
    )
    .expect("retry identity derives");
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert_ne!(first, retry);
    assert!(first.ends_with("-1"));
    assert!(retry.ends_with("-2"));
    assert!(first.starts_with("telchar-prod-"));

    let trace_context = telchar_telemetry::TraceContext::new(
        Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()),
        Some("vendor=value".into()),
    )
    .expect("trace context validates");
    let job = telchar::nomad::backend::render_job_with_trace_context(
        backend,
        b"shared-build-key",
        &trace_context,
    )
    .expect("job renders");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_TRANSFER_AUTHENTICATION"],
        "workload-identity"
    );
    assert_eq!(job["Job"]["ID"], first);
    assert_eq!(job["Job"]["Namespace"], "telchar");
    assert_eq!(job["Job"]["NodePool"], "arm-builders");
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
    assert_eq!(job["Job"]["Priority"], 50);
    assert_eq!(job["Job"]["Meta"]["telchar_resource_profile"], "default");

    let ci_job =
        telchar::nomad::backend::render_job_for_features(backend, b"ci-build-key", &["telchar-ci"])
            .expect("CI job renders");
    assert_eq!(
        ci_job["Job"]["TaskGroups"][0]["Tasks"][1]["Resources"],
        serde_json::json!({"CPU": 4000, "MemoryMB": 8192, "DiskMB": 32768})
    );
    assert_eq!(ci_job["Job"]["Priority"], 60);
    assert_eq!(ci_job["Job"]["Meta"]["telchar_resource_profile"], "ci");
    assert_eq!(
        ci_job["Job"]["Constraints"][2],
        serde_json::json!({
            "LTarget": "${node.class}",
            "Operand": "=",
            "RTarget": "overflow-aws",
        })
    );

    assert!(
        telchar::nomad::backend::render_job_for_features(
            backend,
            b"ambiguous-build-key",
            &["telchar-ci", "telchar-memory"],
        )
        .is_err(),
        "multiple resource-profile features must be rejected"
    );
    assert_eq!(job["Job"]["Meta"]["telchar_backend"], "nomad-arm");
    assert_eq!(job["Job"]["Meta"]["telchar_system"], "aarch64-linux");
    assert_eq!(
        job["Job"]["Constraints"],
        serde_json::json!([
            {
                "LTarget": "${attr.cpu.arch}",
                "Operand": "=",
                "RTarget": "amd64",
            },
            {
                "LTarget": "${node.class}",
                "Operand": "=",
                "RTarget": "general",
            },
        ])
    );
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
    assert_eq!(job["Job"]["TaskGroups"][0]["Networks"][0]["Mode"], "bridge");
    let callback_service = &job["Job"]["TaskGroups"][0]["Services"][0];
    assert_eq!(callback_service["Name"], "telchar-build-callback");
    let callback_sidecar = &callback_service["Connect"]["SidecarService"];
    let callback_upstream = &callback_sidecar["Proxy"]["Upstreams"][0];
    assert_eq!(callback_upstream["DestinationName"], "telchar-callback");
    assert_eq!(callback_upstream["LocalBindPort"], 17443);
    assert_eq!(
        callback_service["Connect"]["SidecarTask"]["Config"]["image"],
        "registry.example/envoy:v1.38.4"
    );
    assert!(callback_sidecar["SidecarTask"].is_null());
    assert_eq!(job["Job"]["TaskGroups"][0]["RestartPolicy"]["Attempts"], 0);
    assert_eq!(job["Job"]["TaskGroups"][0]["RestartPolicy"]["Mode"], "fail");
    assert_eq!(
        job["Job"]["TaskGroups"][0]["ReschedulePolicy"]["Attempts"],
        0
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["ReschedulePolicy"]["Unlimited"],
        false
    );
    let environment = &job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"];
    assert_eq!(
        environment["TRACEPARENT"],
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
    );
    assert_eq!(environment["TRACESTATE"], "vendor=value");
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
        "ws://127.0.0.1:17443/callback"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_NIX_STORE_URI"],
        "unix:///nix/var/nix/daemon-socket/socket"
    );
    assert_eq!(
        job["Job"]["TaskGroups"][0]["Tasks"][1]["Env"]["TELCHAR_TRANSFER_CHUNK_BYTES"],
        "262144"
    );
    assert_eq!(environment["TELCHAR_MAXIMUM_MANIFEST_BYTES"], "1048576");
    assert_eq!(environment["TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS"], "30");
    assert_eq!(
        environment["TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS"],
        "300"
    );
    assert_eq!(
        environment["TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS"],
        "3600"
    );
    let identity = &job["Job"]["TaskGroups"][0]["Tasks"][1]["Identities"][0];
    assert_eq!(identity["Name"], "telchar_transfer");
    assert_eq!(identity["Env"], true);
    assert_eq!(identity["File"], false);
    assert_eq!(identity["Audience"][0], "telchar-transfer");
    assert_eq!(identity["TTL"], 3_600_000_000_000_u64);
    assert!(job["Job"]["TaskGroups"][0]["Tasks"][1]["Identity"].is_null());

    let configured = fs::read_to_string(&config_path).expect("configuration reads");
    for invalid_config in [
        configured.replace("minimum = 40", "minimum = 0"),
        configured.replace(
            "required_feature = \"telchar-memory\"",
            "required_feature = \"telchar-ci\"",
        ),
        configured.replace(
            "required_feature = \"telchar-memory\"",
            "required_feature = \"unadvertised\"",
        ),
    ] {
        fs::write(&config_path, invalid_config).expect("invalid configuration writes");
        assert!(
            ServiceConfig::load().is_err(),
            "invalid resource profile configuration must fail"
        );
    }

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
[backends.nomad.nomad-hmac]
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "http://nomad.example:4646"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "telchar-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "ws://telchar.example:7443"

[backends.nomad.nomad-hmac.transfer_authentication]
mode = "hmac"
key_id = "primary"
secret_file = {secret_path:?}

[backends.nomad.nomad-hmac.store]
mode = "daemon"
uri = "unix:///nix/var/nix/daemon-socket/socket"

[backends.nomad.nomad-hmac.transfer_limits]
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

[backends.nomad.nomad-hmac.resources]
cpu_mhz = 1000
memory_mb = 512
disk_mb = 1024

[backends.nomad.nomad-hmac.driver_config]
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
    assert!(
        claims["request_key"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
    );
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
