//! Exercises cold stored dependency execution through admitted Telchar requests.

use super::*;

#[test]
fn cold_stored_dependencies_use_telchar_execution() {
    stored_dependency_build(false, false);
}

#[test]
fn build_paths_executes_cold_stored_dependencies() {
    stored_dependency_request(false, false, 9);
}

#[test]
fn build_paths_dependency_failure_preserves_session() {
    stored_dependency_request(true, false, 9);
}

#[test]
fn failed_stored_dependency_stops_root_and_preserves_session() {
    stored_dependency_build(true, false);
}

#[test]
fn prerequisite_survives_output_expiry_and_gc_before_consumer_admission() {
    stored_dependency_build(false, true);
}

#[test]
fn worker_and_stored_requests_share_transitive_input_identity() {
    for worker_first in [true, false] {
        let name = if worker_first {
            "worker-first"
        } else {
            "stored-first"
        };
        let expr = format!(
            r#"let leaf = builtins.toFile "identity-leaf-{name}" "leaf"; input = builtins.toFile "identity-input-{name}" leaf; in derivation {{ name = "identity-root-{name}"; system = builtins.currentSystem; builder = "/bin/sh"; inherit input; args = [ "-c" "printf done > $out" ]; }}"#
        );
        let instantiated = Command::new("nix-instantiate")
            .args(["--store", "local", "--expr", &expr])
            .output()
            .unwrap();
        assert!(instantiated.status.success(), "{instantiated:?}");
        let drv = String::from_utf8(instantiated.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let endpoint =
            telchar::store::GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket")
                .unwrap();
        let mut export = telchar::store::export::GatewayStoreExportBackend::new(endpoint);
        let target = telchar::backend::BackendTarget::new(
            "local",
            telchar::backend::BackendKind::Local,
            "x86_64-linux",
            [] as [&str; 0],
        )
        .unwrap();
        let request = telchar::build::BuildRequest::load_stored(
            std::path::Path::new(&drv),
            &mut export,
            &[target],
        )
        .unwrap();
        let closure = Command::new("nix-store")
            .arg("-qR")
            .args(
                request
                    .input_sources()
                    .iter()
                    .map(|p| std::str::from_utf8(p).unwrap()),
            )
            .output()
            .unwrap();
        assert!(closure.status.success(), "{closure:?}");
        let inputs = String::from_utf8(closure.stdout)
            .unwrap()
            .lines()
            .map(|p| p.as_bytes().to_vec())
            .collect::<Vec<_>>();
        assert!(
            inputs.len() > request.input_sources().len(),
            "fixture must contain transitive references"
        );
        let mut worker = serde_json::to_value(&request).unwrap();
        worker["input_sources"] = serde_json::to_value(inputs).unwrap();
        let worker: telchar::build::BuildRequest = serde_json::from_value(worker).unwrap();
        let mut fixture = FrontendFixture::spawn_multi_with_store(
            "unix:///nix/var/nix/daemon-socket/socket",
            [] as [(&str, String); 0],
        );
        let mut input = fixture.frontend.stdin.take().unwrap();
        let mut output = fixture.frontend.stdout.take().unwrap();
        complete_handshake(&mut input, &mut output);
        for keyed in [!worker_first, worker_first] {
            if keyed {
                write_integer(&mut input, 9);
                write_integer(&mut input, 1);
                write_string(&mut input, format!("{drv}!out").as_bytes());
                write_integer(&mut input, 0);
                input.flush().unwrap();
                assert_eq!(read_integer(&mut output), STDERR_LAST);
                assert_eq!(read_integer(&mut output), 1);
            } else {
                super::build_failure::send_failure_build(&mut input, &worker, "", false);
                read_build_status(&mut output, if worker_first { 0 } else { 2 }).unwrap();
            }
        }
        drop(input);
        drop(output);
        assert!(fixture.frontend.wait().unwrap().success());
        let mut restart = Command::new(env!("CARGO_BIN_EXE_telchar"));
        restart
            .args([
                "daemon",
                "--socket",
                fixture.root.join("daemon.sock").to_str().unwrap(),
                "--frontend-uid",
                &rustix::process::getuid().as_raw().to_string(),
            ])
            .env("TELCHAR_CONFIG", fixture.root.join("telchar.toml"))
            .env("TELCHAR_DATABASE_URL", fixture.database.url())
            .env(
                "TELCHAR_GATEWAY_STORE_URI",
                "unix:///nix/var/nix/daemon-socket/socket",
            )
            .env(
                "TELCHAR_GATEWAY_GC_ROOT_DIRECTORY",
                fixture.root.join("gc-roots"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(fixture.root.join("restart.stderr")).unwrap());
        assert!(
            Command::new("kill")
                .args(["-TERM", &fixture.daemon.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(fixture.daemon.wait().unwrap().success());
        drop(export);
        let mut removed = vec![drv.clone()];
        removed.extend(
            worker
                .input_sources()
                .iter()
                .map(|path| std::str::from_utf8(path).unwrap().to_owned()),
        );
        let deleted = Command::new("nix-store")
            .args(["--option", "keep-derivations", "false"])
            .arg("--delete")
            .args(&removed)
            .output()
            .unwrap();
        if !deleted.status.success() {
            let roots = Command::new("nix-store")
                .args(["-q", "--roots"])
                .args(&removed)
                .output()
                .unwrap();
            let refs = Command::new("nix-store")
                .args(["-q", "--referrers"])
                .args(&removed)
                .output()
                .unwrap();
            panic!("{deleted:?}; roots={roots:?}; refs={refs:?}");
        }
        for path in &removed {
            assert!(
                !std::path::Path::new(path).exists(),
                "input remains: {path}"
            );
        }
        fixture.daemon = restart.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fixture.root.join("daemon.sock").exists() {
            assert!(Instant::now() < deadline, "restart socket unavailable");
            thread::sleep(Duration::from_millis(5));
        }
        fixture.frontend = fixture.spawn_frontend();
        let mut input = fixture.frontend.stdin.take().unwrap();
        let mut output = fixture.frontend.stdout.take().unwrap();
        complete_handshake(&mut input, &mut output);
        super::build_failure::send_failure_build(&mut input, &worker, "", false);
        if let Err(error) = read_build_status(&mut output, 2) {
            panic!(
                "{error}; restart: {}",
                fs::read_to_string(fixture.root.join("restart.stderr")).unwrap()
            );
        }
        drop(input);
        drop(output);
        assert!(fixture.frontend.wait().unwrap().success());
        fixture.finish();
    }
}

fn stored_dependency_build(fail: bool, collect: bool) {
    stored_dependency_request(fail, collect, 46);
}

#[test]
fn conflicting_cached_request_returns_protocol_error() {
    let instantiated = Command::new("nix-instantiate")
        .args(["--store", "local", "--expr", r#"let dependency = derivation { name = "telchar-cross-operation-input"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "printf input > $out" ]; }; in derivation { name = "telchar-cross-operation-cache"; system = builtins.currentSystem; builder = "/bin/sh"; inherit dependency; args = [ "-c" "printf cached > $out" ]; }"#])
        .output()
        .unwrap();
    assert!(instantiated.status.success(), "{instantiated:?}");
    let drv = String::from_utf8(instantiated.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let endpoint =
        telchar::store::GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket")
            .unwrap();
    let mut export = telchar::store::export::GatewayStoreExportBackend::new(endpoint);
    let dependencies = Command::new("nix-store")
        .args(["-q", "--references", &drv])
        .output()
        .unwrap();
    assert!(dependencies.status.success(), "{dependencies:?}");
    let dependency_paths = String::from_utf8(dependencies.stdout).unwrap();
    for path in dependency_paths
        .lines()
        .filter(|path| path.ends_with(".drv"))
    {
        let built = Command::new("nix-store")
            .args(["--realise", path, "--builders", ""])
            .output()
            .unwrap();
        assert!(built.status.success(), "{built:?}");
    }
    let target = telchar::backend::BackendTarget::new(
        "local",
        telchar::backend::BackendKind::Local,
        "x86_64-linux",
        [] as [&str; 0],
    )
    .unwrap();
    let request = telchar::build::BuildRequest::load_stored(
        std::path::Path::new(&drv),
        &mut export,
        &[target],
    )
    .unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///nix/var/nix/daemon-socket/socket",
        [] as [(&str, String); 0],
    );
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    super::build_failure::send_failure_build(&mut input, &request, "", false);
    read_build_success(&mut output).unwrap();
    let mut persisted = serde_json::to_value(&request).unwrap();
    for path in dependency_paths
        .lines()
        .filter(|path| path.ends_with(".drv"))
    {
        persisted["input_sources"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(path.as_bytes()).unwrap());
    }
    let persisted: telchar::build::BuildRequest = serde_json::from_value(persisted).unwrap();
    let mut database = fixture.database.connect();
    database.execute("UPDATE shared_builds SET build_request=$1::text::jsonb, request_digest=$2 WHERE derivation_path=$3", &[&serde_json::to_string(&persisted).unwrap(), &&persisted.shared_build_digest()[..], &drv]).unwrap();
    super::build_failure::send_failure_build(&mut input, &request, &format!("{drv}!out"), true);
    let mut frame = [0; 8];
    let response = output.read_exact(&mut frame);
    if let Err(error) = response {
        drop(input);
        drop(output);
        let diagnostic = fixture.finish();
        panic!("cached response failed: {error}; daemon diagnostic: {diagnostic}");
    }
    assert_eq!(u64::from_le_bytes(frame), nix_worker_protocol::STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_string(&mut output), "Error");
    assert_eq!(
        read_string(&mut output),
        "cached build metadata could not be validated"
    );
    drop(input);
    drop(output);
    let status = fixture.frontend.wait().unwrap();
    let diagnostic = fixture.finish();
    assert!(status.success(), "{status}: {diagnostic}");
}

fn stored_dependency_request(fail: bool, collect: bool, operation: u64) {
    let root = tempfile::tempdir().unwrap();
    let expression = r#"
      let dependency = derivation {
        name = "telchar-cold-stored-dependency";
        system = builtins.currentSystem;
        builder = "/bin/sh";
        args = [ "-c" "printf dependency > $out" ];
      }; in derivation {
        name = "telchar-cold-stored-root";
        system = builtins.currentSystem;
        builder = "/bin/sh";
        inherit dependency;
        args = [ "-c" "read value < $dependency; printf '%s-root' $value > $out" ];
      }
    "#;
    let expression = if collect {
        let release = root.path().join("release");
        format!(
            r#"let
          dependency = derivation {{ name = "telchar-cold-stored-dependency"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "printf dependency > $out" ]; }};
          candidates = builtins.genList (index: derivation {{ name = "telchar-cold-stored-sibling-${{toString index}}"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "while [ ! -e {} ]; do :; done; printf sibling > $out" ]; }}) 64;
          sibling = builtins.head (builtins.filter (candidate: builtins.lessThan dependency.drvPath candidate.drvPath) candidates);
        in derivation {{ name = "telchar-cold-stored-root"; system = builtins.currentSystem; builder = "/bin/sh"; inherit dependency sibling; args = [ "-c" "read value < $dependency; printf '%s-root' $value > $out" ]; }}"#,
            release.display()
        )
    } else {
        expression.to_owned()
    };
    let dependency_name = if collect {
        "telchar-gc-stored-dependency"
    } else {
        "telchar-cold-stored-dependency"
    };
    let expression = expression.replace("telchar-cold-stored-dependency", dependency_name);
    let expression = if operation == 9 {
        expression.replace("telchar-cold-stored-", "telchar-build-paths-")
    } else {
        expression
    };
    let dependency_name = if operation == 9 {
        "telchar-build-paths-dependency"
    } else {
        dependency_name
    };
    let expression = expression.replace(
        "printf dependency > $out",
        if fail {
            "exit 7"
        } else {
            "printf dependency > $out"
        },
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
    let dependency = Command::new("nix-store")
        .args(["-q", "--references", &drv])
        .output()
        .unwrap();
    assert!(dependency.status.success(), "{dependency:?}");
    let dependency = String::from_utf8(dependency.stdout)
        .unwrap()
        .lines()
        .find(|path| path.ends_with(&format!("-{dependency_name}.drv")))
        .unwrap()
        .to_owned();
    let config = root.path().join("config.toml");
    fs::write(&config, "[scheduling.default]\nmaximum_queued_builds = 16\nmaximum_active_builds = 1\n[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n").unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///nix/var/nix/daemon-socket/socket",
        [("TELCHAR_CONFIG", config.display().to_string())],
    );
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    write_integer(&mut input, operation);
    write_integer(&mut input, 1);
    write_string(&mut input, format!("{drv}!out").as_bytes());
    write_integer(&mut input, 0);
    input.flush().unwrap();
    if collect {
        let sibling = Command::new("nix-store")
            .args(["-qR", &drv])
            .output()
            .unwrap();
        assert!(sibling.status.success(), "{sibling:?}");
        let sibling = String::from_utf8(sibling.stdout)
            .unwrap()
            .lines()
            .find(|path| path.contains("-telchar-cold-stored-sibling-") && path.ends_with(".drv"))
            .unwrap()
            .to_owned();
        wait_for_path_state_for(
            fixture.database.url(),
            &sibling,
            telchar::persistence::SharedBuildState::Running,
            Duration::from_secs(10),
        );
        let mut retention = telchar::store::retention::NixStoreRetentionBackend::new(
            "unix:///nix/var/nix/daemon-socket/socket",
            fixture.root.join("gc-roots"),
        )
        .unwrap();
        telchar::store::retention::reconcile_output_retention(
            fixture.database.url(),
            &mut retention,
            SystemTime::now() + Duration::from_secs(86400),
        )
        .unwrap();
        let output = Command::new("nix-store")
            .args(["-q", "--outputs", &dependency])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let dependency_output = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let gc = Command::new("nix-store")
            .args(["--delete", &dependency_output])
            .output()
            .unwrap();
        assert!(
            !gc.status.success(),
            "graph prerequisite must remain live: {gc:?}"
        );
        let diagnostic = String::from_utf8(gc.stderr).unwrap();
        assert!(diagnostic.contains("is still alive"), "{diagnostic}");
        assert_eq!(fs::read(&dependency_output).unwrap(), b"dependency");
        fs::write(root.path().join("release"), b"release").unwrap();
    }
    while read_integer(&mut output) == nix_worker_protocol::STDERR_NEXT {
        read_string(&mut output);
    }
    if operation == 9 && fail {
        assert_eq!(read_string(&mut output), "Error");
        assert_eq!(read_integer(&mut output), 0);
        assert_eq!(read_string(&mut output), "Error");
        assert!(!read_string(&mut output).is_empty());
        assert_eq!(read_integer(&mut output), 0);
        assert_eq!(read_integer(&mut output), 0);
    } else {
        assert_eq!(read_integer(&mut output), 1);
    }
    if operation == 46 {
        assert_eq!(read_string(&mut output), format!("{drv}!out"));
        assert_eq!(read_integer(&mut output), if fail { 3 } else { 0 });
        let message = read_string(&mut output);
        if fail {
            assert!(!message.is_empty());
        } else {
            assert!(message.is_empty());
        }
        for _ in 0..6 {
            read_integer(&mut output);
        }
        assert_eq!(read_integer(&mut output), 0);
    }
    if !fail {
        write_integer(&mut input, 41);
        write_string(&mut input, drv.as_bytes());
        input.flush().unwrap();
        assert_eq!(read_integer(&mut output), STDERR_LAST);
        assert_eq!(read_integer(&mut output), 1);
        assert_eq!(read_string(&mut output), "out");
        let path = read_string(&mut output);
        assert_eq!(fs::read(&path).unwrap(), b"dependency-root");
        write_integer(&mut input, 10);
        write_string(&mut input, path.as_bytes());
        input.flush().unwrap();
        assert_eq!(read_integer(&mut output), STDERR_LAST);
        assert_eq!(read_integer(&mut output), 1);
    }
    write_integer(&mut input, 31);
    write_integer(&mut input, 0);
    write_integer(&mut input, 0);
    input.flush().unwrap();
    assert_eq!(read_integer(&mut output), STDERR_LAST);
    assert_eq!(
        read_integer(&mut output),
        0,
        "one terminal response per graph"
    );
    // Each dependency must have its own fenced durable execution, not a daemon-only build.
    for path in [&dependency, &drv] {
        let build = telchar::persistence::read_shared_build(fixture.database.url(), path).unwrap();
        if fail && path == &drv {
            assert!(
                build.is_none(),
                "failed prerequisite must prevent parent admission"
            );
            continue;
        }
        let build = build.expect("stored build has a Telchar execution");
        assert_eq!(
            build.state,
            if fail {
                telchar::persistence::SharedBuildState::Failed
            } else {
                telchar::persistence::SharedBuildState::Succeeded
            }
        );
        assert_eq!(build.backend_name, "local");
    }
    drop(input);
    drop(output);
    let stderr = fixture.finish();
    assert!(!stderr.contains("gateway.stored_build.failed"), "{stderr}");
}
