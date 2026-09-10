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
    let shared_builds = telchar::shared_build::SharedBuildRegistry::new();
    let leader = match shared_builds
        .acquire(std::str::from_utf8(shared_build_key).expect("shared build key is UTF-8"))
    {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let error = client
        .execute(
            "postgresql://unused",
            &execution,
            shared_build_key,
            &mut |_| Ok(()),
            &shared_builds,
            1024,
            &mut || Ok(true),
        )
        .expect_err("cancelled execution rejects");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    drop(leader);
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
        thread::sleep(Duration::from_millis(10));
        write_json_response(&mut submit_request, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut stop_request, _) = listener.accept().expect("stop request accepts");
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
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_millis(1))
        .expect("execution creates");
    let client = NomadClient::new(backend).expect("Nomad client constructs");
    let shared_builds = telchar::shared_build::SharedBuildRegistry::new();
    let leader = match shared_builds.acquire(&shared_build_key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let error = client
        .execute(
            database.url(),
            &execution,
            shared_build_key.as_bytes(),
            &mut |_| Ok(()),
            &shared_builds,
            1024,
            &mut || Ok(false),
        )
        .expect_err("timed out execution rejects");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    drop(leader);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_does_not_retry_callback_recorded_build_failure() {
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
    let key = admitted.shared_build_key();
    let job_id = deterministic_job_name(&backend, key.as_bytes());
    let served_job = job_id.clone();
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut submit, _) = listener.accept().expect("submit accepts");
        assert!(read_http_request_with_body(&mut submit).contains(&served_job));
        write_json_response(&mut submit, 200, r#"{"EvalID":"evaluation-1"}"#);
        let (mut status, _) = listener.accept().expect("status accepts");
        let _ = read_http_request(&mut status);
        write_json_response(
            &mut status,
            200,
            &format!(
                r#"{{"ID":"{served_job}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations, _) = listener.accept().expect("allocations accepts");
        let _ = read_http_request(&mut allocations);
        write_json_response(
            &mut allocations,
            200,
            r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#,
        );
        status_tx.send(()).expect("status reports");
        listener
            .set_nonblocking(true)
            .expect("listener becomes nonblocking");
        thread::sleep(Duration::from_millis(250));
        assert!(
            listener.accept().is_err(),
            "deterministic build failure was retried"
        );
    });
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let derivation = std::str::from_utf8(admitted.derivation_path()).expect("derivation is UTF-8");
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation,
        &admitted.shared_build_digest(),
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&job_id),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).expect("output path is UTF-8"))
            .collect::<Vec<_>>(),
        &admitted,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(database.url(), derivation)
        .expect("shared build starts");
    let failure_database = database.url().to_owned();
    let failure_derivation = derivation.to_owned();
    let failure = thread::spawn(move || {
        status_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("status completes");
        telchar::persistence::complete_shared_build_failure(
            &failure_database,
            &failure_derivation,
            "nomad-build-failure",
            &serde_json::json!({"diagnostic": "builder exited 1"}),
            Duration::from_secs(60),
        )
        .expect("build failure records");
    });
    let mut execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    execution
        .set_target_name("nomad-test")
        .expect("target records");
    let builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let leader = match builds.acquire(&key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let mut executor = ConfiguredBackends::new(&config, gateway_store_endpoint())
        .expect("backends configure")
        .executor(
            telchar::persistence::Database::connect(database.url()).expect("database connects"),
            Arc::clone(&builds),
        )
        .expect("executor configures");

    executor
        .execute(&execution)
        .expect_err("deterministic build failure rejects");

    assert_eq!(
        telchar::persistence::read_shared_build_attempt(database.url(), derivation)
            .expect("attempt reads")
            .expect("attempt exists")
            .ordinal,
        1
    );
    drop(leader);
    failure.join().expect("failure joins");
    server.join().expect("server joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_stops_after_nomad_retry_budget_is_exhausted() {
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
    let key = admitted.shared_build_key();
    let first_job = deterministic_job_name(&backend, key.as_bytes());
    let second_job =
        telchar::nomad::backend::deterministic_job_name_for_attempt(&backend, key.as_bytes(), 2)
            .expect("retry identity derives");
    let server = thread::spawn(move || {
        for (job_id, evaluation) in [(first_job, 1), (second_job, 2)] {
            let (mut submit, _) = listener.accept().expect("submit accepts");
            assert!(read_http_request_with_body(&mut submit).contains(&job_id));
            write_json_response(
                &mut submit,
                200,
                &format!(r#"{{"EvalID":"evaluation-{evaluation}"}}"#),
            );
            let (mut status, _) = listener.accept().expect("status accepts");
            assert!(read_http_request(&mut status).starts_with(&format!("GET /v1/job/{job_id}?")));
            write_json_response(&mut status, 404, r#"{}"#);
        }
    });
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let derivation = std::str::from_utf8(admitted.derivation_path()).expect("derivation is UTF-8");
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation,
        &admitted.shared_build_digest(),
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&deterministic_job_name(&backend, key.as_bytes())),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).expect("output path is UTF-8"))
            .collect::<Vec<_>>(),
        &admitted,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(database.url(), derivation)
        .expect("shared build starts");
    let mut execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    execution
        .set_target_name("nomad-test")
        .expect("target records");
    let builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let leader = match builds.acquire(&key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let mut executor = ConfiguredBackends::new(&config, gateway_store_endpoint())
        .expect("backends configure")
        .executor(
            telchar::persistence::Database::connect(database.url()).expect("database connects"),
            Arc::clone(&builds),
        )
        .expect("executor configures");

    executor
        .execute(&execution)
        .expect_err("exhausted retry rejects");

    assert_eq!(
        telchar::persistence::read_shared_build_attempt(database.url(), derivation)
            .expect("attempt reads")
            .expect("attempt exists")
            .ordinal,
        2
    );
    drop(leader);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_retries_missing_nomad_execution_with_distinct_identity() {
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
    let first_job_id = deterministic_job_name(&backend, shared_build_key.as_bytes());
    let second_job_id = telchar::nomad::backend::deterministic_job_name_for_attempt(
        &backend,
        shared_build_key.as_bytes(),
        2,
    )
    .expect("retry identity derives");
    let served_first = first_job_id.clone();
    let served_second = second_job_id.clone();
    let (second_status_tx, second_status_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut first_submit, _) = listener.accept().expect("first submit accepts");
        let request = read_http_request_with_body(&mut first_submit);
        assert!(request.contains(&served_first));
        write_json_response(&mut first_submit, 200, r#"{"EvalID":"evaluation-1"}"#);

        let (mut missing_status, _) = listener.accept().expect("missing status accepts");
        let request = read_http_request(&mut missing_status);
        assert!(request.starts_with(&format!("GET /v1/job/{served_first}?")));
        write_json_response(&mut missing_status, 404, r#"{}"#);

        let (mut second_submit, _) = listener.accept().expect("second submit accepts");
        let request = read_http_request_with_body(&mut second_submit);
        assert!(request.contains(&served_second));
        write_json_response(&mut second_submit, 200, r#"{"EvalID":"evaluation-2"}"#);

        let (mut second_status, _) = listener.accept().expect("second status accepts");
        let _ = read_http_request(&mut second_status);
        write_json_response(
            &mut second_status,
            200,
            &format!(
                r#"{{"ID":"{served_second}","Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations, _) = listener.accept().expect("allocations accepts");
        let _ = read_http_request(&mut allocations);
        write_json_response(
            &mut allocations,
            200,
            r#"[{"ID":"allocation-2","ClientStatus":"complete"}]"#,
        );
        second_status_tx.send(()).expect("second status reports");
    });
    let database = support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let derivation_path =
        std::str::from_utf8(admitted.derivation_path()).expect("derivation path is UTF-8");
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation_path,
        &admitted.shared_build_digest(),
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&first_job_id),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).expect("output path is UTF-8"))
            .collect::<Vec<_>>(),
        &admitted,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(database.url(), derivation_path)
        .expect("shared build starts");
    let mut execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    execution
        .set_target_name("nomad-test")
        .expect("selected target records");
    let live_builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let leader = match live_builds.acquire(&shared_build_key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let completion_database = database.url().to_owned();
    let completion_derivation = derivation_path.to_owned();
    let completion_job = second_job_id.clone();
    let completion = thread::spawn(move || {
        second_status_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("second status completes");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let build = telchar::persistence::read_shared_build(
                &completion_database,
                &completion_derivation,
            )
            .expect("shared build reads")
            .expect("shared build exists");
            if build.backend_execution_id.as_deref() == Some(completion_job.as_str()) {
                telchar::persistence::collect_shared_build(
                    &completion_database,
                    &completion_derivation,
                )
                .expect("shared build collects");
                telchar::persistence::complete_shared_build_success(
                    &completion_database,
                    &completion_derivation,
                    &serde_json::json!({"status": "built"}),
                    Duration::from_secs(60),
                )
                .expect("shared build completes");
                break;
            }
            assert!(Instant::now() < deadline, "retry identity did not rotate");
            thread::sleep(Duration::from_millis(10));
        }
    });
    let mut executor = ConfiguredBackends::new(&config, gateway_store_endpoint())
        .expect("backends configure")
        .executor(
            telchar::persistence::Database::connect(database.url()).expect("database connects"),
            Arc::clone(&live_builds),
        )
        .expect("executor configures");

    let result = executor
        .execute(&execution)
        .expect("retry execution completes");

    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(
        telchar::persistence::read_shared_build_attempt(database.url(), derivation_path)
            .expect("attempt reads")
            .expect("attempt exists")
            .ordinal,
        2
    );
    drop(leader);
    completion.join().expect("completion joins");
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
    let (status_tx, status_rx) = std::sync::mpsc::channel();
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
        status_tx.send(()).expect("status reports");
    });
    let admitted = admitted_request();
    let mut execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    execution
        .set_target_name("nomad-test")
        .expect("selected target records");
    execution.set_trace_context(
        telchar_telemetry::TraceContext::new(
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()),
            None,
        )
        .expect("trace context validates"),
    );
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
    let completion_database = database.url().to_owned();
    let completion_derivation = std::str::from_utf8(request.derivation_path())
        .expect("derivation path is UTF-8")
        .to_owned();
    let completion = thread::spawn(move || {
        status_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("status completes");
        telchar::persistence::collect_shared_build(&completion_database, &completion_derivation)
            .expect("shared build collects");
        telchar::persistence::complete_shared_build_success(
            &completion_database,
            &completion_derivation,
            &serde_json::json!({"status": "built"}),
            Duration::from_secs(60),
        )
        .expect("shared build completes");
    });
    let live_builds = Arc::new(telchar::shared_build::SharedBuildRegistry::new());
    let live_leader = match live_builds.acquire(&request.shared_build_key()) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let mut executor = ConfiguredBackends::new(&config, gateway_store_endpoint())
        .expect("backends configure")
        .executor(
            telchar::persistence::Database::connect(database.url()).expect("database connects"),
            Arc::clone(&live_builds),
        )
        .expect("executor configures");
    let result = executor
        .execute(&execution)
        .expect("Nomad execution completes");
    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
    assert!(
        telchar::persistence::read_shared_build_attempt(
            database.url(),
            std::str::from_utf8(request.derivation_path()).expect("derivation path is UTF-8"),
        )
        .expect("attempt reads")
        .expect("attempt exists")
        .trace_context
        .trace_id()
        .is_some()
    );
    drop(live_leader);
    completion.join().expect("completion joins");
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
