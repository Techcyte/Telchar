//! Tests deployment config contracts and failure boundaries, including parses running disconnect policy.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::time::Duration;

use telchar::service::deployment::{OutputRetention, RunningDisconnectPolicy};

use telchar_test_support::process;

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
fn output_retention_defaults_to_one_hour() {
    let name = "output_retention_defaults_to_one_hour";
    if !process::is_child(name) {
        process::assert_success(
            process::command(name).env_remove("TELCHAR_OUTPUT_RETENTION_SECONDS"),
        );
        return;
    }

    let retention = OutputRetention::from_environment().expect("default retention parses");

    assert_eq!(retention.seconds(), 3_600);
    assert_eq!(retention.duration(), Duration::from_secs(3_600));
}

#[test]
fn output_retention_accepts_inclusive_bounds() {
    let name = "output_retention_accepts_inclusive_bounds";
    if process::is_child(name) {
        let expected = std::env::var("TELCHAR_OUTPUT_RETENTION_SECONDS")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(
            OutputRetention::from_environment().unwrap().seconds(),
            expected
        );
        return;
    }
    for (value, expected) in [("60", 60), ("86400", 86_400)] {
        assert_eq!(value.parse::<u64>().unwrap(), expected);
        process::assert_success(
            process::command(name).env("TELCHAR_OUTPUT_RETENTION_SECONDS", value),
        );
    }
}

#[test]
fn output_retention_rejects_out_of_range_values() {
    let name = "output_retention_rejects_out_of_range_values";
    if process::is_child(name) {
        assert_eq!(
            OutputRetention::from_environment().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        return;
    }
    for value in ["0", "59", "86401", "18446744073709551616"] {
        process::assert_success(
            process::command(name).env("TELCHAR_OUTPUT_RETENTION_SECONDS", value),
        );
    }
}

#[test]
fn output_retention_rejects_malformed_values() {
    let name = "output_retention_rejects_malformed_values";
    if process::is_child(name) {
        assert_eq!(
            OutputRetention::from_environment().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        return;
    }
    for value in ["", " ", " 60", "60 ", "+60", "-60", "60.0", "6e1", "sixty"] {
        process::assert_success(
            process::command(name).env("TELCHAR_OUTPUT_RETENTION_SECONDS", value),
        );
    }
}

#[test]
fn output_retention_rejects_non_unicode_value() {
    let name = "output_retention_rejects_non_unicode_value";
    if !process::is_child(name) {
        process::assert_success(process::command(name).env(
            "TELCHAR_OUTPUT_RETENTION_SECONDS",
            OsString::from_vec(vec![b'6', b'0', 0x80]),
        ));
        return;
    }

    let error = OutputRetention::from_environment().expect_err("non-Unicode value rejects");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
