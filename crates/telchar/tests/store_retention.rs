use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use telchar::nix_fixture::{NixDaemon, NixFixture, TrustMode};
use telchar::store_retention::{NixStoreRetentionBackend, RetentionEntry, StoreRetentionBackend};

#[test]
fn empty_retention_set_does_not_spawn_helper() {
    let root = std::env::temp_dir().join(format!("telchar-retention-empty-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let helper = root.join("helper");
    let marker = root.join("marker");
    fs::write(
        &helper,
        format!("#!/bin/sh\nprintf invoked > '{}'\n", marker.display()),
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut backend =
        NixStoreRetentionBackend::new(&helper, "unix:///fixed", &root).expect("backend configures");

    assert!(
        backend
            .retain(&[])
            .expect("empty retain succeeds")
            .is_empty()
    );
    assert!(!marker.exists(), "empty retain spawned helper");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn altered_helper_response_is_rejected() {
    let root =
        std::env::temp_dir().join(format!("telchar-retention-response-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .expect("fixture root permissions set");
    let helper = root.join("helper");
    fs::write(
        &helper,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"version\":1,\"retained\":[{\"lease_id\":\"altered\",\"store_path\":\"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a\",\"root_path\":\"/tmp/altered\"}]}'\n",
    )
    .expect("helper writes");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let mut backend =
        NixStoreRetentionBackend::new(&helper, "unix:///fixed", &root).expect("backend configures");

    let error = backend
        .retain(&[RetentionEntry::new(
            "lease-request-1",
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a",
        )])
        .expect_err("altered response rejects");
    assert_eq!(error.to_string(), "gateway store retention failed");
    fs::remove_dir_all(root).expect("fixture cleans");
}

#[test]
fn existing_exact_root_is_idempotent_but_conflicts_do_not_clobber() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "idempotent-leased", "leased");
    let other = build_fixture_path(&fixture, &daemon, "idempotent-other", "other");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let helper = std::env::var_os("TELCHAR_NIX_STORE_RETAIN")
        .expect("TELCHAR_NIX_STORE_RETAIN points to the flake-built helper");
    let root = root_directory.join("lease-idempotent");

    assert!(
        retain_fixture_path(
            &helper,
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-idempotent",
            &leased
        )
        .status
        .success(),
        "initial retain fails"
    );
    assert_eq!(fs::read_link(&root).expect("root target reads"), leased);
    assert!(
        retain_fixture_path(
            &helper,
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-idempotent",
            &leased
        )
        .status
        .success(),
        "same root retain is not idempotent"
    );
    assert_eq!(
        fs::read_link(&root).expect("idempotent root remains"),
        leased
    );

    fs::remove_file(&root).expect("root removes for conflict test");
    std::os::unix::fs::symlink(&other, &root).expect("conflicting symlink creates");
    let response = retain_fixture_path(
        &helper,
        &fixture,
        &daemon.store_url(),
        &root_directory,
        "lease-idempotent",
        &leased,
    );
    assert!(!response.status.success(), "conflicting root succeeds");
    assert_eq!(
        fs::read_link(&root).expect("conflicting target remains"),
        other
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn permanent_root_survives_daemon_restart_and_second_private_gc() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "restart-leased", "restart");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");
    let helper = std::env::var_os("TELCHAR_NIX_STORE_RETAIN")
        .expect("TELCHAR_NIX_STORE_RETAIN points to the flake-built helper");
    let root = root_directory.join("lease-restart");

    assert!(
        retain_fixture_path(
            &helper,
            &fixture,
            &daemon.store_url(),
            &root_directory,
            "lease-restart",
            &leased
        )
        .status
        .success(),
        "initial retain fails"
    );
    daemon.collect_garbage().expect("first private GC succeeds");
    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("first GC preserves lease")
    );
    daemon.stop().expect("fixture daemon stops");

    let mut restarted = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon restarts");
    assert_eq!(fs::read_link(&root).expect("root survives restart"), leased);
    restarted
        .collect_garbage()
        .expect("second private GC succeeds");
    assert!(
        restarted
            .is_valid_path(&leased)
            .expect("second GC preserves lease")
    );
    assert_eq!(fs::read(&leased).expect("leased content reads"), b"restart");

    restarted.stop().expect("restarted daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

#[test]
fn real_permanent_root_preserves_leased_path_while_gc_collects_unrooted_control() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leased = build_fixture_path(&fixture, &daemon, "retained-leased", "leased");
    let control = build_fixture_path(&fixture, &daemon, "retained-control", "control");
    let root_directory = fixture.root().join("gc-roots");
    fs::create_dir(&root_directory).expect("root directory creates");
    fs::set_permissions(&root_directory, fs::Permissions::from_mode(0o700))
        .expect("root directory permissions set");

    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("leased path valid before GC")
    );
    assert!(
        daemon
            .is_valid_path(&control)
            .expect("control path valid before GC")
    );

    let helper = std::env::var_os("TELCHAR_NIX_STORE_RETAIN")
        .expect("TELCHAR_NIX_STORE_RETAIN points to the flake-built helper");
    let request = serde_json::json!({
        "version": 1,
        "store_uri": daemon.store_url(),
        "root_directory": root_directory,
        "entries": [{
            "lease_id": "lease-retained-fixture",
            "store_path": leased,
        }],
    });
    let mut helper = Command::new(helper)
        .envs(fixture.environment())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("retention helper starts");
    helper
        .stdin
        .take()
        .expect("retention helper stdin")
        .write_all(request.to_string().as_bytes())
        .expect("retention helper request writes");
    let response = helper
        .wait_with_output()
        .expect("retention helper completes");
    assert!(
        response.status.success(),
        "retention helper failed: {response:?}"
    );
    let response: serde_json::Value =
        serde_json::from_slice(&response.stdout).expect("retention response parses");
    assert_eq!(response["version"], 1);
    assert_eq!(
        response["retained"][0]["lease_id"],
        "lease-retained-fixture"
    );
    assert_eq!(
        response["retained"][0]["store_path"].as_str(),
        Some(leased.to_string_lossy().as_ref())
    );
    let root = root_directory.join("lease-retained-fixture");
    assert_eq!(
        response["retained"][0]["root_path"].as_str(),
        Some(root.to_string_lossy().as_ref())
    );
    assert_eq!(fs::read_link(&root).expect("root symlink reads"), leased);

    daemon
        .collect_garbage()
        .expect("private store garbage collects");
    assert!(
        daemon
            .is_valid_path(&leased)
            .expect("leased path valid after GC")
    );
    assert_eq!(fs::read(&leased).expect("leased path reads"), b"leased");
    assert!(
        !daemon
            .is_valid_path(&control)
            .expect("control path validity after GC"),
        "unrooted control path survived private-store GC: {control:?}"
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
}

fn retain_fixture_path(
    helper: &std::ffi::OsStr,
    fixture: &NixFixture,
    store_uri: &str,
    root_directory: &std::path::Path,
    lease_id: &str,
    store_path: &std::path::Path,
) -> std::process::Output {
    let request = serde_json::json!({
        "version": 1,
        "store_uri": store_uri,
        "root_directory": root_directory,
        "entries": [{ "lease_id": lease_id, "store_path": store_path }],
    });
    let mut child = Command::new(helper)
        .envs(fixture.environment())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("retention helper starts");
    child
        .stdin
        .take()
        .expect("retention helper stdin")
        .write_all(request.to_string().as_bytes())
        .expect("retention helper request writes");
    child
        .wait_with_output()
        .expect("retention helper completes")
}

fn build_fixture_path(
    fixture: &NixFixture,
    daemon: &NixDaemon,
    name: &str,
    contents: &str,
) -> std::path::PathBuf {
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {contents} > \\\"$out\\\"\" ]; }}"
    );
    let output = Command::new("nix")
        .envs(fixture.environment())
        .args([
            "--store",
            &daemon.store_url(),
            "build",
            "--impure",
            "--expr",
            &expression,
            "--no-link",
            "--print-out-paths",
        ])
        .output()
        .expect("fixture derivation builds");
    assert!(
        output.status.success(),
        "fixture derivation failed: {output:?}"
    );
    std::path::PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("output path is UTF-8")
            .trim(),
    )
}
