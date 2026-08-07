use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex;

static ENVIRONMENT: Mutex<()> = Mutex::new(());

#[test]
fn deployment_environment_selects_the_pinned_local_executor() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved_helper = std::env::var_os("TELCHAR_NIX_STORE_BUILD");
    let saved_store = std::env::var_os("TELCHAR_GATEWAY_STORE_URI");
    unsafe {
        std::env::set_var("TELCHAR_NIX_STORE_BUILD", "/absolute/nix-store-build");
        std::env::set_var(
            "TELCHAR_GATEWAY_STORE_URI",
            "unix:///nix/var/nix/daemon-socket/socket",
        );
    }

    let executor =
        telchar::local_executor::executor_from_environment().expect("configured executor creates");

    assert_eq!(
        executor.helper(),
        Some(Path::new("/absolute/nix-store-build"))
    );
    assert_eq!(
        executor.store_uri(),
        Some("unix:///nix/var/nix/daemon-socket/socket")
    );
    restore("TELCHAR_NIX_STORE_BUILD", saved_helper);
    restore("TELCHAR_GATEWAY_STORE_URI", saved_store);
}

#[test]
fn configured_executor_rejects_an_inaccessible_gateway_store() {
    let helper =
        std::env::var_os("TELCHAR_NIX_STORE_BUILD").expect("dev shell supplies flake-built helper");
    let mut executor = telchar::local_executor::NixStoreExecutor::new(
        helper,
        "unix:///definitely-missing/telchar-gateway.sock",
    )
    .expect("executor config is valid");
    let build = admitted_request();
    let request = telchar::local_executor::LocalExecutionRequest::new(
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
fn absent_helper_selects_execution_unavailable_without_store_fallback() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved_helper = std::env::var_os("TELCHAR_NIX_STORE_BUILD");
    let saved_store = std::env::var_os("TELCHAR_GATEWAY_STORE_URI");
    unsafe {
        std::env::remove_var("TELCHAR_NIX_STORE_BUILD");
        std::env::remove_var("TELCHAR_GATEWAY_STORE_URI");
    }

    let executor =
        telchar::local_executor::executor_from_environment().expect("unavailable executor creates");

    assert_eq!(executor.helper(), None);
    assert_eq!(executor.store_uri(), None);
    restore("TELCHAR_NIX_STORE_BUILD", saved_helper);
    restore("TELCHAR_GATEWAY_STORE_URI", saved_store);
}

fn admitted_request() -> telchar::build_request::BuildRequest {
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
    telchar::build_request::BuildRequest::from_worker_request(
        &request,
        &telchar::deployment::DeploymentConfig::parse("x86_64-linux", "")
            .expect("deployment parses"),
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

fn restore(name: &str, value: Option<OsString>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}
