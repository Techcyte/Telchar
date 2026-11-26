//! Tests Nomad identity.

use super::*;

#[test]
fn monitors_only_the_exact_backend_bound_job() {
    monitor_job(false, false, false);
}

#[test]
fn cleanup_requires_stopped_job_and_every_allocation_terminal() {
    monitor_job(true, false, false);
    monitor_job(false, true, false);
    monitor_job(true, true, true);
}

#[test]
fn cleanup_requests_share_one_deadline() {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    load_nomad_config(&root, &endpoint, None);
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
    let service = ServiceConfig::load_from_default(&path).unwrap();
    let config = service.nomad_backends()[0].clone();
    assert_eq!(config.cleanup_timeout(), Duration::from_secs(1));
    let job = deterministic_job_name(&config, b"cleanup-budget");
    let expected = job.clone();
    let server = thread::spawn(move || {
        let (mut request, _) = listener.accept().unwrap();
        assert!(read_http_request(&mut request).starts_with("GET /v1/job/"));
        thread::sleep(Duration::from_millis(650));
        write_json_response(
            &mut request,
            200,
            &format!(
                r#"{{"ID":"{expected}","Namespace":"telchar","Type":"batch","Stop":true,"Version":1,"Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
            ),
        );
        let (mut allocations, _) = listener.accept().unwrap();
        assert!(read_http_request(&mut allocations).contains("/allocations?"));
        thread::sleep(Duration::from_millis(900));
        drop(allocations);
    });
    let client = NomadClient::new(config).unwrap();
    let started = std::time::Instant::now();
    let result = client.execution_cleanup_complete(&job);
    let elapsed = started.elapsed();
    server.join().unwrap();
    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_millis(1400),
        "cleanup exceeded shared budget: {elapsed:?}"
    );
    fs::remove_dir_all(root).unwrap();
}

fn monitor_job(stopped: bool, terminal: bool, cleanup: bool) {
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
                r#"{{"ID":"{expected_job_id}","Namespace":"telchar","Type":"batch","Stop":{stopped},"Version":1,"Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
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
            &format!(
                r#"[{{"ID":"allocation-1","JobVersion":1,"ClientStatus":"complete"}},{{"ID":"allocation-2","JobVersion":0,"ClientStatus":"{}"}}]"#,
                if terminal { "failed" } else { "running" }
            ),
        );
        if !cleanup && !stopped {
            let (mut stop, _) = listener.accept().expect("non-purging stop accepts");
            let request = read_http_request(&mut stop);
            assert!(request.starts_with(&format!(
                "DELETE /v1/job/{expected_job_id}?namespace=telchar&purge=false HTTP/1.1\r\n"
            )));
            write_json_response(&mut stop, 200, r#"{"EvalID":"cleanup"}"#);
        }
    });
    let client = NomadClient::new(config).expect("Nomad client constructs");
    assert_eq!(
        client
            .execution_cleanup_complete(&job_id)
            .expect("Nomad cleanup status reads"),
        cleanup
    );
    server.join().expect("HTTP fixture joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn verifies_exact_callback_allocation_identity() {
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
        assert!(
            read_http_request(&mut request)
                .starts_with("GET /v1/allocation/allocation-1?namespace=telchar HTTP/1.1\r\n")
        );
        write_json_response(
            &mut request,
            200,
            r#"{"ID":"allocation-1","Namespace":"telchar","JobID":"job-1","TaskGroup":"build","NodeID":"producer-node","ClientStatus":"running","TaskStates":{"prestart":{"State":"dead"},"build":{"State":"running"}}}"#,
        );
    });

    assert_eq!(
        client
            .verify_allocation("allocation-1", "job-1", "build")
            .expect("exact allocation verifies"),
        Some("producer-node".to_owned())
    );
    server.join().expect("server joins");
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn verifies_exact_callback_allocation_during_task_startup() {
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
    let root = fixture_root();
    let config = load_service_config(&root, "http://127.0.0.1:4646", None);
    let backend = config.nomad_backends()[0].clone();
    let configured =
        ConfiguredBackends::new(&config, gateway_store_endpoint()).expect("backends configure");
    let executor = configured
        .executor(
            telchar::persistence::Database::connect("postgresql://fixture")
                .expect("database configuration is valid"),
            Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
        )
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
