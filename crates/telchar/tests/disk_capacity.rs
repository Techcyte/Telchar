//! Exercises capacity admission with real filesystem permissions and captured diagnostics.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use telchar::service::disk_reserve::{
    DiskReserve, OsDiskReserveProbe, RejectionReason, gateway_store_directory,
};

#[test]
fn capacity_child() {
    let Ok(mode) = std::env::var("TELCHAR_TEST_CAPACITY_MODE") else {
        return;
    };
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    if mode == "default" {
        assert_eq!(gateway_store_directory().unwrap(), Path::new("/nix/store"));
        return;
    }
    if mode == "relative" {
        assert_eq!(
            gateway_store_directory().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        return;
    }
    let store = gateway_store_directory().expect("store directory loads");
    assert_eq!(
        store,
        std::path::PathBuf::from(std::env::var_os("TELCHAR_GATEWAY_STORE_DIRECTORY").unwrap())
    );
    let staging = std::env::var_os("TELCHAR_TEST_STAGING_DIRECTORY").expect("staging configured");
    let bytes = if mode.starts_with("insufficient") {
        u64::MAX
    } else {
        1
    };
    let reserve = DiskReserve::parse(&bytes.to_string()).expect("reserve parses");
    let build = reserve.admit_build(&OsDiskReserveProbe, Path::new(&store));
    let transfer = reserve.admit_transfer(
        &OsDiskReserveProbe,
        Path::new(&store),
        Path::new(&staging),
        0,
    );
    if mode == "insufficient-staging" {
        build.expect("unknown store capacity permits build");
        assert_eq!(
            transfer.expect_err("measured staging rejects").filesystem(),
            "staging"
        );
    } else if mode == "insufficient-store" {
        assert_eq!(
            build.expect_err("measured store rejects").reason(),
            RejectionReason::InsufficientSpace
        );
        assert_eq!(
            transfer.expect_err("measured store rejects").filesystem(),
            "gateway-store"
        );
    } else if mode == "insufficient" {
        assert_eq!(
            build.expect_err("measured store rejects").reason(),
            RejectionReason::InsufficientSpace
        );
        assert_eq!(
            transfer.expect_err("measured store rejects").reason(),
            RejectionReason::InsufficientSpace
        );
    } else {
        build.expect("unavailable measurement permits build");
        transfer.expect("unavailable measurement permits transfer");
    }
}

#[test]
fn capacity_checks_warn_on_denied_access_and_reject_measured_shortage() {
    let root = std::env::temp_dir().join(format!("telchar-capacity-{}", std::process::id()));
    fs::create_dir(&root).expect("root creates");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    let blocked = root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::create_dir(blocked.join("store")).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    for (mode, store, staging, warnings) in [
        ("available", root.clone(), root.clone(), 0),
        ("denied", blocked.join("store"), root.clone(), 2),
        ("denied", root.clone(), blocked.join("store"), 1),
        ("denied", blocked.join("store"), blocked.join("store"), 3),
        ("missing", root.join("missing"), root.clone(), 2),
        ("insufficient", root.clone(), root.clone(), 0),
        (
            "insufficient-staging",
            blocked.join("store"),
            root.clone(),
            2,
        ),
        ("insufficient-store", root.clone(), blocked.join("store"), 1),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "capacity_child", "--nocapture"])
            .env("TELCHAR_TEST_CAPACITY_MODE", mode)
            .env("TELCHAR_GATEWAY_STORE_DIRECTORY", &store)
            .env("TELCHAR_TEST_STAGING_DIRECTORY", &staging);
        if rustix::process::geteuid().is_root() {
            child.uid(65534).gid(65534);
        }
        let output = child.output().expect("capacity subprocess runs");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{mode}: {stderr}");
        assert_eq!(
            stderr.matches("worker.disk_reserve.probe_failed").count(),
            warnings,
            "{stderr}"
        );
        assert!(
            !stderr.contains(&root.display().to_string()),
            "paths must not appear: {stderr}"
        );
        if warnings == 0 {
            assert!(stderr.is_empty(), "{stderr}");
        } else {
            assert!(stderr.contains("WARN"), "{stderr}");
        }
    }
    for mode in ["default", "relative"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "capacity_child", "--nocapture"])
            .env("TELCHAR_TEST_CAPACITY_MODE", mode)
            .env_remove("TELCHAR_GATEWAY_STORE_DIRECTORY");
        if mode == "relative" {
            child.env("TELCHAR_GATEWAY_STORE_DIRECTORY", "relative/path");
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(root).unwrap();
}
