//! Tests unread frontend backpressure and request cleanup.

use super::*;

#[test]
fn unread_frontend_does_not_block_build_completion() {
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("build-helper");
    let completed = root.path().join("completed");
    fs::write(&helper, format!(
        "#!/bin/sh\nset -eu\ncat >/dev/null\nhead -c 67108864 /dev/zero >&2\ntouch '{}'\nprintf '%s\\n' '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}'\n", completed.display()
    )).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string())],
    );
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
    input.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
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
    if succeeded {
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
        assert_eq!(
            realization["outPath"],
            "11111111111111111111111111111111-telchar-build-derivation-contract"
        );
        drop(input);
        assert!(fixture.frontend.wait().unwrap().success());
    } else {
        fixture.frontend.kill().unwrap();
        fixture.frontend.wait().unwrap();
        drop(input);
    }
    drop(output);
    let diagnostics = fixture.finish();
    assert!(
        succeeded && completed.exists(),
        "unread frontend blocked execution completion: {diagnostics}"
    );
}

#[test]
fn unread_frontend_disconnect_cancels_running_helper_and_cleans_request() {
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
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nprintf started > '{}'\nprintf 'telchar-hostile-log-secret' >&2\nhead -c 67108864 /dev/zero >&2\nwhile :; do sleep 1; done\nprintf completed > '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}}\\n'\n",
            pid_path.display(),
            started_path.display(),
            completed_path.display()
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

    child.kill().expect("frontend terminates");
    child.wait().expect("frontend reaps");
    drop(input);
    drop(output);

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
