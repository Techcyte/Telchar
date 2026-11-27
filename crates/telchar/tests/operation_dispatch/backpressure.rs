//! Tests unread frontend backpressure and request cleanup.

use super::*;

static LOG_FLOODS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn unread_frontend_does_not_block_build_completion() {
    unread_build_completion(false, false, 0);
}

#[test]
fn unread_frontend_does_not_block_real_local_build_completion() {
    unread_build_completion(true, false, 0);
}

#[test]
fn unread_coalesced_frontends_do_not_block_real_local_completion() {
    unread_build_completion(true, true, 0);
}

#[test]
fn unread_completed_frontend_disconnect_releases_protocol_session() {
    unread_build_completion(true, false, 1);
}

#[test]
fn unread_completed_frontend_does_not_block_ownership_shutdown() {
    unread_build_completion(true, false, 2);
}

fn unread_build_completion(real_store: bool, attach_follower: bool, completion_action: u8) {
    let _flood = LOG_FLOODS.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("build-helper");
    let completed = root.path().join("completed");
    let started = root.path().join("started");
    let release = root.path().join("release");
    fs::write(&helper, format!(
        "#!/bin/sh\nset -eu\ncat >/dev/null\nhead -c 67108864 /dev/zero >&2\ntouch '{}'\nprintf '%s\\n' '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}'\n", completed.display()
    )).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let build = real_store.then(|| {
        assert!(std::env::var_os("TELCHAR_STORE_ROOT").is_some());
        let head = Command::new("sh").args(["-c", "command -v head"]).output().unwrap();
        assert!(head.status.success());
        let head = String::from_utf8(head.stdout).unwrap();
        let expression = format!(r#"derivation {{ name = "telchar-unread-local"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "printf started > {}; while [ ! -e {} ]; do :; done; {} -c 67108864 /dev/zero >&2; printf done > {}; printf independent-output > $out" ]; }}"#, started.display(), release.display(), head.trim(), completed.display());
        let instantiated = Command::new("nix-instantiate").args(["--store", "local", "--expr", &expression]).output().unwrap();
        assert!(instantiated.status.success(), "{instantiated:?}");
        let drv = String::from_utf8(instantiated.stdout).unwrap().trim().to_owned();
        let kind = telchar::backend::BackendKind::Local;
        telchar::build::BuildRequest::from_stored_derivation(drv.as_bytes(), &fs::read(&drv).unwrap(), &[
            telchar::backend::BackendTarget::new("local", kind, "x86_64-linux", [] as [&str; 0]).unwrap(),
        ]).unwrap()
    });
    let archive_directory = root.path().join("logs");
    let archive_config = root.path().join("service.toml");
    if real_store {
        fs::create_dir(&archive_directory).unwrap();
        fs::write(&archive_config, format!("[log_retention]\nretention_days = 7\ndirectory = '{}'\nmaximum_bytes = 1048576\nmaximum_queue_bytes = 1048576\nmaximum_captures = 1\nmaximum_spool_bytes = 4194304\n[backends.local]\nname = 'local'\nsystem = 'x86_64-linux'\nmaximum_concurrent_builds = 1\n", archive_directory.display())).unwrap();
    }
    let mut fixture = if real_store {
        FrontendFixture::spawn_multi_with_store(
            "unix:///nix/var/nix/daemon-socket/socket",
            [("TELCHAR_CONFIG", archive_config.display().to_string())],
        )
    } else {
        FrontendFixture::spawn_multi_with_store(
            "unix:///fixed-gateway.sock",
            [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        )
    };
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    if let Some(build) = &build {
        super::build_failure::send_failure_build(&mut input, build, "", false);
    } else {
        write_build_derivation_request(&mut input, "x86_64-linux", 0);
        input.flush().unwrap();
    }
    let mut follower = if attach_follower {
        wait_for_file_for(
            &started,
            Duration::from_secs(5),
            "real leader did not start",
        );
        let mut child = fixture.spawn_frontend();
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        complete_handshake(&mut input, &mut output);
        super::build_failure::send_failure_build(&mut input, build.as_ref().unwrap(), "", false);
        assert_eq!(read_integer(&mut output), nix_worker_protocol::STDERR_NEXT);
        assert_eq!(
            read_string(&mut output),
            "identical build already in progress\n"
        );
        Some((child, input, output))
    } else {
        None
    };
    fs::write(&release, b"release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let succeeded = loop {
        let state = fixture
            .database
            .connect()
            .query_opt("SELECT state FROM shared_builds", &[])
            .unwrap();
        if state.is_some_and(|row| row.get::<_, String>(0) == "succeeded") {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    if succeeded && real_store {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let row = fixture.database.connect().query_opt("SELECT object_key, complete, uncompressed_bytes FROM attempt_log_archives WHERE state = 'available'", &[]).unwrap();
            if let Some(row) = row {
                assert!(
                    !row.get::<_, bool>(1),
                    "bounded flood archive must report truncation"
                );
                let bytes = row.get::<_, i64>(2);
                assert!(bytes > 0 && bytes <= 1048576);
                let path: String = row.get(0);
                let decoded = zstd::stream::decode_all(fs::File::open(path).unwrap()).unwrap();
                assert_eq!(decoded.len() as i64, bytes);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real local execution did not retain bounded logs"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    if succeeded && completion_action == 2 {
        fixture.database.expire_singleton_ownership("daemon");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = fixture.daemon.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "unread terminal response blocked ownership shutdown"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        assert!(!fixture.root.join("daemon.sock").exists());
        assert!(fixture.frontend.try_wait().unwrap().is_none());
        fixture.frontend.kill().unwrap();
        fixture.frontend.wait().unwrap();
        drop(input);
        drop(output);
        let daemon_output = fixture.wait_for_daemon();
        let diagnostics = String::from_utf8(daemon_output.stderr).unwrap();
        assert!(
            diagnostics.contains("database.singleton_ownership.lost"),
            "{diagnostics}"
        );
        fs::remove_dir_all(&fixture.root).unwrap();
        return;
    }
    if succeeded && completion_action == 1 {
        fixture.frontend.kill().unwrap();
        fixture.frontend.wait().unwrap();
        drop(input);
        drop(output);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let open: i64 = fixture
                .database
                .connect()
                .query_one(
                    "SELECT count(*) FROM protocol_sessions WHERE state = 'open'",
                    &[],
                )
                .unwrap()
                .get(0);
            if open == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "completed unread session did not close after disconnect"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let mut replacement = fixture.spawn_frontend();
        let mut input = replacement.stdin.take().unwrap();
        let mut output = replacement.stdout.take().unwrap();
        complete_handshake(&mut input, &mut output);
        super::build_failure::send_failure_build(&mut input, build.as_ref().unwrap(), "", false);
        read_build_status(&mut output, 2).unwrap();
        drop(input);
        assert!(replacement.wait().unwrap().success());
        let diagnostics = fixture.finish();
        assert!(completed.exists(), "{diagnostics}");
        return;
    }
    if succeeded {
        if attach_follower {
            let mut independent = fixture.spawn_frontend();
            let mut independent_input = independent.stdin.take().unwrap();
            let mut independent_output = independent.stdout.take().unwrap();
            complete_handshake(&mut independent_input, &mut independent_output);
            super::build_failure::send_failure_build(
                &mut independent_input,
                build.as_ref().unwrap(),
                "",
                false,
            );
            read_build_status(&mut independent_output, 2).unwrap();
            drop(independent_input);
            assert!(independent.wait().unwrap().success());
        }
        let mut truncated = false;
        loop {
            let frame = read_integer(&mut output);
            if frame == STDERR_LAST {
                break;
            }
            assert_eq!(frame, nix_worker_protocol::STDERR_NEXT);
            truncated |= read_string(&mut output).as_bytes()
                == telchar::shared_build::LIVE_LOG_TRUNCATION_MARKER;
        }
        assert!(truncated, "resumed reader must observe omitted logs");
        assert_eq!(read_integer(&mut output), 0);
        assert!(read_string(&mut output).is_empty());
        for _ in 0..6 {
            read_integer(&mut output);
        }
        assert_eq!(read_integer(&mut output), 1);
        assert!(read_string(&mut output).ends_with("!out"));
        let realization: serde_json::Value =
            serde_json::from_str(&read_string(&mut output)).unwrap();
        let expected_path = build
            .as_ref()
            .map(|build| std::str::from_utf8(&build.expected_outputs()[0].1).unwrap())
            .unwrap_or(
                "/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract",
            );
        assert_eq!(
            realization["outPath"],
            expected_path.strip_prefix("/nix/store/").unwrap()
        );
        if real_store {
            assert_eq!(fs::read(expected_path).unwrap(), b"independent-output");
            super::build_failure::send_failure_build(
                &mut input,
                build.as_ref().unwrap(),
                "",
                false,
            );
            read_build_status(&mut output, 2).unwrap();
        }
        drop(input);
        assert!(fixture.frontend.wait().unwrap().success());
    } else {
        fixture.frontend.kill().unwrap();
        fixture.frontend.wait().unwrap();
        drop(input);
    }
    drop(output);
    if let Some((mut child, input, output)) = follower.take() {
        if succeeded {
            read_build_success(output).unwrap();
            drop(input);
            assert!(child.wait().unwrap().success());
            let owners: i64 = fixture.database.connect().query_one("SELECT count(DISTINCT owner_id) FROM store_leases WHERE purpose = 'output' AND state = 'active'", &[]).unwrap().get(0);
            assert_eq!(owners, 4);
        } else {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    let helper_completed = completed.exists();
    let states = fixture
        .database
        .connect()
        .query("SELECT state FROM shared_builds", &[])
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let diagnostics = fixture.finish();
    assert!(
        succeeded && helper_completed,
        "unread frontend blocked execution completion: helper_completed={helper_completed}, states={states:?}: {diagnostics}"
    );
}

#[test]
fn unread_frontend_disconnect_cancels_running_helper_and_cleans_request() {
    stop_running_log_producer(false);
}

#[test]
fn ownership_loss_stops_running_log_producer_with_unread_frontend() {
    stop_running_log_producer(true);
}

fn stop_running_log_producer(ownership_loss: bool) {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-log-backpressure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let pid_path = root.join("pid");
    let started_path = root.join("started");
    let completed_path = root.join("completed");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nprintf started > '{}'\nprintf 'telchar-hostile-log-secret' >&2\nhead -c 67108864 /dev/zero >&2\nwhile :; do head -c 1048576 /dev/zero >&2; done\nprintf completed > '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            pid_path.display(),
            started_path.display(),
            completed_path.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut fixture = if ownership_loss {
        FrontendFixture::spawn_multi_with_store(
            "unix:///fixed-gateway.sock",
            [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        )
    } else {
        FrontendFixture::spawn_with_store(
            None,
            "unix:///fixed-gateway.sock",
            [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
        )
    };
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
    input.flush().expect("BuildDerivation request flushes");

    let deadline = Instant::now() + Duration::from_secs(2);
    while !started_path.exists() {
        assert!(
            Instant::now() < deadline,
            "helper did not start log production"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let blocked_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < blocked_deadline {
        assert!(
            !completed_path.exists(),
            "helper passed its running barrier before disconnect"
        );
        assert!(
            child.try_wait().expect("frontend status reads").is_none(),
            "frontend exited before disconnect"
        );
        thread::sleep(Duration::from_millis(5));
    }

    if ownership_loss {
        fixture.database.expire_singleton_ownership("daemon");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = fixture.daemon.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "active unread build blocked ownership shutdown"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        assert!(!fixture.root.join("daemon.sock").exists());
    }
    if !ownership_loss {
        child.kill().expect("frontend terminates");
        child.wait().expect("frontend reaps");
    }

    let pid = fs::read_to_string(&pid_path).expect("helper recorded PID");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("process liveness query runs")
            .success();
        if !alive {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "running helper remains alive after disconnect"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !completed_path.exists(),
        "disconnect allowed helper to pass its running barrier"
    );

    if ownership_loss {
        child.kill().expect("frontend terminates");
        child.wait().expect("frontend reaps");
        drop(input);
        drop(output);
        let daemon_output = fixture.wait_for_daemon();
        let diagnostics = String::from_utf8(daemon_output.stderr).unwrap();
        assert!(
            diagnostics.contains("database.singleton_ownership.lost"),
            "{diagnostics}"
        );
        fs::remove_dir_all(&fixture.root).unwrap();
        fs::remove_dir_all(root).unwrap();
        return;
    }
    drop(input);
    drop(output);
    let mut client = fixture.database.connect();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = client
            .query_one("SELECT state FROM request_attachments", &[])
            .expect("attachment state reads")
            .get::<_, String>(0);
        if state == "detached" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "backpressured disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let mut frontend_stderr = String::new();
    fixture
        .frontend
        .stderr
        .take()
        .expect("frontend stderr")
        .read_to_string(&mut frontend_stderr)
        .expect("frontend stderr reads");
    let daemon_output = fixture.wait_for_daemon();
    assert!(
        !frontend_stderr.contains("telchar-hostile-log-secret"),
        "{frontend_stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&daemon_output.stderr).contains("telchar-hostile-log-secret"),
        "{:?}",
        daemon_output.stderr
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}
