use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::sync::Mutex;
use std::time::Duration;

use telchar::deployment::{DeploymentConfig, OutputRetention, RunningDisconnectPolicy};

static ENVIRONMENT: Mutex<()> = Mutex::new(());

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

#[test]
fn output_retention_defaults_to_one_hour() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
    unsafe { std::env::remove_var("TELCHAR_OUTPUT_RETENTION_SECONDS") };

    let retention = OutputRetention::from_environment().expect("default retention parses");

    assert_eq!(retention.seconds(), 3_600);
    assert_eq!(retention.duration(), Duration::from_secs(3_600));
    restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved);
}

#[test]
fn output_retention_accepts_inclusive_bounds() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    for (value, expected) in [("60", 60), ("86400", 86_400)] {
        let saved = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
        unsafe { std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", value) };

        let retention = OutputRetention::from_environment().expect("boundary retention parses");

        assert_eq!(retention.seconds(), expected);
        restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved);
    }
}

#[test]
fn output_retention_rejects_out_of_range_values() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    for value in ["0", "59", "86401", "18446744073709551616"] {
        let saved = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
        unsafe { std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", value) };

        let error = OutputRetention::from_environment().expect_err("out-of-range value rejects");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{value:?}");
        restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved);
    }
}

#[test]
fn output_retention_rejects_malformed_values() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    for value in ["", " ", " 60", "60 ", "+60", "-60", "60.0", "6e1", "sixty"] {
        let saved = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
        unsafe { std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", value) };

        let error = OutputRetention::from_environment().expect_err("malformed value rejects");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{value:?}");
        restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved);
    }
}

#[test]
fn output_retention_rejects_non_unicode_value() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
    unsafe {
        std::env::set_var(
            "TELCHAR_OUTPUT_RETENTION_SECONDS",
            OsString::from_vec(vec![b'6', b'0', 0x80]),
        )
    };

    let error = OutputRetention::from_environment().expect_err("non-Unicode value rejects");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved);
}

#[test]
fn deployment_config_exposes_output_retention_duration() {
    let _guard = ENVIRONMENT.lock().expect("environment lock");
    let saved_system = std::env::var_os("TELCHAR_SYSTEM");
    let saved_retention = std::env::var_os("TELCHAR_OUTPUT_RETENTION_SECONDS");
    unsafe {
        std::env::set_var("TELCHAR_SYSTEM", "x86_64-linux");
        std::env::set_var("TELCHAR_OUTPUT_RETENTION_SECONDS", "60");
    }

    let config = DeploymentConfig::from_environment().expect("deployment parses");

    assert_eq!(
        config.output_retention().duration(),
        Duration::from_secs(OutputRetention::MINIMUM_SECONDS)
    );
    restore("TELCHAR_SYSTEM", saved_system);
    restore("TELCHAR_OUTPUT_RETENTION_SECONDS", saved_retention);
}

fn restore(name: &str, value: Option<OsString>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}
