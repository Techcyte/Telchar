//! Tests Nomad identity.

use super::*;

#[test]
fn monitors_only_the_exact_backend_bound_job() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let job_id = deterministic_job_name(&config, b"shared-build-key");
    let expected_job_id = job_id.clone();
    let server = thread::spawn(move || {
        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let request = read_http_request(&mut job_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{expected_job_id}?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );

        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let request = read_http_request(&mut allocations_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{expected_job_id}/allocations?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    assert_eq!(
        client.status(&job_id).expect("Nomad job status reads"),
        NomadExecutionState::Monitoring
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn verifies_exact_callback_allocation_identity() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let client = NomadClient::new(config).expect("Nomad client constructs");
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("allocation request accepts");
        assert!(read_http_request(&mut request)
            .starts_with("GET /v1/allocation/allocation-1?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(
            &mut request,
            200,
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"running","TaskStates":{"prestart":{"State":"dead"},"build":{"State":"running"}}}"#,
        );
    });

    client
        .verify_allocation("allocation-1", "job-1", "build")
        .expect("exact allocation verifies");
    server.join().expect("server joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn verifies_exact_callback_allocation_during_task_startup() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let config = load_nomad_config(&root, &endpoint, None);
    let client = NomadClient::new(config).expect("Nomad client constructs");
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().expect("allocation request accepts");
        read_http_request(&mut request);
        write_json_response(
            &mut request,
            200,
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"running","TaskStates":{"build":{"State":"starting"}}}"#,
        );
    });

    client
        .verify_allocation("allocation-1", "job-1", "build")
        .expect("starting allocation verifies");
    server.join().expect("server joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn rejects_foreign_or_terminal_callback_allocation_identity() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    for (name, body) in [
        (
            "foreign-job",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"foreign","TaskGroup":"build","ClientStatus":"running","TaskStates":{"build":{"State":"running"}}}"#,
        ),
        (
            "foreign-task",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"running","TaskStates":{"foreign":{"State":"running"}}}"#,
        ),
        (
            "terminal",
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","ClientStatus":"complete","TaskStates":{"build":{"State":"dead"}}}"#,
        ),
    ] {
        let root = fixture_root();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let config = load_nomad_config(&root, &endpoint, None);
        let client = NomadClient::new(config).expect("Nomad client constructs");
        let server = thread::spawn(move || {
            let (mut request, _) = listener.accept().expect("allocation request accepts");
            read_http_request(&mut request);
            write_json_response(&mut request, 200, body);
        });
        assert!(
            client
                .verify_allocation("allocation-1", "job-1", "build")
                .is_err(),
            "{name} allocation must reject"
        );
        server.join().expect("server joins");
        fs::remove_dir_all(root).expect("fixture removes");
    }
}

#[test]
fn configured_backend_exposes_deterministic_execution_identity_before_submission() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let config = load_service_config(&root, "http://127.0.0.1:4646", None);
    let backend = config.nomad_backends()[0].clone();
    let configured =
        ConfiguredBackends::new(&config, gateway_store_endpoint()).expect("backends configure");
    let executor = configured
        .executor("postgresql://fixture")
        .expect("executor configures");
    assert_eq!(
        executor
            .execution_id(backend.target(), b"shared-build-key")
            .expect("execution identity derives")
            .as_deref(),
        Some(deterministic_job_name(&backend, b"shared-build-key").as_str())
    );
    fs::remove_dir_all(root).expect("fixture removes");
}
