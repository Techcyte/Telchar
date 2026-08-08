use std::process::Command;

use telchar::nix_fixture::{NixFixture, TrustMode};

#[test]
fn pinned_helper_returns_complete_transitive_input_closure() {
    let fixture = NixFixture::create().expect("fixture creates");
    let daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("fixture daemon starts");
    let leaf = Command::new("nix")
        .envs(fixture.environment())
        .args([
            "--store",
            &daemon.store_url(),
            "build",
            "--no-link",
            "--print-out-paths",
            "--expr",
            "derivation { name = \"closure-leaf\"; system = \"x86_64-linux\"; builder = \"/bin/sh\"; args = [ \"-c\" \"printf leaf > $out\" ]; }", 
        ])
        .output()
        .expect("leaf evaluates");
    assert!(leaf.status.success(), "leaf evaluation failed: {leaf:?}");
    let leaf = String::from_utf8(leaf.stdout).expect("leaf path is UTF-8");
    let leaf = leaf.trim();
    let root_expression = format!(
        "derivation {{ name = \"closure-root\"; system = \"x86_64-linux\"; src = builtins.storePath \"{leaf}\"; builder = \"/bin/sh\"; args = [ \"-c\" \"printf {leaf} > $out\" ]; }}"
    );
    let root = Command::new("nix")
        .envs(fixture.environment())
        .args([
            "--store",
            &daemon.store_url(),
            "build",
            "--impure",
            "--no-link",
            "--print-out-paths",
            "--expr",
            &root_expression,
        ])
        .output()
        .expect("root evaluates");
    assert!(root.status.success(), "root evaluation failed: {root:?}");
    let root = String::from_utf8(root.stdout).expect("root path is UTF-8");
    let root = root.trim();

    let helper = std::env::var("TELCHAR_NIX_STORE_CLOSURE")
        .expect("TELCHAR_NIX_STORE_CLOSURE points to the flake-built helper");
    let request = serde_json::json!({
        "version": 1,
        "store_uri": daemon.store_url(),
        "roots": [root],
    });
    let response = Command::new(helper)
        .envs(fixture.environment())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("helper stdin")
                .write_all(request.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect("closure helper runs");
    assert!(response.status.success(), "helper failed: {response:?}");
    let response: serde_json::Value =
        serde_json::from_slice(&response.stdout).expect("helper response JSON");
    let paths = response["paths"]
        .as_array()
        .expect("helper response paths array");
    let paths: Vec<&str> = paths
        .iter()
        .map(|path| path.as_str().expect("store path string"))
        .collect();
    assert_eq!(paths, vec![leaf, root]);
}
