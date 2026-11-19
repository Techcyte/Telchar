use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::time::Duration;
use telchar_test_support::process;

#[test]
fn receives_child_environment() {
    if process::is_child("receives_child_environment") {
        let expected = std::env::var_os("EXPECTED").unwrap();
        assert_eq!(std::env::var_os("ISOLATED_VALUE"), Some(expected));
    }
}

#[test]
fn concurrent_children_keep_values_out_of_parent() {
    let before = std::env::var_os("ISOLATED_VALUE");
    std::thread::scope(|scope| {
        for value in [OsString::from("first"), OsString::from_vec(vec![0x80])] {
            scope.spawn(move || {
                let mut command = process::command("receives_child_environment");
                command.env("ISOLATED_VALUE", &value).env("EXPECTED", value);
                process::assert_success(&mut command);
            });
        }
    });
    assert_eq!(std::env::var_os("ISOLATED_VALUE"), before);
}

#[test]
fn rejects_empty_filter_results_and_child_failure() {
    let missing =
        process::output(&mut process::command("absent_test"), Duration::from_secs(5)).unwrap();
    assert!(missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stdout).contains("0 passed"));
    let failure = process::output(
        process::command("receives_child_environment").env_remove("EXPECTED"),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stdout).contains("FAILED"));
}

#[test]
fn bounds_stalled_process() {
    let mut command = std::process::Command::new("sleep");
    command.arg("10");
    assert_eq!(
        process::output(&mut command, Duration::from_millis(20))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
}
