//! Tests bounded cache publication command execution without shell interpolation.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use telchar::service::cache_publication::CachePublisher;

#[test]
fn publisher_passes_validated_outputs_through_stdin() {
    let root =
        std::env::temp_dir().join(format!("telchar-cache-publication-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).expect("fixture creates");
    let input = root.join("input");
    let program = root.join("publisher");
    fs::write(
        &program,
        format!("#!/bin/sh\nset -eu\ncat > '{}'\n", input.display()),
    )
    .expect("publisher writes");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("publisher executable");
    let publisher = CachePublisher::new(
        &program,
        std::iter::empty::<&str>(),
        Duration::from_secs(1),
        1024,
    )
    .expect("publisher config validates");

    publisher
        .publish(&[
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".to_owned(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-two".to_owned(),
        ])
        .expect("publication succeeds");

    assert_eq!(
        fs::read_to_string(input).expect("publisher input reads"),
        "[\"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one\",\"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-two\"]\n"
    );
    fs::remove_dir_all(root).expect("fixture removes");
}

#[test]
fn publisher_rejects_relative_executable_and_bounds_diagnostics() {
    assert!(CachePublisher::new(
        "publisher",
        std::iter::empty::<&str>(),
        Duration::from_secs(1),
        1024,
    )
    .is_err());
    let publisher = CachePublisher::new(
        "/bin/sh",
        ["-c", "printf '%02048d' 0 >&2; exit 1"],
        Duration::from_secs(1),
        64,
    )
    .expect("publisher config validates");

    let error = publisher
        .publish(&["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".to_owned()])
        .expect_err("publisher failure reports");

    assert_eq!(error.to_string(), "cache publication command failed");
}
