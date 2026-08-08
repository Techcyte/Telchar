use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use telchar::nix_fixture::{NixDaemon, NixFixture, TrustMode};

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

    assert!(daemon.is_valid_path(&leased).expect("leased path valid before GC"));
    assert!(daemon.is_valid_path(&control).expect("control path valid before GC"));

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
    assert!(response.status.success(), "retention helper failed: {response:?}");
    let response: serde_json::Value =
        serde_json::from_slice(&response.stdout).expect("retention response parses");
    assert_eq!(response["version"], 1);
    assert_eq!(response["retained"][0]["lease_id"], "lease-retained-fixture");
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

    daemon.collect_garbage().expect("private store garbage collects");
    assert!(daemon.is_valid_path(&leased).expect("leased path valid after GC"));
    assert_eq!(fs::read(&leased).expect("leased path reads"), b"leased");
    assert!(
        !daemon.is_valid_path(&control).expect("control path validity after GC"),
        "unrooted control path survived private-store GC: {control:?}"
    );

    daemon.stop().expect("fixture daemon stops");
    fixture.cleanup().expect("fixture cleans up");
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
    assert!(output.status.success(), "fixture derivation failed: {output:?}");
    std::path::PathBuf::from(String::from_utf8(output.stdout).expect("output path is UTF-8").trim())
}
