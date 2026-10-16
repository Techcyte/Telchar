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
