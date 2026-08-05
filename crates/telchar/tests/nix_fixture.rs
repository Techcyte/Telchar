use std::process::Command;

use telchar::nix_fixture::{NixFixture, TrustMode};

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

        assert_eq!(daemon.trusted().expect("daemon reports trust"), expected_trust);
        assert!(daemon.store_url().starts_with("unix://"));
        assert!(daemon.socket_path().starts_with(fixture.root()));
        assert!(fixture.store_dir().starts_with(fixture.root()));
        assert_ne!(fixture.store_dir(), std::path::Path::new("/nix/store"));

        daemon.stop().expect("fixture daemon stops");
        fixture.cleanup().expect("fixture cleans up");
    }
}
