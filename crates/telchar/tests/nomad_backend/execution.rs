//! Tests Nomad execution.

use super::*;

#[test]
fn cancellation_stops_only_the_exact_submitted_nomad_job() {
    cancelled_execution(false);
}

#[test]
fn stalled_cancellation_retains_exact_execution_liability() {
    cancelled_execution(true);
}

fn cancelled_execution(stalled: bool) {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    load_service_config(&root, &endpoint, None);
    let path = root.join("client.toml");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        source.replace(
            "runtime_limit_seconds = 60",
            "runtime_limit_seconds = 60\ncleanup_timeout_seconds = 1",
        ),
    )
    .unwrap();
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let backend = config.nomad_backends()[0].clone();
    assert_eq!(backend.cleanup_timeout(), Duration::from_secs(1));
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
            "DELETE /v1/job/{expected_job_id}?namespace=telchar&purge=false HTTP/1.1\r\n"
        )));
        if stalled {
            thread::sleep(Duration::from_secs(2));
        } else {
            write_json_response(&mut stop_request, 200, r#"{"EvalID":"evaluation-2"}"#);
        }
    });
    let admitted = admitted_request();
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(5))
        .expect("execution creates");
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let derivation = std::str::from_utf8(admitted.derivation_path()).unwrap();
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation,
        &admitted.shared_build_digest(),
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&deterministic_job_name(&backend, shared_build_key)),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).unwrap())
            .collect::<Vec<_>>(),
        &admitted,
    )
    .expect("shared build claims");
    telchar::persistence::start_shared_build(database.url(), derivation)
        .expect("shared build starts");
    let client = NomadClient::new(backend).expect("Nomad client constructs");
    let shared_builds = telchar::shared_build::SharedBuildRegistry::new();
    let leader = match shared_builds
        .acquire(std::str::from_utf8(shared_build_key).expect("shared build key is UTF-8"))
    {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        telchar::shared_build::SharedBuildAccess::Follower(_) => panic!("build leads"),
    };
    let started = std::time::Instant::now();
    let error = client
        .execute(
            database.url(),
            &execution,
            shared_build_key,
            &mut |_| Ok(()),
            &shared_builds,
            1024,
            &mut || Ok(true),
        )
        .expect_err("cancelled execution rejects");
    if stalled {
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(1700),
            "cancellation exceeded cleanup budget: {elapsed:?}"
        );
        assert_eq!(
            telchar::persistence::read_nomad_execution_obligations(database.url())
                .unwrap()
                .len(),
            1
        );
    } else {
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }
    drop(leader);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn stalled_submission_obeys_execution_deadline_and_retains_obligation() {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let config = load_service_config(&root, &endpoint, None);
    let backend = config.nomad_backends()[0].clone();
    let admitted = admitted_request();
    let key = admitted.shared_build_key();
    let job = deterministic_job_name(&backend, key.as_bytes());
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let derivation = std::str::from_utf8(admitted.derivation_path()).unwrap();
    telchar::persistence::claim_shared_build_with_request(
        database.url(),
        derivation,
        &admitted.shared_build_digest(),
        "nomad-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&job),
        &admitted
            .expected_outputs()
            .iter()
            .map(|(_, path)| std::str::from_utf8(path).unwrap())
            .collect::<Vec<_>>(),
        &admitted,
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), derivation).unwrap();
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().unwrap();
        assert!(read_http_request_with_body(&mut request).contains(&job));
        thread::sleep(Duration::from_secs(2));
        drop(request);
    });
    let client = NomadClient::new(backend).unwrap();
    let builds = telchar::shared_build::SharedBuildRegistry::new();
    let leader = match builds.acquire(&key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        _ => panic!("build leads"),
    };
    let execution =
        BuildExecution::new("request-1", &admitted, Duration::from_millis(100)).unwrap();
    let started = std::time::Instant::now();
    let result = client.execute(
        database.url(),
        &execution,
        key.as_bytes(),
        &mut |_| Ok(()),
        &builds,
        1024,
        &mut || Ok(false),
    );
    let elapsed = started.elapsed();
    server.join().unwrap();
    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_secs(1),
        "submission exceeded execution budget: {elapsed:?}"
    );
    assert_eq!(
        telchar::persistence::read_nomad_execution_obligations(database.url())
            .unwrap()
            .len(),
        1
    );
    drop(leader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovered_status_obeys_cleanup_budget() {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let _ = load_service_config(&root, &endpoint, None);
    let path = root.join("client.toml");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        source.replace(
            "runtime_limit_seconds = 60",
            "runtime_limit_seconds = 60\ncleanup_timeout_seconds = 1",
        ),
    )
    .unwrap();
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let client = NomadClient::new(config.nomad_backends()[0].clone()).unwrap();
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().unwrap();
        assert!(read_http_request(&mut request).starts_with("GET /v1/job/exact-recovered"));
        thread::sleep(Duration::from_secs(2));
    });
    let started = std::time::Instant::now();
    assert!(client.status("exact-recovered").is_err());
    let elapsed = started.elapsed();
    server.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(1700),
        "recovered status exceeded cleanup budget: {elapsed:?}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn timeout_stops_only_the_exact_submitted_nomad_job() {
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

        let (mut stop_request, _) = listener.accept().expect("stop request accepts");
        let request = read_http_request(&mut stop_request);
        assert!(request.starts_with(&format!(
            "DELETE /v1/job/{served_job_id}?namespace=telchar&purge=false HTTP/1.1\r\n"
        )));
        write_json_response(&mut stop_request, 200, r#"{"EvalID":"evaluation-2"}"#);
    });
    let database = telchar_test_support::postgres::PostgresFixture::start();
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
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(1))
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
            &mut || {
                thread::sleep(Duration::from_millis(1100));
                Ok(false)
            },
        )
        .expect_err("timed out execution rejects");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    drop(leader);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_does_not_retry_callback_recorded_build_failure() {
    callback_recorded_failure(true);
}

#[test]
fn configured_backend_keeps_callback_infrastructure_failure_distinct() {
    callback_recorded_failure(false);
}

fn callback_recorded_failure(ordinary: bool) {
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
    let database = telchar_test_support::postgres::PostgresFixture::start();
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
            &if ordinary { serde_json::json!({"nix_failure": {"status": 4, "message": "builder failed with exit code 3"}}) }
            else { serde_json::json!({"diagnostic": "worker Nix store connection failed"}) },
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

    let result = executor.execute(&execution);
    if ordinary {
        let failure = result.unwrap().failure().cloned().unwrap();
        assert_eq!(failure.status, 4);
        assert_eq!(failure.message, "builder failed with exit code 3");
    } else {
        let error = result.expect_err("infrastructure failure remains an execution error");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
    }

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
    retry_after_status(404, 2, false);
}

#[test]
fn uncertain_monitoring_retains_attempt_and_capacity_across_reload() {
    retry_after_status(500, 1, false);
}

#[test]
fn database_uncertainty_retains_execution_capacity() {
    retry_after_status(500, 1, true);
}

fn retry_after_status(status_code: u16, expected_ordinal: i32, database_failure: bool) {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture binds");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("fixture address reads")
    );
    load_service_config(&root, &endpoint, None);
    let config_path = root.join("client.toml");
    fs::write(
        &config_path,
        format!(
            "[backends]\npermit_wait_seconds = 1\n{}",
            fs::read_to_string(&config_path).unwrap()
        ),
    )
    .unwrap();
    let mut config = ServiceConfig::load_from_default(&config_path).unwrap();
    let backend = config.nomad_backends()[0].clone();
    let admitted = admitted_request();
    let key = admitted.shared_build_key();
    let first_job = deterministic_job_name(&backend, key.as_bytes());
    let second_job =
        telchar::nomad::backend::deterministic_job_name_for_attempt(&backend, key.as_bytes(), 2)
            .expect("retry identity derives");
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let failure_database = database.url().to_owned();
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
            if database_failure {
                postgres::Client::connect(&failure_database, postgres::NoTls)
                    .unwrap()
                    .batch_execute(
                        "ALTER TABLE nomad_execution_obligations RENAME TO hidden_obligations",
                    )
                    .unwrap();
            }
            write_json_response(&mut status, status_code, r#"{}"#);
            if status_code == 500 {
                for code in if database_failure {
                    vec![401, 403, 400, 200, 201, 429, 500, 502, 503, 504, 404]
                } else {
                    Vec::new()
                } {
                    let (mut cleanup, _) = listener.accept().unwrap();
                    assert!(
                        read_http_request(&mut cleanup)
                            .starts_with(&format!("GET /v1/job/{job_id}?"))
                    );
                    if code == 201 {
                        write_json_response(
                            &mut cleanup,
                            200,
                            r#"{"ID":"foreign-job","Namespace":"telchar","Type":"batch","Version":1,"Meta":{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}"#,
                        );
                    } else {
                        write_json_response(&mut cleanup, code, r#"{}"#);
                    }
                }
                break;
            }
        }
    });
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
    let backends = ConfiguredBackends::new(&config, gateway_store_endpoint()).unwrap();
    let mut executor = backends
        .executor(
            telchar::persistence::Database::connect(database.url()).unwrap(),
            Arc::clone(&builds),
        )
        .unwrap();

    executor
        .execute(&execution)
        .expect_err("unavailable execution rejects");

    assert_eq!(
        telchar::persistence::read_shared_build_attempt(database.url(), derivation)
            .expect("attempt reads")
            .expect("attempt exists")
            .ordinal,
        expected_ordinal
    );
    if database_failure {
        database
            .connect()
            .batch_execute("ALTER TABLE hidden_obligations RENAME TO nomad_execution_obligations")
            .unwrap();
    }
    let unresolved: i64 = database
        .connect()
        .query_one(
            "SELECT count(*) FROM nomad_execution_obligations WHERE resolved_at IS NULL",
            &[],
        )
        .expect("obligations read")
        .get(0);
    assert_eq!(unresolved, if status_code == 500 { 1 } else { 0 });
    if status_code == 500 {
        assert!(executor.reserve_target("x86_64-linux", &[]).is_err());
        let health = backends.static_ssh_health();
        let mut health_service = telchar::service::daemon_services::StaticSshHealthService::start(
            health,
            Duration::from_secs(60),
        )
        .unwrap();
        let reloadable = telchar::backend::routing::ReloadableBackends::new(backends);
        let replacement = ServiceConfig::load_from_default(&config_path).unwrap();
        telchar::service::config_reload::BackendReload::prepare_config(
            &config,
            replacement,
            Some(gateway_store_endpoint()),
            None,
            Duration::from_secs(60),
        )
        .unwrap()
        .apply(&mut config, &reloadable, &mut health_service)
        .unwrap();
        let reloaded = reloadable
            .snapshot()
            .executor(
                telchar::persistence::Database::connect(database.url()).unwrap(),
                Arc::clone(&builds),
            )
            .unwrap();
        assert!(reloaded.reserve_target("x86_64-linux", &[]).is_err());
        health_service.shutdown().unwrap();
        if database_failure {
            let connection = telchar::persistence::Database::connect(database.url()).unwrap();
            reloadable
                .snapshot()
                .restore_nomad_capacity(&connection)
                .unwrap();
            for _ in 0..5 {
                assert!(
                    reloadable
                        .snapshot()
                        .reconcile_nomad_capacity(&connection)
                        .is_err(),
                    "credential, request, malformed response, and identity errors remain fatal"
                );
                assert!(executor.reserve_target("x86_64-linux", &[]).is_err());
            }
            for _ in 0..5 {
                reloadable
                    .snapshot()
                    .reconcile_nomad_capacity(&connection)
                    .unwrap();
                assert!(
                    executor.reserve_target("x86_64-linux", &[]).is_err(),
                    "transient cleanup retains capacity"
                );
                assert_eq!(
                    telchar::persistence::read_nomad_execution_obligations(&connection)
                        .unwrap()
                        .len(),
                    1
                );
            }
            reloadable
                .snapshot()
                .reconcile_nomad_capacity(&connection)
                .unwrap();
            assert!(
                telchar::persistence::read_nomad_execution_obligations(&connection)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                executor.reserve_target("x86_64-linux", &[]).is_err(),
                "database-uncertain permit remains retained independently of restored obligations"
            );
        }
    } else {
        assert!(
            executor
                .reserve_target("x86_64-linux", &[])
                .unwrap()
                .is_some()
        );
    }
    drop(leader);
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn configured_backend_retries_missing_nomad_execution_with_distinct_identity() {
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
    let database = telchar_test_support::postgres::PostgresFixture::start();
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
    let database = telchar_test_support::postgres::PostgresFixture::start();
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
