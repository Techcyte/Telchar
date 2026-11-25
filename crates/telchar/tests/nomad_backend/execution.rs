//! Tests Nomad execution.

use super::*;

#[test]
fn independent_daemon_requests_resubmit_after_failed_nomad_cleanup() {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let root = fixture_root();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    load_service_config(&root, &endpoint, None);
    let finished = Arc::new(AtomicBool::new(false));
    let server_finished = Arc::clone(&finished);
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut jobs = std::collections::BTreeMap::<String, serde_json::Value>::new();
        let mut submissions = Vec::new();
        while !server_finished.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "Nomad fixture exceeded deadline");
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("Nomad fixture accept: {error}"),
            };
            let request = read_http_request(&mut stream);
            let line = request.lines().next().unwrap();
            if line.starts_with("POST ") {
                let (headers, body) = request.split_once("\r\n\r\n").unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = body.as_bytes().to_vec();
                while body.len() < length {
                    let mut bytes = [0; 4096];
                    let count = stream.read(&mut bytes).unwrap();
                    assert_ne!(count, 0);
                    body.extend_from_slice(&bytes[..count]);
                }
                let submitted: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let mut job = submitted["Job"].clone();
                let id = job["ID"].as_str().unwrap().to_owned();
                job["Version"] = serde_json::json!(0);
                job["Stop"] = serde_json::json!(false);
                submissions.push(id.clone());
                jobs.insert(id, job);
                write_json_response(&mut stream, 200, r#"{"EvalID":"evaluation"}"#);
            } else {
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap();
                let rest = path.strip_prefix("/v1/job/").expect("exact job endpoint");
                let id = rest.strip_suffix("/allocations").unwrap_or(rest);
                let job = jobs.get_mut(id).expect("only submitted jobs are inspected");
                if line.starts_with("DELETE ") {
                    job["Stop"] = serde_json::json!(true);
                    write_json_response(&mut stream, 200, "{}");
                } else if rest.ends_with("/allocations") {
                    write_json_response(
                        &mut stream,
                        200,
                        r#"[{"ID":"allocation","JobVersion":0,"ClientStatus":"failed"}]"#,
                    );
                } else {
                    write_json_response(&mut stream, 200, &job.to_string());
                }
            }
        }
        submissions
    });
    let database = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let socket = root.join("daemon.sock");
    fs::create_dir(root.join("gc-roots")).unwrap();
    let stderr_path = root.join("daemon.stderr");
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_telchar"))
            .args([
                "daemon",
                "--socket",
                socket.to_str().unwrap(),
                "--frontend-uid",
                &rustix::process::getuid().as_raw().to_string(),
            ])
            .env("TELCHAR_CONFIG", root.join("client.toml"))
            .env("TELCHAR_DATABASE_URL", database.url())
            .env(
                "TELCHAR_GATEWAY_STORE_URI",
                "unix:///nix/var/nix/daemon-socket/socket",
            )
            .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", root.join("gc-roots"))
            .env("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", "67108864")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(&stderr_path).unwrap()
        );
        assert!(Instant::now() < deadline, "daemon did not bind");
        thread::sleep(Duration::from_millis(5));
    }
    let ssh = root.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nexec \"$TELCHAR_STDIO_BIN\" serve-stdio\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let instantiated = Command::new("nix-instantiate").args(["--store", "local", "--expr", "derivation { name = \"telchar-nomad-independent-failure\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"exit 3\" ]; }"]).output().unwrap();
    assert!(instantiated.status.success(), "{instantiated:?}");
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mut failures = Vec::new();
    for _ in 0..2 {
        let result = Command::new("timeout")
            .args([
                "15",
                "nix",
                "--extra-experimental-features",
                "nix-command",
                "build",
                "--store",
                "ssh-ng://fixture",
                "--no-link",
                &format!("{drv}^*"),
            ])
            .env(
                "PATH",
                format!("{}:{}", root.display(), std::env::var("PATH").unwrap()),
            )
            .env("TELCHAR_STDIO_BIN", env!("CARGO_BIN_EXE_telchar"))
            .env("TELCHAR_IPC_SOCKET", &socket)
            .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_ne!(result.status.code(), Some(124), "client timed out");
        failures.push(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    drop(daemon);
    finished.store(true, Ordering::SeqCst);
    let submissions = server.join().unwrap();
    assert_eq!(
        submissions.len(),
        4,
        "each request gets two attempts: {failures:?}; {}",
        fs::read_to_string(&stderr_path).unwrap()
    );
    assert_eq!(
        submissions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4,
        "attempt job identities must be distinct"
    );
    for failure in failures {
        assert!(
            failure.contains("BuildDerivation execution failed"),
            "{failure}"
        );
        assert!(!failure.contains("shared build retry failed"), "{failure}");
    }
    fs::remove_dir_all(root).unwrap();
}

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
    listener.set_nonblocking(true).unwrap();
    let (submission_finished, finished) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut request = loop {
            match listener.accept() {
                Ok((request, _)) => break request,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if finished.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                        return false;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("submission listener failed: {error}"),
            }
        };
        assert!(read_http_request_with_body(&mut request).contains(&job));
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("submission finishes while HTTP response is withheld");
        true
    });
    let client = NomadClient::new(backend).unwrap();
    let builds = telchar::shared_build::SharedBuildRegistry::new();
    let leader = match builds.acquire(&key) {
        telchar::shared_build::SharedBuildAccess::Leader(leader) => leader,
        _ => panic!("build leads"),
    };
    let execution = BuildExecution::new("request-1", &admitted, Duration::from_secs(1)).unwrap();
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
    let _ = submission_finished.send(());
    let submitted = server.join().unwrap();
    assert!(
        submitted,
        "execution returned before HTTP submission: {result:?}; elapsed={elapsed:?}"
    );
    let error = result.unwrap_err();
    assert!(
        format!("{error:?}").contains("TimedOut"),
        "submission must time out: {error:?}"
    );
    assert!(
        elapsed >= execution.timeout() && elapsed < Duration::from_secs(2),
        "submission did not honor execution budget: {elapsed:?}"
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
    callback_recorded_failure(true, true);
}

#[test]
fn configured_backend_keeps_callback_infrastructure_failure_distinct() {
    callback_recorded_failure(false, true);
}

#[test]
fn callback_failure_retains_obligation_when_cleanup_is_uncertain() {
    callback_recorded_failure(true, false);
}

fn callback_recorded_failure(ordinary: bool, cleanup_confirmed: bool) {
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
        if !cleanup_confirmed {
            let (mut status, _) = listener.accept().unwrap();
            assert!(
                read_http_request(&mut status).starts_with(&format!("GET /v1/job/{served_job}?"))
            );
            write_json_response(&mut status, 500, r#"{}"#);
        }
        for stopped in [false, true].into_iter().filter(|_| cleanup_confirmed) {
            let (mut status, _) = listener.accept().unwrap();
            assert!(
                read_http_request(&mut status).starts_with(&format!("GET /v1/job/{served_job}?"))
            );
            write_json_response(
                &mut status,
                200,
                &format!(
                    r#"{{"ID":"{served_job}","Stop":{stopped},"Namespace":"telchar","Type":"batch","Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
                ),
            );
            let (mut allocations, _) = listener.accept().unwrap();
            assert!(read_http_request(&mut allocations).contains("/allocations?"));
            write_json_response(
                &mut allocations,
                200,
                if stopped {
                    r#"[{"ID":"allocation-1","ClientStatus":"complete"}]"#
                } else {
                    r#"[{"ID":"allocation-1","ClientStatus":"running"}]"#
                },
            );
            if !stopped {
                let (mut stop, _) = listener.accept().unwrap();
                assert!(
                    read_http_request(&mut stop)
                        .starts_with(&format!("DELETE /v1/job/{served_job}?"))
                );
                write_json_response(&mut stop, 200, r#"{}"#);
            }
        }
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
    let obligations =
        telchar::persistence::read_nomad_execution_obligations(database.url()).unwrap();
    assert_eq!(obligations.len(), usize::from(!cleanup_confirmed));
    let reclaimed = telchar::persistence::claim_shared_build_with_request(
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
            .map(|(_, path)| std::str::from_utf8(path).unwrap())
            .collect::<Vec<_>>(),
        &admitted,
    )
    .unwrap();
    assert_eq!(
        reclaimed.ownership,
        if cleanup_confirmed {
            telchar::persistence::SharedBuildOwnership::Claimed
        } else {
            telchar::persistence::SharedBuildOwnership::Joined
        }
    );
    if !cleanup_confirmed {
        assert_eq!(
            reclaimed.build.state,
            telchar::persistence::SharedBuildState::Failed
        );
    }
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
fn configured_backend_retries_failed_nomad_execution() {
    retry_after_status(200, 2, false);
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
            if status_code == 200 {
                write_json_response(
                    &mut status,
                    200,
                    &format!(
                        r#"{{"ID":"{job_id}","Namespace":"telchar","Type":"batch","Version":1,"Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
                    ),
                );
                let (mut allocations, _) = listener.accept().unwrap();
                assert!(read_http_request(&mut allocations).contains("/allocations?"));
                write_json_response(
                    &mut allocations,
                    200,
                    r#"[{"ID":"failed-allocation","JobVersion":1,"ClientStatus":"failed"}]"#,
                );
                for stopped in [false, true] {
                    let (mut cleanup, _) = listener.accept().unwrap();
                    assert!(
                        read_http_request(&mut cleanup)
                            .starts_with(&format!("GET /v1/job/{job_id}?"))
                    );
                    write_json_response(
                        &mut cleanup,
                        200,
                        &format!(
                            r#"{{"ID":"{job_id}","Stop":{stopped},"Namespace":"telchar","Type":"batch","Version":1,"Meta":{{"telchar_backend":"nomad-test","telchar_system":"x86_64-linux"}}}}"#
                        ),
                    );
                    let (mut allocations, _) = listener.accept().unwrap();
                    assert!(read_http_request(&mut allocations).contains("/allocations?"));
                    write_json_response(
                        &mut allocations,
                        200,
                        r#"[{"ID":"failed-allocation","JobVersion":1,"ClientStatus":"failed"}]"#,
                    );
                    if !stopped {
                        let (mut stop, _) = listener.accept().unwrap();
                        assert!(
                            read_http_request(&mut stop)
                                .starts_with(&format!("DELETE /v1/job/{job_id}?"))
                        );
                        write_json_response(&mut stop, 200, r#"{}"#);
                    }
                }
                continue;
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

    let execution_error = executor
        .execute(&execution)
        .expect_err("unavailable execution rejects");
    assert!(
        !execution_error
            .to_string()
            .contains("shared build retry failed"),
        "{execution_error}"
    );

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
