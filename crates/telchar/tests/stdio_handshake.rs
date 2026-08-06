use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn pinned_nix_completes_worker_handshake_with_serve_stdio() {
    let root = std::env::temp_dir().join(format!(
        "telchar-stdio-handshake-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::create_dir(&root).expect("test root creates");
    let ssh = root.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nexec \"$TELCHAR_STDIO_BIN\" serve-stdio\n")
        .expect("SSH shim writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).expect("SSH shim is executable");

    let path = format!(
        "{}:{}",
        root.display(),
        std::env::var("PATH").expect("PATH exists")
    );
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            "ssh-ng://telchar-handshake-test",
            "store",
            "info",
        ])
        .env("PATH", path)
        .env("TELCHAR_STDIO_BIN", env!("CARGO_BIN_EXE_telchar"))
        .output()
        .expect("pinned Nix client runs");

    let _ = fs::remove_dir_all(root);
    assert!(output.status.success(), "pinned Nix failed: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Version: telchar"),
        "pinned client did not receive server handshake information: {stderr}"
    );
}

#[test]
fn pinned_nix_reports_framed_error_after_set_options() {
    let root = std::env::temp_dir().join(format!(
        "telchar-stdio-error-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::create_dir(&root).expect("test root creates");
    let ssh = root.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nexec \"$TELCHAR_STDIO_BIN\" serve-stdio\n")
        .expect("SSH shim writes");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).expect("SSH shim is executable");

    let path = format!(
        "{}:{}",
        root.display(),
        std::env::var("PATH").expect("PATH exists")
    );
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            "ssh-ng://telchar-error-test",
            "path-info",
            "/nix/store/00000000000000000000000000000000-missing",
        ])
        .env("PATH", path)
        .env("TELCHAR_STDIO_BIN", env!("CARGO_BIN_EXE_telchar"))
        .output()
        .expect("pinned Nix client runs");

    let _ = fs::remove_dir_all(root);
    assert!(
        !output.status.success(),
        "request must receive framed rejection"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported worker operation"),
        "client did not decode worker error: {stderr}"
    );
    assert!(
        !stderr.contains("unexpected end-of-file"),
        "client received EOF instead of worker error: {stderr}"
    );
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
