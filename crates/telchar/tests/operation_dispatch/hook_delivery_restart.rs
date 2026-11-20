//! Durable hook delivery retries across daemon process loss.

use super::*;

#[test]
fn hook_delivery_preparation_failure_terminalizes_collecting_build() {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("build");
    fs::write(&helper, "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"version\":1,\"success\":true,\"status\":\"built\",\"outputs\":[[\"out\",\"/nix/store/11111111111111111111111111111111-telchar-build-derivation-contract\"]]}'\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let config = directory.path().join("service.toml");
    fs::write(&config, "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n[post_build_hook]\nexecutable = \"/bin/true\"\n").unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_TEST_BUILD_HELPER", helper.display().to_string()),
            ("TELCHAR_CONFIG", config.display().to_string()),
        ],
    );
    fixture.database.connect().batch_execute(
        "CREATE FUNCTION reject_hook_delivery_roots() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected hook_delivery retention failure'; END $$;
         CREATE TRIGGER reject_hook_delivery_roots BEFORE INSERT ON post_build_hook_roots FOR EACH STATEMENT EXECUTE FUNCTION reject_hook_delivery_roots();"
    ).unwrap();
    let mut input = fixture.frontend.stdin.take().unwrap();
    let mut output = fixture.frontend.stdout.take().unwrap();
    complete_handshake(&mut input, &mut output);
    write_build_derivation_request(&mut input, "x86_64-linux", 0);
    input.flush().unwrap();
    assert_eq!(read_integer(&mut output), STDERR_ERROR);
    assert_eq!(read_string(&mut output), "Error");
    read_integer(&mut output);
    assert_eq!(read_string(&mut output), "Error");
    let diagnostic = read_string(&mut output);
    assert!(!diagnostic.is_empty());
    assert_eq!(read_integer(&mut output), 0);
    assert_eq!(read_integer(&mut output), 0);
    drop(input);
    drop(output);
    assert!(fixture.frontend.wait().unwrap().success());
    let derivation =
        "/nix/store/00000000000000000000000000000000-telchar-build-derivation-contract.drv";
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.database.url(), derivation)
            .unwrap()
            .unwrap()
            .state,
        telchar::persistence::SharedBuildState::Failed
    );
    assert_eq!(
        fixture
            .database
            .connect()
            .query_one("SELECT count(*) FROM post_build_hooks_outbox", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let stderr = fixture.finish();
    assert!(stderr.contains("gateway-store-retention"), "{stderr}");
}

#[test]
fn failed_hook_delivery_resumes_after_daemon_is_killed() {
    hook_delivery_after_restart(false);
}

#[test]
fn disabling_hook_on_restart_releases_pending_hook_delivery() {
    hook_delivery_after_restart(true);
}

fn hook_delivery_after_restart(disable_hook: bool) {
    use telchar::{backend::BackendKind, persistence as db};
    let directory = tempfile::tempdir().unwrap();
    let attempted = directory.path().join("attempted");
    let published = directory.path().join("published");
    let hook = directory.path().join("publish");
    fs::write(&hook, format!("#!/bin/sh\nif [ ! -e '{}' ]; then cat >/dev/null; touch '{}'; exit 1; fi\ncat > '{}'\n", attempted.display(), attempted.display(), published.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let config = directory.path().join("service.toml");
    fs::write(&config, format!("[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n[post_build_hook]\nexecutable = \"{}\"\ntimeout_seconds = 5\ninitial_backoff_seconds = 2\n", hook.display())).unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///fixed-gateway.sock",
        [
            ("TELCHAR_TEST_BUILD_HELPER", "/bin/false".to_owned()),
            ("TELCHAR_CONFIG", config.display().to_string()),
        ],
    );
    let database = db::Database::connect(fixture.database.url())
        .unwrap()
        .with_post_build_hooks(true);
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hook_delivery-restart.drv";
    let output = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hook_delivery-restart";
    db::claim_shared_build(
        &database,
        drv,
        &[1; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[output],
    )
    .unwrap();
    db::start_shared_build(&database, drv).unwrap();
    db::collect_shared_build(&database, drv).unwrap();
    for root in db::prepare_post_build_hook_roots(&database, drv).unwrap() {
        std::os::unix::fs::symlink(
            &root.store_path,
            fixture.root.join("gc-roots").join(root.lease_id()),
        )
        .unwrap();
        db::retain_post_build_hook_root(&database, &root).unwrap();
    }
    db::complete_shared_build_success(
        &database,
        drv,
        &serde_json::json!({"status":"built"}),
        Duration::from_secs(60),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let failed: bool = fixture
            .database
            .connect()
            .query_one(
                "SELECT last_failure_class IS NOT NULL FROM post_build_hooks_outbox",
                &[],
            )
            .unwrap()
            .get(0);
        if failed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hook delivery failure was not persisted"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fixture.daemon.kill().unwrap();
    fixture.daemon.wait().unwrap();
    let failure_log = fs::read_to_string(fixture.root.join("daemon.stderr")).unwrap();
    assert!(
        failure_log.contains("post_build_hooks.attempt_failed"),
        "{failure_log}"
    );
    assert!(!failure_log.contains("ERROR"), "{failure_log}");
    fixture.frontend.kill().unwrap();
    fixture.frontend.wait().unwrap();
    fixture.database.expire_singleton_ownership("daemon");
    if disable_hook {
        fs::write(&config, "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n").unwrap();
    }
    fixture.daemon = Command::new(env!("CARGO_BIN_EXE_telchar"))
        .args([
            "daemon",
            "--socket",
            fixture.root.join("daemon.sock").to_str().unwrap(),
            "--frontend-uid",
            &rustix::process::getuid().as_raw().to_string(),
        ])
        .env("TELCHAR_CONFIG", &config)
        .env("TELCHAR_DATABASE_URL", fixture.database.url())
        .env("TELCHAR_GATEWAY_STORE_URI", "unix:///fixed-gateway.sock")
        .env(
            "TELCHAR_GATEWAY_GC_ROOT_DIRECTORY",
            fixture.root.join("gc-roots"),
        )
        .env("TELCHAR_TEST_STORE_RETENTION", "filesystem-only")
        .env("TELCHAR_TEST_BUILD_HELPER", "/bin/false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(fixture.root.join("daemon.stderr")).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while db::post_build_hooks_backlog(&database).unwrap().0 != 0
        || (disable_hook && fs::read_dir(fixture.root.join("gc-roots")).unwrap().count() != 0)
    {
        assert!(
            fixture.daemon.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(fixture.root.join("daemon.stderr")).unwrap()
        );
        assert!(Instant::now() < deadline, "hook delivery did not resume");
        thread::sleep(Duration::from_millis(10));
    }
    if disable_hook {
        assert!(!published.exists());
    } else {
        let outputs: Vec<String> = serde_json::from_slice(&fs::read(published).unwrap()).unwrap();
        assert_eq!(outputs, [output]);
    }
    assert_eq!(
        db::read_shared_build(&database, drv)
            .unwrap()
            .unwrap()
            .state,
        db::SharedBuildState::Succeeded
    );
    let stderr = fixture.finish();
    assert!(!stderr.contains("ERROR"), "{stderr}");
}
