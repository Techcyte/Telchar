//! Tests unread frontend backpressure and request cleanup.

use super::*;

#[test]
fn unread_frontend_backpressures_build_logs_and_disconnect_cleans_request() {
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
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nprintf started > '{}'\nprintf 'telchar-hostile-log-secret' >&2\nhead -c 67108864 /dev/zero >&2\nprintf completed > '{}'\nprintf '{{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract\"]]}}\\n'\n",
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
    write_gate_3_build_derivation(&mut input, "x86_64-linux", 0);
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
            "helper completed 64 MiB log production while frontend output was unread"
        );
        assert!(
            child.try_wait().expect("frontend status reads").is_none(),
            "frontend exited instead of applying backpressure"
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
            "backpressured helper remains alive after disconnect"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !completed_path.exists(),
        "disconnect allowed blocked helper to complete log production"
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
    let daemon_output = fixture.daemon.wait_with_output().expect("daemon exits");
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
