//! Tests nix fixture contracts and failure boundaries, including dropping fixture removes isolated root without explicit cleanup.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use nix_worker_protocol::{WorkerOperation, WorkerTrust, LATEST_WORKER_VERSION};
use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::fixture::worker_trace::TraceCapture;

#[test]
fn dropping_fixture_removes_isolated_root_without_explicit_cleanup() {
    let fixture = NixFixture::create().expect("fixture creates");
    let root = fixture.root().to_path_buf();

    drop(fixture);

    assert!(!root.exists(), "fixture root leaked after drop: {root:?}");
}

#[test]
fn dropping_running_daemon_and_fixture_cleans_process_and_store() {
    let fixture = NixFixture::create().expect("fixture creates");
    let root = fixture.root().to_path_buf();
    let _daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");

    drop(_daemon);
    drop(fixture);

    assert!(
        !root.exists(),
        "fixture root leaked after daemon drop: {root:?}"
    );
}

#[test]
fn killed_fixture_owner_cleans_process_and_store() {
    const CHILD_ENV: &str = "TELCHAR_NIX_FIXTURE_KILL_CHILD";
    const ROOT_ENV: &str = "TELCHAR_NIX_FIXTURE_ROOT_EVIDENCE";
    if std::env::var_os(CHILD_ENV).is_some() {
        let fixture = NixFixture::create().expect("fixture creates");
        let daemon = fixture
            .start_daemon(TrustMode::Trusted)
            .expect("fixture daemon starts");
        std::fs::write(
            std::env::var_os(ROOT_ENV).expect("root evidence path"),
            fixture.root().as_os_str().as_encoded_bytes(),
        )
        .expect("root evidence writes");
        std::hint::black_box((&fixture, &daemon));
        thread::sleep(Duration::from_secs(60));
        return;
    }

    let evidence = std::env::temp_dir().join(format!(
        "telchar-nix-fixture-kill-evidence-{}",
        std::process::id()
    ));
    let mut child = Command::new(std::env::current_exe().expect("test executable path"))
        .args(["killed_fixture_owner_cleans_process_and_store", "--exact"])
        .env(CHILD_ENV, "1")
        .env("TELCHAR_NIX_FIXTURE_SKIP_PROCESS_LOCK", "1")
        .env(ROOT_ENV, &evidence)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("fixture owner starts");
    for _ in 0..500 {
        if evidence.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let root = std::path::PathBuf::from(
        std::fs::read_to_string(&evidence).expect("fixture root evidence reads"),
    );
    child.kill().expect("fixture owner killed");
    child.wait().expect("fixture owner reaped");
    for _ in 0..500 {
        if !root.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!root.exists(), "killed fixture owner leaked root: {root:?}");
    let _ = std::fs::remove_file(evidence);
}

#[test]
fn creates_isolated_real_nix_client_state_and_removes_it() {
    let fixture = NixFixture::create().expect("fixture creates");
    let root = fixture.root().to_path_buf();

    assert!(fixture.config_path().is_file());
    assert!(fixture.private_key_path().is_file());
    assert!(fixture.public_key_path().is_file());
    let key = Command::new("ssh-keygen")
        .args(["-l", "-f"])
        .arg(fixture.public_key_path())
        .output()
        .expect("SSH key validator runs");
    assert!(key.status.success(), "fixture key invalid: {key:?}");
    assert!(fixture.state_dir().is_dir());
    assert!(fixture.temp_dir().is_dir());

    let output = Command::new("nix")
        .arg("show-config")
        .envs(fixture.environment())
        .output()
        .expect("real Nix client runs");
    assert!(output.status.success(), "Nix failed: {output:?}");
    let configuration = String::from_utf8_lossy(&output.stdout);
    assert!(
        configuration.contains("warn-dirty = false"),
        "fixture configuration was not used: {configuration}"
    );

    fixture.cleanup().expect("fixture cleans up");
    assert!(!root.exists(), "fixture root leaked: {root:?}");
}

#[test]
fn reusable_client_negotiates_with_real_private_daemon() {
    for (mode, expected_trust) in [
        (TrustMode::Trusted, WorkerTrust::Trusted),
        (TrustMode::Untrusted, WorkerTrust::Untrusted),
    ] {
        let fixture = NixFixture::create().expect("fixture creates");
        let daemon = fixture.start_daemon(mode).expect("fixture daemon starts");

        let profile = daemon
            .worker_client_profile()
            .expect("worker client negotiates with private daemon");

        assert_eq!(profile.version, LATEST_WORKER_VERSION);
        assert_eq!(profile.trust, expected_trust);
        assert!(profile.capabilities.root_registration);
    }
}

#[test]
fn fixture_owned_daemon_reports_explicit_trust_modes_without_host_store_access() {
    for (mode, expected_trust) in [(TrustMode::Trusted, true), (TrustMode::Untrusted, false)] {
        let fixture = NixFixture::create().expect("fixture creates");
        let mut daemon = fixture.start_daemon(mode).expect("fixture daemon starts");

        assert_eq!(
            daemon.trusted().expect("daemon reports trust"),
            expected_trust
        );
        assert!(daemon.store_url().starts_with("unix://"));
        assert!(daemon.socket_path().starts_with(fixture.root()));
        assert!(fixture.store_dir().starts_with(fixture.root()));
        assert_ne!(fixture.store_dir(), std::path::Path::new("/nix/store"));

        daemon.stop().expect("fixture daemon stops");
        fixture.cleanup().expect("fixture cleans up");
    }
}

#[test]
fn fixture_owned_daemon_builds_the_fixed_classic_derivation_in_both_trust_modes() {
    for (mode, expected_trust) in [(TrustMode::Trusted, true), (TrustMode::Untrusted, false)] {
        let fixture = NixFixture::create().expect("fixture creates");
        let mut daemon = fixture.start_daemon(mode).expect("fixture daemon starts");

        assert_eq!(
            daemon.trusted().expect("daemon reports trust"),
            expected_trust
        );
        let output = daemon
            .build_classic_derivation()
            .expect("fixture daemon builds classic derivation");
        assert!(output.starts_with(fixture.store_dir()));
        assert_eq!(
            std::fs::read(&output).expect("fixture output reads"),
            b"telchar-classic-fixture"
        );

        daemon.stop().expect("fixture daemon stops");
        fixture.cleanup().expect("fixture cleans up");
    }
}

#[test]
fn real_store_query_reports_valid_and_invalid_paths() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let valid = daemon
        .build_classic_derivation()
        .expect("fixture daemon builds classic derivation");
    let invalid = fixture
        .store_dir()
        .join("00000000000000000000000000000000-missing");

    assert!(daemon.is_valid_path(&valid).expect("valid path query"));
    assert!(!daemon.is_valid_path(&invalid).expect("invalid path query"));

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn real_garbage_collection_removes_unrooted_private_store_path() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let path = daemon
        .build_classic_derivation()
        .expect("fixture daemon builds classic derivation");

    assert!(daemon.is_valid_path(&path).expect("path valid before GC"));
    daemon.collect_garbage().expect("fixture garbage collects");
    assert!(
        !daemon.is_valid_path(&path).expect("path validity after GC"),
        "unrooted path survived private-store GC: {path:?}"
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn real_store_validity_query_does_not_hide_daemon_failure() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let path = daemon
        .build_classic_derivation()
        .expect("fixture daemon builds classic derivation");
    daemon.stop().expect("fixture daemon stops");

    let error = daemon
        .is_valid_path(&path)
        .expect_err("daemon failure must not become invalid-path result");
    assert!(
        error.to_string().contains("path-info query failed"),
        "unexpected store failure: {error}"
    );

    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn real_store_import_registers_valid_nar_and_export_streams_it() {
    let source_fixture = NixFixture::create().expect("source fixture creates");
    let mut source_daemon = source_fixture
        .start_daemon(TrustMode::Trusted)
        .expect("source daemon starts");
    let path = source_daemon
        .build_classic_derivation()
        .expect("source path builds");
    let mut body = Vec::new();
    let exported = source_daemon
        .export_path(&path, &mut body)
        .expect("path exports");
    assert_eq!(exported.path, path);
    assert_eq!(exported.info.nar_size, 136);
    assert!(!body.is_empty());

    source_daemon
        .delete_path(&path)
        .expect("source path deletes before import");
    source_daemon
        .import_nar(body.as_slice())
        .expect("valid NAR imports");
    assert!(source_daemon
        .is_valid_path(&path)
        .expect("imported path query"));
    let imported = source_daemon
        .query_path_info(&path)
        .expect("imported metadata query");
    assert_eq!(imported, exported.info);

    source_daemon.stop().expect("source daemon stops");
    source_fixture.cleanup().expect("source fixture cleans");
}

#[test]
fn legacy_import_rejects_structurally_corrupt_export() {
    let source_fixture = NixFixture::create().expect("source fixture creates");
    let mut source_daemon = source_fixture
        .start_daemon(TrustMode::Trusted)
        .expect("source daemon starts");
    let path = source_daemon
        .build_classic_derivation()
        .expect("source path builds");
    let mut exported = Vec::new();
    source_daemon
        .export_path(&path, &mut exported)
        .expect("path exports");
    let index = exported.len() / 2;
    exported[index] ^= 0xff;

    source_daemon
        .delete_path(&path)
        .expect("source path deletes before corrupt import");
    let error = source_daemon
        .import_nar(exported.as_slice())
        .expect_err("structurally corrupt export must be rejected");
    assert!(error.to_string().contains("import"));
    assert!(!source_daemon
        .is_valid_path(&path)
        .expect("corrupt path query"));

    source_daemon.stop().expect("source daemon stops");
    source_fixture.cleanup().expect("source fixture cleans");
}

#[test]
fn real_store_query_returns_required_path_metadata() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let path = daemon
        .build_classic_derivation()
        .expect("fixture daemon builds classic derivation");

    let info = daemon.query_path_info(&path).expect("path metadata query");
    assert_eq!(info.path, path);
    assert_eq!(info.nar_size, 136);
    assert!(info.nar_hash.starts_with("sha256-"));
    assert!(info.references.is_empty());
    assert!(info.deriver.is_some());
    assert!(info.content_address.is_none());

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn relay_streams_the_complete_classic_build_fixture_in_both_trust_modes() {
    for (mode, expected_trust) in [(TrustMode::Trusted, true), (TrustMode::Untrusted, false)] {
        let fixture = NixFixture::create().expect("fixture creates");
        let mut daemon = fixture.start_daemon(mode).expect("fixture daemon starts");
        assert_eq!(
            daemon.trusted().expect("daemon reports trust"),
            expected_trust
        );

        let capture =
            TraceCapture::start(daemon.socket_path().to_str().expect("UTF-8 socket path"))
                .expect("capture starts");
        let output = Command::new("nix")
            .envs(fixture.environment())
            .args([
                "--store",
                &capture.store_url(),
                "build",
                "--impure",
                "--expr",
                "derivation { name = \"telchar-classic-fixture\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf telchar-classic-fixture > \\\"$out\\\"\" ]; }",
                "--no-link",
                "--print-out-paths",
            ])
            .output()
            .expect("stock Nix client runs through relay");
        assert!(output.status.success(), "Nix client failed: {output:?}");

        let trace = capture.finish().expect("capture finishes");
        assert_eq!(
            trace.operations(),
            &[
                WorkerOperation::SetOptions,
                WorkerOperation::AddTempRoot,
                WorkerOperation::IsValidPath,
                WorkerOperation::AddToStore,
                WorkerOperation::QueryMissing,
                WorkerOperation::QueryPathInfo,
                WorkerOperation::BuildPathsWithResults,
            ]
        );
        assert!(!trace.contains_payloads());
        assert_eq!(
            trace.sanitized_json(),
            format!(
                "{{\"client_protocol\":\"1.38\",\"operations\":[SetOptions, AddTempRoot, IsValidPath, AddToStore, QueryMissing, QueryPathInfo, BuildPathsWithResults],\"peer_protocol\":\"1.38\",\"trusted\":{expected_trust}}}"
            )
        );
        assert_eq!(
            std::fs::read(
                String::from_utf8(output.stdout)
                    .expect("output UTF-8")
                    .trim()
            )
            .expect("fixture output reads"),
            b"telchar-classic-fixture"
        );

        daemon.stop().expect("fixture daemon stops");
        fixture.cleanup().expect("fixture cleans up");
    }
}

#[test]
fn classic_build_diagnostics_repeat_sanitized_worker_operation_candidates() {
    for mode in [TrustMode::Trusted, TrustMode::Untrusted] {
        let first = diagnostic_operations(mode);
        let second = diagnostic_operations(mode);

        assert_eq!(first, second);
        assert_eq!(
            first,
            vec![19, 11, 1, 7, 40, 26, 46],
            "diagnostic classifications changed"
        );
    }
}

fn diagnostic_operations(mode: TrustMode) -> Vec<u64> {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_diagnostic_daemon(mode)
        .expect("diagnostic daemon starts");
    daemon
        .build_classic_derivation()
        .expect("diagnostic fixture build succeeds");
    let operations = daemon
        .diagnostic_operations()
        .expect("diagnostic operation classifications read");
    daemon.stop().expect("diagnostic daemon stops");
    fixture.cleanup().expect("fixture cleans up");
    operations
}
