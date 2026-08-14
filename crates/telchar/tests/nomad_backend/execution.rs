//! Tests Nomad execution.

use super::*;

#[test]
fn cancellation_stops_only_the_exact_submitted_nomad_job() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let backend = config.nomad_backends()[0].clone();
    let shared_build_key = b"cancelled-shared-build";
    let expected_job_id = deterministic_job_name(&backend, shared_build_key);
    let server = thread::spawn(move || {
        let (mut submit_request, _) = listener.accept().expect("submit request accepts");
        let request = read_http_request_with_body(&mut submit_request);
        assert!(request.starts_with("POST /v1/jobs?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(&mut submit_request, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut stop_request, _) = listener.accept().expect("stop request accepts");
        let request = read_http_request(&mut stop_request);
        assert!(request.starts_with(&format!(
            "DELETE /v1/job/{expected_job_id}?namespace=telchar&purge=true HTTP/1.1\r\n"
        )));
        write_json_response(&mut stop_request, 200, r#"{"EvalID":"evaluation-2"}"#);
    });
    let admitted = admitted_request();
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    let client = NomadClient::new(backend).expect("Nomad client constructs");
    let error = client
        .execute(
            "postgresql://unused",
            &execution,
            shared_build_key,
            &mut || Ok(true),
        )
        .expect_err("cancelled execution rejects");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn timeout_stops_only_the_exact_submitted_nomad_job() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let backend = config.nomad_backends()[0].clone();
    let admitted = admitted_request();
    let shared_build_key = admitted.shared_build_key();
    let expected_job_id = deterministic_job_name(&backend, shared_build_key.as_bytes());
    let served_job_id = expected_job_id.clone();
    let server = thread::spawn(move || {
        let (mut submit_request, _) = listener.accept().expect("submit request accepts");
        let request = read_http_request_with_body(&mut submit_request);
        assert!(request.starts_with("POST /v1/jobs?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(&mut submit_request, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let request = read_http_request(&mut job_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{served_job_id}?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{served_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );

        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let request = read_http_request(&mut allocations_request);
        assert!(request.starts_with(&format!(
            "GET /v1/job/{served_job_id}/allocations?namespace=telchar HTTP/1.1\r\n"
        )));
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );

        listener
            .set_nonblocking(true)
            .expect("listener becomes nonblocking");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stop_request = loop {
            match listener.accept() {
                Ok((request, _)) => break request,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "timeout did not stop Nomad job");
                    thread::yield_now();
                }
                Err(error) => panic!("stop request failed: {error}"),
            }
        };
        let request = read_http_request(&mut stop_request);
        assert!(request.starts_with(&format!(
            "DELETE /v1/job/{served_job_id}?namespace=telchar&purge=true HTTP/1.1\r\n"
        )));
        write_json_response(&mut stop_request, 200, r#"{"EvalID":"evaluation-2"}"#);
    });
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let digest = admitted.shared_build_digest();
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        std::str::from_utf8(admitted.derivation_path()).expect("derivation path is UTF-8"),
        &digest,
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&expected_job_id),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).expect("output path is UTF-8"))
            .collect::<Vec<_>>(),
        &admitted,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(
        database.url(),
        std::str::from_utf8(admitted.derivation_path()).expect("derivation path is UTF-8"),
    )
    .expect("shared build starts");
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_nanos(1))
        .expect("execution creates");
    let client = NomadClient::new(backend).expect("Nomad client constructs");
    let error = client
        .execute(
            database.url(),
            &execution,
            shared_build_key.as_bytes(),
            &mut || Ok(false),
        )
        .expect_err("timed out execution rejects");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_submits_and_monitors_nomad_execution() {
    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let server = thread::spawn(move || {
        let (mut submit_request, _) = listener.accept().expect("submit request accepts");
        let request = read_http_request_with_body(&mut submit_request);
        assert!(request.starts_with("POST /v1/jobs?namespace=telchar HTTP/1.1\r\n"));
        write_json_response(&mut submit_request, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let request = read_http_request(&mut job_request);
        let job_id = request
            .strip_prefix("GET /v1/job/")
            .and_then(|request| request.split('?').next())
            .expect("job identity reads");
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let _ = read_http_request(&mut allocations_request);
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"complete"}]"#,
        );
    });
    let admitted = admitted_request();
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let request = &admitted;
    let digest = request.shared_build_digest();
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        std::str::from_utf8(request.derivation_path()).expect("derivation path is UTF-8"),
        &digest,
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&deterministic_job_name(
            &config.nomad_backends()[0],
            request.shared_build_key().as_bytes(),
        )),
        &request
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).expect("output path is UTF-8"))
            .collect::<Vec<_>>(),
        request,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(
        database.url(),
        std::str::from_utf8(request.derivation_path()).expect("derivation path is UTF-8"),
    )
    .expect("shared build starts");
    telchar::persistence::collect_shared_build(
        database.url(),
        std::str::from_utf8(request.derivation_path()).expect("derivation path is UTF-8"),
    )
    .expect("shared build collects");
    telchar::persistence::complete_shared_build_success(
        database.url(),
        std::str::from_utf8(request.derivation_path()).expect("derivation path is UTF-8"),
        &serde_json::json!({"status": "built"}),
        Duration::from_secs(60),
    )
    .expect("shared build completes");
    let mut executor = ConfiguredBackends::new(&config, gateway_store_endpoint())
        .expect("backends configure")
        .executor(database.url())
        .expect("executor configures");
    let result = executor
        .execute(&execution)
        .expect("Nomad execution completes");
    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_adopts_exact_nomad_execution() {
    use telchar::shared_build::recovery::{AdoptedExecution, RecoveryBackend};

    let _guard = CONFIGURATION_TESTS
        .lock()
        .expect("configuration lock holds");
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    let config = load_service_config(&root, &endpoint, None);
    let backend = config.nomad_backends()[0].clone();
    let job_id = deterministic_job_name(&backend, b"shared-build-key");
    let expected_job_id = job_id.clone();
    let server = thread::spawn(move || {
        let (mut job_request, _) = listener.accept().expect("job request accepts");
        let _ = read_http_request(&mut job_request);
        write_json_response(
            &mut job_request,
            200,
            &format!(
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations_request, _) = listener.accept().expect("allocations request accepts");
        let _ = read_http_request(&mut allocations_request);
        write_json_response(
            &mut allocations_request,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );
    });
    let mut configured =
        ConfiguredBackends::new(&config, gateway_store_endpoint()).expect("backends configure");
    let build = telchar::persistence::SharedBuild {
        derivation_path: "/nix/store/00000000000000000000000000000000-build.drv".to_owned(),
        request_digest: [7; 32],
        state: telchar::persistence::SharedBuildState::Running,
        backend_name: "nomad-test".to_owned(),
        backend_kind: BackendKind::Nomad,
        capabilities: BackendKind::Nomad.capabilities(),
        backend_execution_id: Some(job_id),
        expected_outputs: vec!["/nix/store/11111111111111111111111111111111-output".to_owned()],
        build_request: None,
        result_metadata: None,
        failure_classification: None,
        created_at: std::time::SystemTime::now(),
        started_at: Some(std::time::SystemTime::now()),
        collecting_at: None,
        completed_at: None,
        expires_at: None,
    };
    assert_eq!(
        configured.adopt(&build).expect("execution adopts"),
        AdoptedExecution::Monitoring
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}
