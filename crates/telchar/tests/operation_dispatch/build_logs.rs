//! Tests bounded live log ordering before terminal build results.

use super::*;

#[test]
fn build_derivation_streams_helper_logs_before_success_result() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-log-helper-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let request_path = root.join("request-id");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\ncat > '{}'\nprintf 'build-log-line\\n' >&2\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
            request_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    assert_eq!(read_integer(&mut output), nix_worker_protocol::STDERR_NEXT);
    assert_eq!(read_string(&mut output), "build-log-line\n");
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 0, "Built status");
    assert_eq!(read_string(&mut output), "", "empty build error message");
    assert_eq!(read_integer(&mut output), 0, "times built");
    assert_eq!(read_integer(&mut output), 0, "not nondeterministic");
    assert_eq!(read_integer(&mut output), 0, "start time");
    assert_eq!(read_integer(&mut output), 0, "stop time");
    assert_eq!(read_integer(&mut output), 0, "no user CPU duration");
    assert_eq!(read_integer(&mut output), 0, "no system CPU duration");
    assert_eq!(read_integer(&mut output), 0, "no CA realisations");
    drop(input);
    drop(output);

    let status = child.wait().expect("Telchar exits");
    let request_id = fs::read_to_string(&request_path).expect("helper records request ID");
    let helper_request: serde_json::Value =
        serde_json::from_str(&request_id).expect("helper request is JSON");
    let request_id = helper_request["request_id"]
        .as_str()
        .expect("helper request ID is a string");
    assert!(request_id.starts_with("request-"), "{request_id}");
    assert!(request_id.len() <= telchar::service::ipc::MAX_IPC_COMPONENT_BYTES);
    let persisted = telchar::persistence::read_build_request(fixture.database.url(), request_id)
        .expect("build request reads")
        .expect("build request exists before helper result");
    assert_eq!(persisted.request_id, request_id);
    assert_eq!(
        persisted.derivation_path,
        "/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
    );
    assert_eq!(persisted.system, "x86_64-linux");
    let stderr = fixture.finish();
    assert!(status.success(), "Telchar failed with {status}: {stderr}");
    assert!(
        stderr.contains("worker.build_derivation.completed"),
        "{stderr}"
    );
    assert!(!stderr.contains("build-log-line"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}
