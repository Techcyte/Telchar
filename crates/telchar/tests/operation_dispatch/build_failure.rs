//! Ordinary Nix build failures are results, not broken sessions or reusable successes.
use super::*;

#[test]
fn four_clients_receive_exit_three_logs_and_can_retry_on_the_same_sessions() {
    failed_build_sessions(false);
}

#[test]
fn ordinary_failure_cleanup_fault_has_a_framed_error() {
    failed_build_sessions(true);
}

fn failed_build_sessions(cleanup_fault: bool) {
    // Run under tests/canonical-store.sh: clients and gateway share only this disposable store.
    let root = std::env::temp_dir().join(format!(
        "telchar-failure-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let started = root.join("failure-started");
    let release = root.join("failure-release");
    let sleep = Command::new("sh")
        .args(["-c", "command -v sleep"])
        .output()
        .unwrap();
    assert!(sleep.status.success());
    let sleep = String::from_utf8(sleep.stdout).unwrap().trim().to_owned();
    let expression = format!(
        "derivation {{ name = \"telchar-shared-failure\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"echo x >> {}; while [ ! -e {} ]; do {sleep} 0.01; done; echo telchar-exit-three >&2; exit 3\" ]; }}",
        started.display(),
        release.display(),
    );
    let instantiated = Command::new("nix-instantiate")
        .args(["--store", "local", "--expr", &expression])
        .output()
        .unwrap();
    assert!(instantiated.status.success(), "{instantiated:?}");
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let target = format!("{drv}!*");
    let build = telchar::build::BuildRequest::from_stored_derivation(
        drv.as_bytes(),
        &fs::read(&drv).unwrap(),
        &[telchar::backend::BackendTarget::new(
            "local",
            telchar::backend::BackendKind::Local,
            "x86_64-linux",
            [] as [&str; 0],
        )
        .unwrap()],
    )
    .unwrap();
    let mut fixture =
        FrontendFixture::spawn_multi_with_store("unix:///nix/var/nix/daemon-socket/socket", []);
    let mut children = (0..3).map(|_| fixture.spawn_frontend()).collect::<Vec<_>>();
    let mut clients = Vec::new();
    for child in std::iter::once(&mut fixture.frontend).chain(children.iter_mut()) {
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        complete_handshake(&mut input, &mut output);
        clients.push((input, output));
    }
    for (index, (input, _)) in clients.iter_mut().enumerate() {
        send_failure_build(input, &build, &target, index >= 2);
        if index == 0 {
            wait_for_file_for(&started, Duration::from_secs(10), "leader did not execute");
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let attached = fixture
            .database
            .connect()
            .query_one(
                "SELECT count(*) FROM request_attachments WHERE state = 'attached'",
                &[],
            )
            .unwrap()
            .get::<_, i64>(0);
        if attached == 4 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "only {attached} attached clients"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Attachment precedes joining the registry; wait for all follower notices.
    for (_, output) in clients.iter_mut().skip(1) {
        assert_eq!(read_integer(output), nix_worker_protocol::STDERR_NEXT);
        assert_eq!(read_string(output), "identical build already in progress\n");
    }
    fs::write(&release, "release").unwrap();
    let mut messages = Vec::new();
    for (index, (input, output)) in clients.iter_mut().enumerate() {
        let (message, logs) = read_failure_result(output, (index >= 2).then_some(target.as_str()));
        assert!(message.contains("exit code 3"), "{message}");
        assert!(
            !message.contains("telchar-exit-three"),
            "terminal message must not contain logs: {message}"
        );
        assert!(logs.contains("telchar-exit-three"), "{logs}");
        messages.push(message);
        write_integer(input, 31); // QueryValidPaths after failure, on this connection.
        write_integer(input, 1);
        write_string(input, drv.as_bytes());
        write_integer(input, 0);
        input.flush().unwrap();
        assert_eq!(read_integer(output), STDERR_LAST);
        assert_eq!(read_integer(output), 1);
        assert_eq!(read_string(output), drv);
    }
    assert!(messages.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        fs::read_to_string(&started).unwrap(),
        "x\n",
        "one shared execution"
    );
    let durable = telchar::persistence::read_shared_build(fixture.database.url(), &drv)
        .unwrap()
        .unwrap();
    assert_eq!(
        durable.state,
        telchar::persistence::SharedBuildState::Failed
    );
    let failure = &durable.result_metadata.unwrap()["nix_failure"];
    assert_eq!(failure["status"], 3);
    assert_eq!(failure["message"], messages[0]);
    // A subsequent independent request, on the same session, executes again (no stale wait).
    let (input, output) = &mut clients[0];
    if cleanup_fault {
        fixture.database.connect().batch_execute("CREATE FUNCTION reject_failure_detach() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject detach'; END $$; CREATE TRIGGER reject_failure_detach BEFORE UPDATE ON request_attachments FOR EACH ROW EXECUTE FUNCTION reject_failure_detach();").unwrap();
    }
    send_failure_build(input, &build, &target, true);
    if cleanup_fault {
        loop {
            let frame = read_integer(output);
            if frame == STDERR_ERROR {
                break;
            }
            assert_eq!(frame, nix_worker_protocol::STDERR_NEXT);
            read_string(output);
        }
        assert_eq!(read_string(output), "Error");
        read_integer(output);
        assert_eq!(read_string(output), "Error");
        assert_eq!(read_string(output), "store lease state operation failed");
        assert_eq!(read_integer(output), 0);
        assert_eq!(read_integer(output), 0);
    } else {
        let (message, _) = read_failure_result(output, Some(&target));
        assert!(message.contains("exit code 3"));
    }
    assert_eq!(fs::read_to_string(&started).unwrap(), "x\nx\n");
    drop(clients);
    assert!(fixture.frontend.wait().unwrap().success());
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    fixture.finish();
    fs::remove_dir_all(root).unwrap();
}

fn send_failure_build(
    input: &mut impl Write,
    build: &telchar::build::BuildRequest,
    target: &str,
    keyed: bool,
) {
    if keyed {
        write_integer(input, 46);
        write_integer(input, 1);
        write_string(input, target.as_bytes());
        write_integer(input, 0);
    } else {
        write_integer(input, 36);
        write_string(input, build.derivation_path());
        write_integer(input, build.output_authorities().len() as u64);
        for authority in build.output_authorities() {
            for field in [
                authority.name(),
                authority.path(),
                authority.hash_algorithm(),
                authority.hash(),
            ] {
                write_string(input, field);
            }
        }
        write_integer(input, build.input_sources().len() as u64);
        for path in build.input_sources() {
            write_string(input, path);
        }
        write_string(input, build.system().as_bytes());
        write_string(input, build.builder());
        write_integer(input, build.arguments().len() as u64);
        for argument in build.arguments() {
            write_string(input, argument);
        }
        write_integer(input, build.environment().len() as u64);
        for (key, value) in build.environment() {
            write_string(input, key);
            write_string(input, value);
        }
        write_integer(input, 0);
    }
    input.flush().unwrap();
}

fn read_failure_result(output: &mut impl Read, target: Option<&str>) -> (String, String) {
    let mut logs = String::new();
    loop {
        let frame = read_integer(output);
        if frame == STDERR_LAST {
            break;
        }
        assert_eq!(
            frame,
            nix_worker_protocol::STDERR_NEXT,
            "ordinary failure must not emit STDERR_ERROR"
        );
        logs.push_str(&read_string(output));
    }
    if let Some(target) = target {
        assert_eq!(read_integer(output), 1);
        assert_eq!(read_string(output), target);
    }
    assert_eq!(
        read_integer(output),
        3,
        "Nix PermanentFailure (not process exit code)"
    );
    let message = read_string(output);
    for _ in 0..7 {
        assert_eq!(read_integer(output), 0);
    }
    (message, logs)
}
