//! Tests connection-bound cancellation and child reaping.

use super::*;

#[test]
fn disconnected_frontend_cancels_and_reaps_silent_build_helper() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-cancel-helper-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("build-helper");
    let pid_path = root.join("pid");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{}'\ncat >/dev/null\nsleep 30\n",
            pid_path.display()
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
    while !pid_path.exists() {
        assert!(Instant::now() < deadline, "helper did not record PID");
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
            "disconnected helper remains alive"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let mut client = fixture.database.connect();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM build_requests", &[])
            .expect("request count reads")
            .get::<_, i64>(0),
        1,
        "requester disconnect discarded the immutable build request"
    );
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
            "requester disconnect left attachment attached"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_released_derivation_lease(&fixture.database, &request_id(&mut client));
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.build_derivation.failed"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}
