//! Tests deployment config contracts and failure boundaries, including parses running disconnect policy.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::sync::Mutex;
use std::time::Duration;

use telchar::deployment::{OutputRetention, RunningDisconnectPolicy};

static ENVIRONMENT: Mutex<()> = Mutex::new(());

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

fn restore(name: &str, value: Option<OsString>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}
