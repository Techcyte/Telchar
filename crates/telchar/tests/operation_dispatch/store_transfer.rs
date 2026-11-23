//! Tests store query and AddMultipleToStore transfer dispatch.

use super::*;

#[test]
fn stock_client_dependency_build_reuses_success_through_worker_admission() {
    let expression = "let dep = derivation { name = \"telchar-cache-dependency\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"echo dependency > $out\" ]; }; in derivation { name = \"telchar-cache-root\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"echo ${dep} > $out\" ]; }";
    let instantiated = Command::new("nix-instantiate")
        .args(["--store", "local", "--expr", expression])
        .output()
        .unwrap();
    assert!(instantiated.status.success(), "{instantiated:?}");
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mut fixture =
        FrontendFixture::spawn_multi_with_store("unix:///nix/var/nix/daemon-socket/socket", []);
    let ssh = fixture.root.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nexec \"$TELCHAR_STDIO_BIN\" serve-stdio\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let client = Command::new("nix")
        .args([
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
            format!(
                "{}:{}",
                fixture.root.display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("TELCHAR_STDIO_BIN", env!("CARGO_BIN_EXE_telchar"))
        .env("TELCHAR_IPC_SOCKET", fixture.root.join("daemon.sock"))
        .env("TELCHAR_AUTHENTICATED_KEY", "SHA256:fixture")
        .output()
        .unwrap();
    assert!(client.status.success(), "{client:?}");
    let durable = telchar::persistence::read_shared_build(fixture.database.url(), &drv)
        .unwrap()
        .unwrap();
    assert_eq!(
        durable.state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    let backend = telchar::backend::BackendTarget::new(
        "local",
        telchar::backend::BackendKind::Local,
        "x86_64-linux",
        [] as [&str; 0],
    )
    .unwrap();
    let request = telchar::build::BuildRequest::from_stored_derivation(
        drv.as_bytes(),
        &fs::read(&drv).unwrap(),
        &[backend],
    )
    .unwrap();
    let json = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "derivation",
            "show",
            &drv,
        ])
        .output()
        .unwrap();
    assert!(json.status.success(), "{json:?}");
    let shown: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let root = &shown["derivations"][std::path::Path::new(&drv)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()];
    let mut wire = Vec::new();
    write_integer(&mut wire, 36);
    write_string(&mut wire, drv.as_bytes());
    write_integer(&mut wire, request.output_authorities().len() as u64);
    for output in request.output_authorities() {
        for value in [
            output.name(),
            output.path(),
            output.hash_algorithm(),
            output.hash(),
        ] {
            write_string(&mut wire, value);
        }
    }
    let mut sources = root["inputs"]["srcs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    for (dependency, selected) in root["inputs"]["drvs"].as_object().unwrap() {
        let dependency_path = format!("/nix/store/{dependency}");
        let output = Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "derivation",
                "show",
                &dependency_path,
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let dependency_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        for name in selected["outputs"].as_array().unwrap() {
            sources.insert(format!("/nix/store/{}", dependency_json["derivations"][dependency]["outputs"][name.as_str().unwrap()]["path"].as_str().unwrap()));
        }
    }
    write_integer(&mut wire, sources.len() as u64);
    for source in sources {
        write_string(&mut wire, source.as_bytes());
    }
    write_string(&mut wire, request.system().as_bytes());
    write_string(&mut wire, request.builder());
    write_integer(&mut wire, request.arguments().len() as u64);
    for argument in request.arguments() {
        write_string(&mut wire, argument);
    }
    write_integer(&mut wire, request.environment().len() as u64);
    for (key, value) in request.environment() {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    input.write_all(&wire).unwrap();
    input.flush().unwrap();
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(
        read_integer(&mut output),
        2,
        "worker admission reuses durable success"
    );
    assert_eq!(read_string(&mut output), "");
    for _ in 0..6 {
        read_integer(&mut output);
    }
    assert_eq!(read_integer(&mut output), 1);
    let output_id = read_string(&mut output);
    let realization: serde_json::Value = serde_json::from_str(&read_string(&mut output)).unwrap();
    assert_eq!(realization["id"], output_id);
    let expected_path = std::str::from_utf8(&request.expected_outputs()[0].1).unwrap();
    assert_eq!(
        realization["outPath"],
        expected_path.strip_prefix("/nix/store/").unwrap()
    );
    drop(input);
    drop(output);
    assert!(fixture.frontend.wait().unwrap().success());
    fixture.finish();
}

#[test]
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn query_valid_paths_returns_only_authoritative_valid_paths() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("Nix daemon starts");
    let valid = store
        .build_classic_derivation()
        .expect("authoritative fixture path builds");
    let invalid = fixture
        .store_dir()
        .join("00000000000000000000000000000000-missing");
    let mut frontend =
        FrontendFixture::spawn_with_store(None, &store.store_url(), fixture.environment());
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 2);
    write_string(&mut input, valid.as_os_str().as_encoded_bytes());
    write_string(&mut input, invalid.as_os_str().as_encoded_bytes());
    write_integer(&mut input, 0);
    input.flush().expect("QueryValidPaths request flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 1, "one requested path is valid");
    assert_eq!(read_string(&mut output), valid.to_string_lossy());
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_valid_paths.completed"),
        "missing completion telemetry: {stderr}"
    );

    store.stop().expect("Nix daemon stops");
    fixture.cleanup().expect("Nix fixture cleans");
}

#[test]
fn build_paths_with_results_reports_plain_valid_store_path_as_already_valid() {
    let name = format!("telchar-valid-output-{}", std::process::id());
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {name} > \\\"$out\\\"\" ]; }}"
    );
    let derivation = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .expect("host-store derivation creates");
    assert!(
        derivation.status.success(),
        "nix-instantiate failed: {}",
        String::from_utf8_lossy(&derivation.stderr)
    );
    let derivation = String::from_utf8(derivation.stdout).expect("derivation path is UTF-8");
    let output = Command::new("nix-store")
        .args(["--realise", derivation.trim()])
        .output()
        .expect("host-store output realises");
    assert!(
        output.status.success(),
        "nix-store --realise failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output_path = String::from_utf8(output.stdout).expect("output path is UTF-8");
    let output_path = output_path.trim();

    let mut frontend =
        FrontendFixture::spawn_with_store(None, "unix:///nix/var/nix/daemon-socket/socket", []);
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 46);
    write_integer(&mut input, 1);
    write_string(&mut input, output_path.as_bytes());
    write_integer(&mut input, 0);
    input
        .flush()
        .expect("BuildPathsWithResults request flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 1, "one keyed result");
    assert_eq!(read_string(&mut output), output_path);
    assert_eq!(read_integer(&mut output), 2, "AlreadyValid status");
    assert_eq!(read_string(&mut output), "");
    for _ in 0..7 {
        assert_eq!(read_integer(&mut output), 0);
    }

    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        !stderr.contains("gateway.stored_build.failed"),
        "valid output was treated as a derivation: {stderr}"
    );
}

#[test]
fn real_stock_nix_build_reaches_production_dispatch_without_unsupported_operations() {
    let name = format!("telchar-real-workload-{}", std::process::id());
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {name} > \\\"$out\\\"\" ]; }}"
    );
    let derivation = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .expect("host-store derivation creates");
    assert!(derivation.status.success());
    let derivation = String::from_utf8(derivation.stdout).unwrap();
    let derivation = derivation.trim();
    let config = tempfile::NamedTempFile::new().expect("native backend configuration creates");
    fs::write(
        config.path(),
        format!(
            "[backends.local]\nname = \"local\"\nsystem = \"{}-{}\"\nmaximum_concurrent_builds = 1\n",
            std::env::consts::ARCH,
            std::env::consts::OS,
        ),
    )
    .expect("native backend configuration writes");
    let mut frontend = FrontendFixture::spawn_with_store_policy_and_mode(
        Some(5_000),
        "unix:///nix/var/nix/daemon-socket/socket",
        [("TELCHAR_CONFIG", config.path().display().to_string())],
        Some("cancel-running"),
        false,
    );
    let mut initial_input = frontend.frontend.stdin.take().unwrap();
    let mut initial_output = frontend.frontend.stdout.take().unwrap();
    complete_handshake(&mut initial_input, &mut initial_output);
    drop(initial_input);
    drop(initial_output);
    assert!(frontend.frontend.wait().unwrap().success());
    let mut client = frontend.spawn_frontend();
    let mut input = client.stdin.take().expect("frontend input");
    let mut output = client.stdout.take().expect("frontend output");
    let exchange = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        complete_handshake(&mut input, &mut output);
        write_integer(&mut input, 40);
        write_integer(&mut input, 1);
        write_string(&mut input, format!("{derivation}!*").as_bytes());
        input.flush().expect("QueryMissing request flushes");

        assert_eq!(read_integer(&mut output), STDERR_LAST);
        let will_build = read_integer(&mut output);
        for _ in 0..will_build {
            let _ = read_string(&mut output);
        }
        assert_eq!(read_integer(&mut output), 0);
        assert_eq!(read_integer(&mut output), 0);
        assert_eq!(read_integer(&mut output), 0);
        assert_eq!(read_integer(&mut output), 0);
    }));
    if let Err(failure) = exchange {
        drop(input);
        drop(output);
        let status = client.try_wait().unwrap();
        let _ = client.kill();
        let diagnostics = client.wait_with_output().unwrap();
        let daemon_diagnostics = frontend.finish();
        eprintln!(
            "STOCK_CLIENT_FAILURE status={status:?} frontend={} daemon={daemon_diagnostics}",
            String::from_utf8_lossy(&diagnostics.stderr)
        );
        std::panic::resume_unwind(failure);
    }
    write_integer(&mut input, 46);
    write_integer(&mut input, 1);
    write_string(&mut input, format!("{derivation}!*").as_bytes());
    write_integer(&mut input, 0);
    input
        .flush()
        .expect("BuildPathsWithResults request flushes");
    drop(output);
    let deadline = Instant::now() + Duration::from_secs(10);
    while telchar::persistence::read_shared_build(frontend.database.url(), derivation)
        .expect("shared build reads")
        .is_none()
    {
        assert!(Instant::now() < deadline, "real workload was not admitted");
        thread::sleep(Duration::from_millis(5));
    }
    drop(input);
    let _ = client.kill();
    let _ = client.wait();
    let stderr = frontend.finish();
    assert!(
        !stderr.contains("recognized-unimplemented")
            && !stderr.contains("unsupported worker operation"),
        "real stock Nix operation sequence was rejected: {stderr}"
    );
}

#[test]
fn query_missing_reports_uncached_derivation_as_buildable() {
    let name = format!("telchar-query-missing-{}", std::process::id());
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {name} > \\\"$out\\\"\" ]; }}"
    );
    let derivation = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .unwrap_or_else(|error| panic!("host-store fixture derivation failed to run: {error}"));
    assert!(
        derivation.status.success(),
        "nix-instantiate failed: {}",
        String::from_utf8_lossy(&derivation.stderr)
    );
    let derivation = std::str::from_utf8(&derivation.stdout)
        .unwrap_or_else(|error| panic!("derivation path is not UTF-8: {error}"))
        .trim();
    assert!(derivation.starts_with("/nix/store/") && !derivation.contains('\n'));
    let derivation = PathBuf::from(derivation);
    let mut frontend =
        FrontendFixture::spawn_with_store(None, "unix:///nix/var/nix/daemon-socket/socket", []);
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 40);
    write_integer(&mut input, 1);
    let target = format!("{}!*", derivation.display());
    write_string(&mut input, target.as_bytes());
    input.flush().expect("QueryMissing request flushes");

    let frame = read_integer(&mut output);
    if frame != STDERR_LAST {
        drop(input);
        let _ = child.wait();
        panic!(
            "unexpected QueryMissing frame {frame:#x}: {}",
            frontend.finish()
        );
    }
    assert_eq!(read_integer(&mut output), 1, "derivation needs building");
    assert_eq!(read_string(&mut output), derivation.to_string_lossy());
    assert_eq!(read_integer(&mut output), 0, "nothing will substitute");
    assert_eq!(read_integer(&mut output), 0, "target is known");
    assert_eq!(read_integer(&mut output), 0, "no download bytes");
    assert_eq!(read_integer(&mut output), 0, "no substitute NAR bytes");
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_missing.completed"),
        "missing completion telemetry: {stderr}"
    );
}

#[test]
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn query_path_info_returns_authoritative_metadata() {
    let fixture = NixFixture::create().expect("Nix fixture creates");
    let mut store = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("Nix daemon starts");
    let valid = store
        .build_classic_derivation()
        .expect("authoritative fixture path builds");
    let mut environment = fixture.environment();
    environment.insert("RUST_LOG", "debug".to_owned());
    let mut frontend = FrontendFixture::spawn_with_store(None, &store.store_url(), environment);
    let child = &mut frontend.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 26);
    write_string(&mut input, valid.as_os_str().as_encoded_bytes());
    input.flush().expect("QueryPathInfo flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(read_integer(&mut output), 1);
    let _deriver = read_string(&mut output);
    assert_eq!(read_string(&mut output).len(), 64);
    let reference_count = read_integer(&mut output);
    for _ in 0..reference_count {
        let _ = read_string(&mut output);
    }
    let _registration_time = read_integer(&mut output);
    assert!(read_integer(&mut output) > 0);
    let _ultimate = read_integer(&mut output);
    let signature_count = read_integer(&mut output);
    for _ in 0..signature_count {
        let _ = read_string(&mut output);
    }
    let _content_address = read_string(&mut output);

    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = frontend.finish();
    assert!(
        stderr.contains("worker.query_path_info.completed"),
        "{stderr}"
    );
    for event in [
        "store.export.connection.acquired",
        "store.export.query_path_info.completed",
        "worker.query_path_info.response_flushed",
    ] {
        assert!(stderr.contains(event), "missing {event}: {stderr}");
    }
    store.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
fn empty_query_valid_paths_returns_empty_set_without_store_lookup() {
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///definitely/missing/store.sock",
        std::iter::empty::<(&str, String)>(),
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input
        .flush()
        .expect("empty QueryValidPaths request flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(
        read_integer(&mut output),
        0,
        "empty request has empty result"
    );
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.query_valid_paths.completed"),
        "missing completion telemetry: {stderr}"
    );
}

#[test]
fn oversized_query_valid_paths_count_fails_before_reading_path_bodies() {
    let mut fixture = FrontendFixture::spawn(Some(100));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 31);
    write_integer(&mut input, 257);
    input.flush().expect("oversized count flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "invalid QueryValidPaths request");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("invalid-query-valid-paths"),
        "missing bounded rejection evidence: {stderr}"
    );
    assert!(!stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn empty_add_multiple_to_store_completes_and_keeps_session_open() {
    let mut fixture = FrontendFixture::spawn(None);
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 44);
    write_integer(&mut input, 0);
    write_integer(&mut input, 1);
    write_integer(&mut input, 8);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().expect("empty upload batch flushes");

    assert_eq!(read_integer(&mut output), STDERR_LAST);

    write_integer(&mut input, 0xffff);
    input.flush().expect("next operation flushes");
    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_string(&mut output), "unknown worker operation");
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);
    drop(output);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("worker.add_multiple_to_store.completed"),
        "missing empty-batch completion telemetry: {stderr}"
    );
    assert!(
        stderr.contains("rejection=\"unknown-operation\""),
        "{stderr}"
    );
}

#[test]
fn incomplete_nonempty_add_multiple_to_store_fails_before_first_item_body() {
    let mut fixture = FrontendFixture::spawn(Some(100));
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_integer(&mut input, 44);
    write_integer(&mut input, 0);
    write_integer(&mut input, 1);
    write_integer(&mut input, 8);
    write_integer(&mut input, 1);
    input.flush().expect("nonempty upload count flushes");

    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    let _level = read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "invalid AddMultipleToStore request"
    );
    assert_eq!(read_integer(&mut output), 0, "error has no position");
    assert_eq!(read_integer(&mut output), 0, "error has no trace");
    drop(input);

    assert!(child.wait().expect("Telchar exits").success());
    let stderr = fixture.finish();
    assert!(
        stderr.contains("invalid-add-multiple-to-store"),
        "missing fail-before-body evidence: {stderr}"
    );
    assert!(!stderr.contains("worker.session.timed_out"), "{stderr}");
}

#[test]
fn truncated_add_multiple_to_store_reports_promotion_read_failure() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-upload-truncation-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("promote-helper");
    fs::write(&helper, "#!/bin/sh\nexit 1\n").expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let staging_root = root.join("staging");
    fs::create_dir(&staging_root).expect("staging root creates");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_TEST_PROMOTE_HELPER", helper.display().to_string()),
            ("TMPDIR", staging_root.display().to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_add_multiple_to_store_metadata(&mut input, 1024);
    input.write_all(b"partial-nar").expect("partial NAR writes");
    input.flush().expect("partial upload flushes");
    drop(input);
    drop(output);

    assert!(!child.wait().expect("Telchar exits").success());
    let mut frontend_stderr = String::new();
    fixture
        .frontend
        .stderr
        .take()
        .expect("frontend stderr")
        .read_to_string(&mut frontend_stderr)
        .expect("frontend stderr reads");
    let daemon_output = fixture.wait_for_daemon();
    let stderr = format!(
        "{frontend_stderr}{}",
        String::from_utf8_lossy(&daemon_output.stderr)
    );
    assert!(
        stderr.contains("store input promotion failed: failed to fill whole buffer"),
        "{stderr}"
    );
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn partial_add_multiple_to_store_failure_removes_staging_state() {
    let root = std::env::temp_dir().join(format!(
        "telchar-operation-upload-disconnect-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("fixture root creates");
    let helper = root.join("promote-helper");
    let marker = root.join("helper-started");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nset -eu\nprintf invoked > {}\n",
            marker.display()
        ),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let staging_root = root.join("staging");
    fs::create_dir(&staging_root).expect("staging root creates");
    let mut fixture = FrontendFixture::spawn_with_store(
        None,
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_TEST_PROMOTE_HELPER", helper.display().to_string()),
            ("TMPDIR", staging_root.display().to_string()),
        ],
    );
    let child = &mut fixture.frontend;
    let mut input = child.stdin.take().expect("server input");
    let mut output = child.stdout.take().expect("server output");
    complete_handshake(&mut input, &mut output);

    write_add_multiple_to_store_metadata(&mut input, 1024);
    input.write_all(b"partial-nar").expect("partial NAR writes");
    input.flush().expect("partial upload flushes");
    drop(input);
    drop(output);

    assert!(
        !child.wait().expect("Telchar exits").success(),
        "disconnected partial upload reported protocol success"
    );
    let mut frontend_stderr = String::new();
    fixture
        .frontend
        .stderr
        .take()
        .expect("frontend stderr")
        .read_to_string(&mut frontend_stderr)
        .expect("frontend stderr reads");
    let daemon_output = fixture.wait_for_daemon();
    let stderr = format!(
        "{frontend_stderr}{}",
        String::from_utf8_lossy(&daemon_output.stderr)
    );
    assert!(!marker.exists(), "partial upload started promotion helper");
    assert_eq!(
        fs::read_dir(&staging_root)
            .expect("staging root reads")
            .count(),
        0,
        "partial upload retained staging state"
    );
    assert!(stderr.contains("invalid-add-multiple-to-store"), "{stderr}");
    fs::remove_dir_all(root).expect("fixture cleans");
}
