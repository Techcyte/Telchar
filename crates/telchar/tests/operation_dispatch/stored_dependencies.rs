//! Exercises cold stored dependency execution through admitted Telchar requests.

use super::*;

#[test]
fn cold_stored_dependencies_use_telchar_execution() {
    stored_dependency_build(false, false);
}

#[test]
fn failed_stored_dependency_stops_root_and_preserves_session() {
    stored_dependency_build(true, false);
}

#[test]
fn prerequisite_survives_output_expiry_and_gc_before_consumer_admission() {
    stored_dependency_build(false, true);
}

fn stored_dependency_build(fail: bool, collect: bool) {
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
    write_integer(&mut input, 46);
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
    assert_eq!(read_integer(&mut output), 1);
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
