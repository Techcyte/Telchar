//! Exercises exact durable Nomad occupancy across restart and cleanup.

use super::*;

#[test]
fn restored_capacity_survives_uncertainty_and_releases_only_exact_absence() {
    restored_capacity(false);
}

#[test]
fn reconciliation_budget_continues_after_stalled_predecessor() {
    restored_capacity(true);
}

fn restored_capacity(stalled: bool) {
    let root = fixture_root();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    load_service_config(&root, &endpoint, None);
    let path = root.join("client.toml");
    fs::write(
        &path,
        format!(
            "[backends]\npermit_wait_seconds = 1\n{}",
            fs::read_to_string(&path).unwrap().replace(
                "runtime_limit_seconds = 60",
                "runtime_limit_seconds = 60\ncleanup_timeout_seconds = 1"
            )
        ),
    )
    .unwrap();
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let mut database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    for (derivation, job) in [
        (
            "/nix/store/11111111111111111111111111111111-one.drv",
            "job-one",
        ),
        (
            "/nix/store/22222222222222222222222222222222-two.drv",
            "job-two",
        ),
    ] {
        telchar::persistence::claim_shared_build(
            database.url(),
            derivation,
            &[4; 32],
            "nomad-test",
            BackendKind::Nomad,
            BackendKind::Nomad.capabilities(),
            Some(job),
            &["/nix/store/33333333333333333333333333333333-output"],
        )
        .unwrap();
        telchar::persistence::start_shared_build(database.url(), derivation).unwrap();
        telchar::persistence::retain_nomad_execution_obligation(database.url(), derivation, job)
            .unwrap();
    }
    database.restart();
    let connection = telchar::persistence::Database::connect(database.url()).unwrap();
    let backends = ConfiguredBackends::new(&config, gateway_store_endpoint()).unwrap();
    backends.restore_nomad_capacity(&connection).unwrap();
    backends.restore_nomad_capacity(&connection).unwrap();
    let executor = backends
        .executor(
            connection.clone(),
            Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
        )
        .unwrap();
    assert!(executor.reserve_target("x86_64-linux", &[]).is_err());
    let server = thread::spawn(move || {
        if stalled {
            let (mut request, _) = listener.accept().unwrap();
            assert!(read_http_request(&mut request).contains("/v1/job/job-one?"));
            let stalled_request = thread::spawn(move || {
                thread::sleep(Duration::from_secs(2));
                drop(request);
            });
            let (mut request, _) = listener.accept().unwrap();
            assert!(read_http_request(&mut request).contains("/v1/job/job-two?"));
            write_json_response(&mut request, 404, "{}");
            let (mut request, _) = listener.accept().unwrap();
            assert!(read_http_request(&mut request).contains("/v1/job/job-one?"));
            write_json_response(&mut request, 500, "{}");
            stalled_request.join().unwrap();
            return;
        }
        for (job, status) in [
            ("job-one", 500),
            ("job-two", 500),
            ("job-one", 404),
            ("job-two", 404),
        ] {
            let (mut request, _) = listener.accept().unwrap();
            assert!(
                read_http_request(&mut request)
                    .starts_with(&format!("GET /v1/job/{job}?namespace=telchar "))
            );
            write_json_response(&mut request, status, "{}");
        }
    });
    let started = std::time::Instant::now();
    backends.reconcile_nomad_capacity(&connection).unwrap();
    if stalled {
        assert!(
            started.elapsed() < Duration::from_millis(1700),
            "aggregate sweep exceeded budget: {:?}",
            started.elapsed()
        );
    }
    assert_eq!(
        telchar::persistence::read_nomad_execution_obligations(&connection)
            .unwrap()
            .len(),
        2
    );
    assert!(executor.reserve_target("x86_64-linux", &[]).is_err());
    backends.reconcile_nomad_capacity(&connection).unwrap();
    if stalled {
        let remaining =
            telchar::persistence::read_nomad_execution_obligations(&connection).unwrap();
        assert_eq!(remaining.len(), 1);
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
        return;
    }
    assert!(
        telchar::persistence::read_nomad_execution_obligations(&connection)
            .unwrap()
            .is_empty()
    );
    assert!(
        executor
            .reserve_target("x86_64-linux", &[])
            .unwrap()
            .is_some()
    );
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}
