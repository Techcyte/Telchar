use std::process::Command;

use nix_worker_protocol::WorkerOperation;
use telchar::nix_fixture::{NixFixture, TrustMode};
use telchar::worker_trace::TraceCapture;

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
