use super::*;

fn bash() -> std::path::PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .expect("bash on test PATH")
}

#[test]
fn resolver_configuration_bounds_commands_and_refresh() {
    let valid = r#"[resolver]
command = ["/bin/true"]
refresh_interval = "5m"
command_timeout = "10s"
unknown_key_refresh_limit = "6/min"
"#;
    let config: UploadVerification = toml::from_str(valid).unwrap();
    config.validate().unwrap();
    for invalid in [
        valid.replace("5m", "0s"),
        valid.replace("/bin/true", "true"),
        valid.replace("6/min", "6/hour"),
    ] {
        let result = toml::from_str::<UploadVerification>(&invalid)
            .and_then(|config| config.validate().map_err(serde::de::Error::custom));
        assert!(result.is_err());
    }
}

#[test]
fn registry_refreshes_changed_files_and_retains_keys_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("keys.json");
    let write = |name: &str| {
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key(name)}]}))
                .unwrap(),
        )
        .unwrap()
    };
    write("first");
    let config: UploadVerification = toml::from_str(&format!(
        "[resolver]\nfile = {:?}\nrefresh_interval = \"1s\"",
        file
    ))
    .unwrap();
    let registry = registry::Registry::start(&config).unwrap();
    assert!(registry.lookup(&["first"]).contains_key("first"));
    write("second");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while !registry.lookup(&[]).contains_key("second") {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::fs::write(&file, b"invalid").unwrap();
    assert!(registry.lookup(&["unknown"]).contains_key("second"));
}

#[test]
fn command_refresh_is_rate_limited_and_keeps_last_good_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("resolver");
    let counter = root.path().join("calls");
    let response = root.path().join("response");
    std::fs::write(
        &response,
        serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci") }]}))
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &script,
        format!(
            "#!{}\nprintf x >> \"$1\"\nexec cat \"$2\"\n",
            bash().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config: UploadVerification = toml::from_str(&format!(
        "[resolver]\ncommand = [{:?}, {:?}, {:?}]\nunknown_key_refresh_limit = \"1/min\"",
        script, counter, response
    ))
    .unwrap();
    let registry = registry::Registry::start(&config).unwrap();
    assert!(registry.lookup(&["ci"]).contains_key("ci"));
    std::fs::write(&response, b"invalid").unwrap();
    assert!(registry.lookup(&["unknown"]).contains_key("ci"));
    assert!(registry.lookup(&["another"]).contains_key("ci"));
    assert_eq!(std::fs::read(counter).unwrap(), b"xx");
}

#[test]
fn resolver_command_bounds_noisy_output_and_unclosed_pipes() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("resolver");
    std::fs::write(
        &script,
        format!(
            "#!{}\nif [[ $1 == noisy ]]; then head -c 1048577 /dev/zero; else sleep 2; fi\n",
            bash().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    for argument in ["noisy", "blocked"] {
        let started = std::time::Instant::now();
        let result = registry::run_command(
            &[script.to_string_lossy().into_owned(), argument.into()],
            std::time::Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}

#[test]
fn registry_rejects_oversized_metadata_before_signature_expansion() {
    let config: UploadVerification = toml::from_str(&format!(
        "[[trusted_keys]]\npublic_key = {:?}\nmetadata = {{ identity = {:?} }}",
        key("ci"),
        "x".repeat(4097)
    ))
    .unwrap();
    assert!(config.validate().is_err());
    let mut source = KeySource::default();
    source.replace(&serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci"),"metadata":{"identity":"x".repeat(4097)}}]})).unwrap(), "file").unwrap();
    assert!(source.keys.is_empty());
}

#[test]
fn registry_file_fifo_never_blocks_startup() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("keys");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let config: UploadVerification =
        toml::from_str(&format!("[resolver]\nfile = {:?}", fifo)).unwrap();
    let started = std::time::Instant::now();
    let registry = registry::Registry::start(&config).unwrap();
    assert!(registry.lookup(&[]).is_empty());
    drop(registry);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn successful_resolver_cleans_up_background_descendants() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("resolver");
    let pid = root.path().join("pid");
    std::fs::write(
        &script,
        format!(
            "#!{}\nsleep 60 >/dev/null 2>&1 &\nprintf '%s' \"$!\" > \"$1\"\nprintf '{{}}'",
            bash().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = registry::run_command(
        &[
            script.to_string_lossy().into_owned(),
            pid.to_string_lossy().into_owned(),
        ],
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(output, b"{}");
    let pid: i32 = std::fs::read_to_string(pid).unwrap().parse().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        if stat.as_ref().is_err() || stat.unwrap().split_whitespace().nth(2) == Some("Z") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resolver descendant remains running"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn key(name: &str) -> String {
    format!("{name}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
}

#[test]
fn resolver_retains_snapshot_and_quarantines_detailed_duplicates() {
    let mut source = KeySource::default();
    let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci"),"metadata":{"identity":"alice"}}]})).unwrap();
    source.replace(&bytes, "file").unwrap();
    assert_eq!(source.keys.len(), 1);
    assert!(
        source
            .replace(br#"{"version":2,"keys":[]}"#, "file")
            .is_err()
    );
    assert!(
        source
            .replace(br#"{"version":1,"keys":[]}"#, "file")
            .is_err()
    );
    assert_eq!(source.keys.len(), 1);
    let bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"keys":[{"publicKey":key("ci")},{"publicKey":key("ci")}]}),
    )
    .unwrap();
    source.replace(&bytes, "file").unwrap();
    assert!(source.keys.is_empty());
    assert!(source.rejected.contains("ci"));
}

#[test]
fn registry_combines_simple_with_one_detailed_but_not_two() {
    let config: UploadVerification =
        toml::from_str(&format!("trusted_public_keys = [\"{}\"]", key("ci"))).unwrap();
    let mut source = KeySource::default();
    source.replace(&serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci"),"revoked":true,"metadata":{"identity":"alice"}}]})).unwrap(), "file").unwrap();
    let keys = merge_keys(&config.static_keys().unwrap(), &[&source]);
    assert!(keys["ci"].revoked);
    assert_eq!(keys["ci"].source, "file");
    assert!(merge_keys(&config.static_keys().unwrap(), &[&source, &source]).is_empty());
}
