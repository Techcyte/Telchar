use std::io;

use telchar::deployment::{DeploymentConfig, RunningDisconnectPolicy};

#[test]
fn parses_one_system_and_bounded_unique_features() {
    let config = DeploymentConfig::parse("x86_64-linux", "kvm,big-parallel")
        .expect("one-system deployment parses");

    assert_eq!(config.system(), "x86_64-linux");
    assert_eq!(config.supported_features(), ["big-parallel", "kvm"]);
}

#[test]
fn rejects_empty_multiple_or_malformed_systems() {
    for system in [
        "",
        "x86_64-linux,aarch64-linux",
        "x86_64 linux",
        "../x86_64-linux",
    ] {
        let error = DeploymentConfig::parse(system, "")
            .expect_err("deployment must configure exactly one valid Nix system");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{system:?}");
    }
}

#[test]
fn rejects_duplicate_or_excess_features() {
    assert_eq!(
        DeploymentConfig::parse("x86_64-linux", "kvm,kvm")
            .expect_err("duplicate feature must fail")
            .kind(),
        io::ErrorKind::InvalidInput
    );

    let excess = (0..65)
        .map(|index| format!("feature-{index}"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        DeploymentConfig::parse("x86_64-linux", &excess)
            .expect_err("feature count is bounded")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn parses_running_disconnect_policy() {
    assert_eq!(
        RunningDisconnectPolicy::parse("detach-and-finish").expect("default policy parses"),
        RunningDisconnectPolicy::DetachAndFinish
    );
    assert_eq!(
        RunningDisconnectPolicy::parse("cancel-running").expect("cancel policy parses"),
        RunningDisconnectPolicy::CancelRunning
    );
}

#[test]
fn rejects_unknown_running_disconnect_policy() {
    let error = RunningDisconnectPolicy::parse("requester-choice")
        .expect_err("unknown policy must fail closed");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn rejects_oversized_feature_before_retaining_it() {
    let feature = "x".repeat(257);
    assert_eq!(
        DeploymentConfig::parse("x86_64-linux", &feature)
            .expect_err("feature length is bounded")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
