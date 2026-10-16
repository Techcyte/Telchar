use telchar::service::config::MicrovmBackendConfig;

const CONFIG: &str = r#"
name = "arm"
system = "aarch64-linux"
account_id = "116392764028"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:116392764028:microvm-image:builder"
image_version = "1"
maximum_concurrent_builds = 2
maximum_duration_seconds = 600
execution_timeout_seconds = 540
admission_timeout_seconds = 30
provider_timeout_seconds = 10
cleanup_interval_seconds = 5
callback_url = "wss://gateway.example/microvm"
callback_address = "192.0.2.1:443"
staging_directory = "/var/lib/telchar/microvm"
maximum_paths = 65536
maximum_nar_bytes = 1073741824
maximum_total_bytes = 2147483648
metadata_bytes = 1048576
chunk_bytes = 262144
"#;

fn service_configuration(text: &str) -> std::io::Result<telchar::service::config::ServiceConfig> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.toml");
    std::fs::write(&path, text).unwrap();
    telchar::service::config::ServiceConfig::load_from_default(&path)
}

#[test]
fn service_configuration_accepts_bounded_microvm_fleet() {
    let text =
        format!("[backends]\nmicrovm_bind = \"127.0.0.1:7444\"\n[[backends.microvm]]\n{CONFIG}");
    let config = service_configuration(&text).expect("MicroVM fleet configuration");
    let targets = config.backend_targets().collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name(), "arm");
    assert_eq!(targets[0].kind(), telchar::backend::BackendKind::Microvm);
    assert_eq!(targets[0].system(), "aarch64-linux");
    assert!(config.system_features().contains_key("aarch64-linux"));
    assert!(config.validate_static_ssh_reload(&config).is_ok());
    let changed =
        service_configuration(&text.replace("image_version = \"1\"", "image_version = \"2\""))
            .unwrap();
    assert!(config.validate_static_ssh_reload(&changed).is_err());
    for invalid in [
        text.replace("microvm_bind = \"127.0.0.1:7444\"", ""),
        text.replace("127.0.0.1:7444", "127.0.0.1:0"),
        text.replace("aarch64-linux", "x86_64-linux"),
        format!("{text}\n[[backends.microvm]]\n{CONFIG}"),
        format!(
            "{text}\n[backends.local]\nname = \"arm\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1"
        ),
        "[backends]\nmicrovm_bind = \"127.0.0.1:7444\"".to_owned(),
    ] {
        assert!(service_configuration(&invalid).is_err());
    }
}

#[test]
fn deployment_configuration_preserves_execution_authority() {
    let config = MicrovmBackendConfig::parse(CONFIG).unwrap();
    assert_eq!(config.name(), "arm");
    assert_eq!(config.maximum_concurrent_builds(), 2);
    assert_eq!(config.execution_timeout().as_secs(), 540);
    assert_eq!(config.transfer_limits().chunk_bytes, 262144);
    assert_eq!(
        config.fingerprint(),
        MicrovmBackendConfig::parse(&format!("# comment\n{CONFIG}"))
            .unwrap()
            .fingerprint()
    );
    assert_ne!(
        config.fingerprint(),
        MicrovmBackendConfig::parse(
            &CONFIG.replace("image_version = \"1\"", "image_version = \"2\"")
        )
        .unwrap()
        .fingerprint()
    );
}

#[test]
fn rejects_untrusted_or_unbounded_deployment_configuration() {
    for (from, to) in [
        ("aarch64-linux", "x86_64-linux"),
        (
            "account_id = \"116392764028\"",
            "account_id = \"999999999999\"",
        ),
        ("region = \"us-west-2\"", "region = \"us-east-1\""),
        ("image_version = \"1\"", "image_version = \"latest\""),
        (
            "maximum_concurrent_builds = 2",
            "maximum_concurrent_builds = 0",
        ),
        (
            "execution_timeout_seconds = 540",
            "execution_timeout_seconds = 601",
        ),
        (
            "admission_timeout_seconds = 30",
            "admission_timeout_seconds = 540",
        ),
        (
            "provider_timeout_seconds = 10",
            "provider_timeout_seconds = 61",
        ),
        (
            "cleanup_interval_seconds = 5",
            "cleanup_interval_seconds = 0",
        ),
        (
            "wss://gateway.example/microvm",
            "ws://gateway.example/microvm",
        ),
        (
            "wss://gateway.example/microvm",
            "wss://user:secret@gateway.example/microvm",
        ),
        ("192.0.2.1:443", "0.0.0.0:443"),
        ("/var/lib/telchar/microvm", "relative/staging"),
        ("maximum_paths = 65536", "maximum_paths = 0"),
        (
            "maximum_total_bytes = 2147483648",
            "maximum_total_bytes = 1",
        ),
        ("chunk_bytes = 262144", "chunk_bytes = 262145"),
    ] {
        assert!(
            MicrovmBackendConfig::parse(&CONFIG.replace(from, to)).is_err(),
            "accepted {to}"
        );
    }
    assert!(
        MicrovmBackendConfig::parse(&format!("{CONFIG}\nendpoint_url = \"http://localhost\"\n"))
            .is_err()
    );
}
