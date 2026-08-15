//! Focused failure cleanup contracts.

use super::*;

#[test]
fn helper_failure_is_followed_by_authoritative_validity_query_and_cleanup() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::failing();
    backend.valid_paths = vec![true, false];

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("helper failure must surface");

    assert!(error.to_string().contains("promotion failed"));
    assert!(backend.request.is_some(), "helper was not invoked");
    assert_eq!(backend.queried_paths.last(), Some(&declared.path));
    assert_directory_empty(staging.path());
}

#[test]
fn helper_failure_rejects_unexpected_authoritative_registration() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::failing();
    backend.valid_paths = vec![true, true];

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("helper failure with valid path must fail closed");

    assert!(error.to_string().contains("authoritative path is valid"));
    assert_directory_empty(staging.path());
}

#[test]
fn cleanup_failure_is_reported_after_success() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());
    backend.before_promote = Some(Box::new(|request| {
        std::fs::remove_file(&request.nar_path)?;
        std::fs::create_dir(&request.nar_path)
    }));

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("cleanup failure must surface");

    assert!(error.to_string().contains("staged NAR cleanup failed"));
}

#[test]
fn staged_mutation_after_validation_is_left_for_nix_to_reject() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::failing();
    backend.valid_paths = vec![true, false];
    backend.before_promote = Some(Box::new(|request| {
        let mut nar = std::fs::read(&request.nar_path)?;
        nar[96] ^= 0xff;
        std::fs::write(&request.nar_path, nar)
    }));

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("Nix must reject staged mutation");

    assert!(error.to_string().contains("promotion failed"));
    assert!(backend.request.is_some(), "helper was not invoked");
    assert_eq!(backend.queried_paths.last(), Some(&declared.path));
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_registered_metadata_mismatch() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut wrong = registered_path_info();
    wrong.nar_size += 1;
    let mut backend = RecordingBackend::accepting(wrong);

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("registered metadata mismatch must fail");

    assert!(error.to_string().contains("registered metadata mismatch"));
    assert_directory_empty(staging.path());
}
