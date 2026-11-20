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
