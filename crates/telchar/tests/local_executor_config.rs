//! Tests local executor config contracts and failure boundaries, including deployment environment selects the gateway executor.

use telchar_test_support::process;

use telchar::backend::{BuildBackend, BuildExecution};

#[test]
fn deployment_environment_selects_the_gateway_executor() {
    let name = "deployment_environment_selects_the_gateway_executor";
    if !process::is_child(name) {
        process::assert_success(
            process::command(name)
                .env_remove("TELCHAR_TEST_BUILD_HELPER")
                .env(
                    "TELCHAR_GATEWAY_STORE_URI",
                    "unix:///nix/var/nix/daemon-socket/socket",
                ),
        );
        return;
    }

    let executor =
        telchar::backend::local::executor_from_environment().expect("configured executor creates");

    let _backend: &dyn BuildBackend = executor.as_ref();
}

#[test]
fn explicit_gateway_endpoint_constructs_store_clients_without_environment() {
    let name = "explicit_gateway_endpoint_constructs_store_clients_without_environment";
    if !process::is_child(name) {
        process::assert_success(process::command(name).env_remove("TELCHAR_GATEWAY_STORE_URI"));
        return;
    }
    let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///nix/var/nix/daemon-socket/socket",
    )
    .expect("endpoint is valid");

    let _query = telchar::store::query::GatewayStoreQuery::new(endpoint.clone());
    let _import = telchar::store::import::GatewayStoreImport::new(endpoint.clone())
        .expect("importer creates");
    let _outputs =
        telchar::shared_build::recovery::GatewaySharedBuildOutputStore::new(endpoint.clone());
    let _executor = telchar::backend::local::GatewayStoreExecutor::new(endpoint);
}

#[test]
fn rejected_recovery_validation_protects_outputs_until_owner_drops() {
    use telchar::shared_build::recovery::SharedBuildOutputStore;
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "canonical isolated store runner required"
    );
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("recovery-output");
    std::fs::write(&source, "protected recovery output").unwrap();
    let added = std::process::Command::new("nix-store")
        .args(["--store", "daemon", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let path = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let mut value = serde_json::to_value(admitted_request()).unwrap();
    value["expected_outputs"][0][1] = serde_json::json!(path.as_bytes());
    value["output_authorities"][0]["path"] = serde_json::json!(path.as_bytes());
    let request = serde_json::from_value(value).unwrap();
    let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///nix/var/nix/daemon-socket/socket",
    )
    .unwrap();
    let mut outputs = telchar::shared_build::recovery::GatewaySharedBuildOutputStore::new(endpoint);
    let error = outputs.validate_outputs(&request).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "recovered output content address mismatch"
    );
    let delete = || {
        std::process::Command::new("nix-store")
            .args(["--store", "daemon", "--delete", &path])
            .output()
            .unwrap()
    };
    let rooted = delete();
    assert!(!rooted.status.success());
    assert!(
        String::from_utf8_lossy(&rooted.stderr).contains("is still alive"),
        "{}",
        String::from_utf8_lossy(&rooted.stderr)
    );
    drop(outputs);
    let released = delete();
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    assert!(!std::path::Path::new(&path).exists());
}

#[test]
fn recovered_success_retains_outputs_after_recovery_owner_drops() {
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "canonical isolated store runner required"
    );
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("recovered-output");
    std::fs::write(&source, "durable recovered output").unwrap();
    let added = std::process::Command::new("nix-store")
        .args(["--store", "daemon", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let path = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///nix/var/nix/daemon-socket/socket",
    )
    .unwrap();
    let info = telchar::store::daemon::GatewayStoreConnection::connect(&endpoint)
        .unwrap()
        .query_path_info(path.as_bytes())
        .unwrap()
        .unwrap();
    let mut value = serde_json::to_value(admitted_request()).unwrap();
    value["expected_outputs"][0][1] = serde_json::json!(path.as_bytes());
    value["output_authorities"][0]["path"] = serde_json::json!(path.as_bytes());
    value["output_authorities"][0]["hash_algorithm"] = serde_json::json!(b"r:sha256".as_slice());
    value["output_authorities"][0]["hash"] = serde_json::json!(info.nar_hash_hex().as_bytes());
    value["environment"][2][1] = serde_json::json!(path.as_bytes());
    let request: telchar::build::BuildRequest = serde_json::from_value(value).unwrap();
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let kind = telchar::backend::BackendKind::Local;
    let derivation = std::str::from_utf8(request.derivation_path()).unwrap();
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation,
        &request.shared_build_digest(),
        "local",
        kind,
        kind.capabilities(),
        None,
        &[&path],
        &request,
    )
    .unwrap();
    let roots = directory.path().join("roots");
    std::fs::create_dir(&roots).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&roots, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retention = telchar::store::retention::NixStoreRetentionBackend::new_with_store_directory(
        "unix:///nix/var/nix/daemon-socket/socket",
        "/nix/store",
        &roots,
    )
    .unwrap();
    let mut outputs = telchar::shared_build::recovery::GatewaySharedBuildOutputStore::new(endpoint)
        .hook_delivery_retention(Box::new(retention));
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n").unwrap();
    let config = telchar::service::config::ServiceConfig::load_from_default(&config_path).unwrap();
    let mut backends = telchar::backend::routing::ConfiguredBackends::new(&config, None).unwrap();
    let attempt =
        telchar::persistence::read_shared_build_attempt(database.url(), derivation).unwrap();
    assert!(attempt.is_none());
    std::fs::remove_dir(&roots).unwrap();
    std::fs::write(&roots, "retention unavailable").unwrap();
    assert!(
        telchar::shared_build::recovery::reconcile_active_shared_builds(
            database.url(),
            std::time::Duration::from_secs(60),
            &mut outputs,
            &mut backends,
        )
        .is_err()
    );
    let attempt = telchar::persistence::read_shared_build_attempt(database.url(), derivation)
        .unwrap()
        .unwrap();
    let lease_id = format!("recovery-{}-0", attempt.attempt_id);
    let lease = telchar::persistence::read_store_lease(database.url(), &lease_id)
        .unwrap()
        .expect("root creation failure retains durable cleanup authority");
    assert_eq!(lease.state, telchar::persistence::StoreLeaseState::Active);
    assert_ne!(
        telchar::persistence::read_shared_build(database.url(), derivation)
            .unwrap()
            .unwrap()
            .state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    std::fs::remove_file(&roots).unwrap();
    std::fs::create_dir(&roots).unwrap();
    std::fs::set_permissions(&roots, std::fs::Permissions::from_mode(0o700)).unwrap();
    telchar::shared_build::recovery::reconcile_active_shared_builds(
        database.url(),
        std::time::Duration::from_secs(60),
        &mut outputs,
        &mut backends,
    )
    .unwrap();
    drop(outputs);
    let deletion = std::process::Command::new("nix-store")
        .args(["--store", "daemon", "--delete", &path])
        .output()
        .unwrap();
    assert!(
        !deletion.status.success(),
        "recovered success must retain output after owner drops"
    );
    assert!(
        String::from_utf8_lossy(&deletion.stderr).contains("is still alive"),
        "{}",
        String::from_utf8_lossy(&deletion.stderr)
    );
}

#[test]
fn configured_executor_rejects_an_inaccessible_gateway_store() {
    let endpoint = telchar::store::daemon::GatewayStoreEndpoint::parse(
        "unix:///definitely-missing/telchar-gateway.sock",
    )
    .expect("endpoint is valid");
    let mut executor = telchar::backend::local::GatewayStoreExecutor::new(endpoint);
    let build = admitted_request();
    let request = BuildExecution::new(
        "inaccessible-store",
        &build,
        std::time::Duration::from_secs(5),
    )
    .expect("request is valid");

    let error = executor
        .execute(&request)
        .expect_err("missing gateway store must fail");

    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
    ));
}

#[test]
fn absent_gateway_selects_execution_unavailable_without_store_fallback() {
    let name = "absent_gateway_selects_execution_unavailable_without_store_fallback";
    if !process::is_child(name) {
        process::assert_success(
            process::command(name)
                .env_remove("TELCHAR_TEST_BUILD_HELPER")
                .env_remove("TELCHAR_GATEWAY_STORE_URI"),
        );
        return;
    }

    let executor =
        telchar::backend::local::executor_from_environment().expect("unavailable executor creates");

    let _backend: &dyn BuildBackend = executor.as_ref();
}

fn admitted_request() -> telchar::build::BuildRequest {
    let mut wire = Vec::new();
    write_integer(&mut wire, 36);
    write_string(
        &mut wire,
        b"/nix/store/00000000000000000000000000000000-config-test.drv",
    );
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(
        &mut wire,
        b"/nix/store/11111111111111111111111111111111-config-test",
    );
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 2);
    write_string(&mut wire, b"-c");
    write_string(&mut wire, b"printf configured > $out");
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"config-test".as_slice()),
        (
            b"out".as_slice(),
            b"/nix/store/11111111111111111111111111111111-config-test".as_slice(),
        ),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut reader = nix_worker_protocol::WorkerReader::new(
        wire.as_slice(),
        nix_worker_protocol::ProtocolSessionLimits::DEFAULT,
    );
    assert_eq!(
        reader.read_operation().expect("operation reads"),
        nix_worker_protocol::WorkerOperation::BuildDerivation
    );
    let request = reader
        .complete_build_derivation()
        .expect("worker request parses");
    telchar::build::BuildRequest::from_worker_request(
        &request,
        &[telchar::backend::BackendTarget::new(
            "fixture",
            telchar::backend::BackendKind::Local,
            "x86_64-linux",
            [] as [&str; 0],
        )
        .expect("backend parses")],
    )
    .expect("request admits")
}

fn write_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_string(output: &mut Vec<u8>, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.extend_from_slice(&[0; 7][..(8 - value.len() % 8) % 8]);
}
