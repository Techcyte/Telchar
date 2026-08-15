//! Focused validation contracts.

use super::*;

#[test]
fn promotes_matching_staged_nar_and_verifies_registered_metadata() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let registered = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect("matching NAR promotes");

    assert_eq!(registered, registered_path_info());
    let request = backend.request.expect("helper invoked once");
    assert_eq!(request.version, 1);
    assert_eq!(request.store_uri, "unix:///run/nix-daemon.sock");
    assert_eq!(request.staging_directory, staging.path());
    assert_eq!(request.path, declared.path);
    assert_eq!(request.nar_hash_hex, hex_hash(NAR_HASH));
    assert_eq!(request.nar_size, declared.nar_size);
    assert!(request.nar_path.starts_with(staging.path()));
    assert!(
        !request.nar_path.exists(),
        "staging file leaked after success"
    );
    assert_eq!(
        backend.queried_paths,
        vec![declared.references[0].clone(), declared.path]
    );
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_nar_above_transfer_limit_before_helper_invocation() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());
    let mut session = TransferBudget::new(1_000);
    let mut source = LimitedReader::new(Cursor::new(regular_nar(CONTENT)), 135, &mut session);

    let error = validate_and_promote_nar(
        &mut source,
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("NAR above object limit must fail before promotion");

    assert!(error.to_string().contains("NAR object byte limit exceeded"));
    assert_eq!(session.charged(), 135);
    assert!(
        backend.request.is_none(),
        "helper invoked for transfer-limit rejection"
    );
    assert!(backend.queried_paths.is_empty());
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_content_hash_mismatch_before_helper_invocation() {
    let staging = TestDirectory::create();
    let mut nar = regular_nar(CONTENT);
    nar[96] ^= 0xff;
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let error = validate_and_promote_nar(
        Cursor::new(nar),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("mutated content must fail");

    assert!(error.to_string().contains("NAR hash mismatch"));
    assert!(
        backend.request.is_none(),
        "helper invoked for hash mismatch"
    );
    assert_eq!(backend.queried_paths, vec![declared.path]);
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_declared_size_mismatch_before_helper_invocation() {
    let staging = TestDirectory::create();
    let mut declared = declared_path_info();
    declared.nar_size += 1;
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("size mismatch must fail");

    assert!(error.to_string().contains("NAR size mismatch"));
    assert!(
        backend.request.is_none(),
        "helper invoked for size mismatch"
    );
    assert_eq!(backend.queried_paths, vec![declared.path]);
    assert_directory_empty(staging.path());
}

#[test]
fn preserves_fixed_output_content_address_during_promotion() {
    let staging = TestDirectory::create();
    let mut declared = declared_path_info();
    declared.content_address =
        Some("fixed:r:sha256:0000000000000000000000000000000000000000000000000000".into());
    let mut registered = registered_path_info();
    registered.content_address = declared.content_address.clone();
    let mut backend = RecordingBackend::accepting(registered.clone());

    let result = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect("fixed-output metadata promotes");

    assert_eq!(result.content_address, registered.content_address);
    assert_eq!(
        backend
            .request
            .as_ref()
            .expect("promotion invoked")
            .content_address,
        declared.content_address
    );
}

#[test]
fn rejects_unsupported_classic_metadata_before_staging() {
    let cases: Vec<(&str, DeclarationMutation)> = vec![
        (
            "signature",
            Box::new(|info| info.signatures.push("sig".into())),
        ),
        ("ultimate", Box::new(|info| info.ultimate = true)),
    ];
    for (label, mutate) in cases {
        let staging = TestDirectory::create();
        let mut declared = declared_path_info();
        mutate(&mut declared);
        let mut backend = RecordingBackend::accepting(registered_path_info());

        let error = validate_and_promote_nar(
            Cursor::new(regular_nar(CONTENT)),
            staging.path(),
            Path::new(STORE_DIRECTORY),
            &declared,
            &mut backend,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("unsupported"),
            "{label}: {error}"
        );
        assert!(backend.request.is_none(), "{label} invoked helper");
        assert!(backend.queried_paths.is_empty(), "{label} queried store");
        assert_directory_empty(staging.path());
    }
}

#[test]
fn rejects_invalid_and_duplicate_path_metadata_before_staging() {
    let cases: Vec<(&str, DeclarationMutation)> = vec![
        (
            "path outside store",
            Box::new(|info| info.path = PathBuf::from("/tmp/not-store")),
        ),
        (
            "short path basename",
            Box::new(|info| info.path = PathBuf::from("/nix/store/short")),
        ),
        (
            "invalid path hash",
            Box::new(|info| {
                info.path = PathBuf::from("/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bad")
            }),
        ),
        (
            "missing path separator",
            Box::new(|info| {
                info.path = PathBuf::from("/nix/store/0123456789abcdfghijklmnpqrsvwxyztelchar")
            }),
        ),
        (
            "illegal path name",
            Box::new(|info| {
                info.path =
                    PathBuf::from("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-name@invalid")
            }),
        ),
        (
            "reserved path name",
            Box::new(|info| {
                info.path = PathBuf::from("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-.")
            }),
        ),
        (
            "overlong path",
            Box::new(|info| {
                info.path = Path::new(STORE_DIRECTORY).join(format!(
                    "0123456789abcdfghijklmnpqrsvwxyz-{}",
                    "a".repeat(179)
                ))
            }),
        ),
        (
            "invalid reference",
            Box::new(|info| info.references.push(PathBuf::from("relative"))),
        ),
        (
            "duplicate reference",
            Box::new(|info| info.references.push(info.references[0].clone())),
        ),
        (
            "invalid deriver",
            Box::new(|info| info.deriver = Some(PathBuf::from(PATH))),
        ),
    ];

    for (label, mutate) in cases {
        let staging = TestDirectory::create();
        let mut declared = declared_path_info();
        mutate(&mut declared);
        let mut backend = RecordingBackend::accepting(registered_path_info());

        let error = validate_and_promote_nar(
            Cursor::new(regular_nar(CONTENT)),
            staging.path(),
            Path::new(STORE_DIRECTORY),
            &declared,
            &mut backend,
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid"), "{label}: {error}");
        assert!(backend.request.is_none(), "{label} invoked helper");
        assert!(backend.queried_paths.is_empty(), "{label} queried store");
        assert_directory_empty(staging.path());
    }
}

#[test]
fn rejects_reference_count_above_limit_before_staging() {
    let staging = TestDirectory::create();
    let mut declared = declared_path_info();
    declared.references = (0..=MAXIMUM_PROMOTION_REFERENCES)
        .map(|index| {
            Path::new(STORE_DIRECTORY)
                .join(format!("{:032}-reference-{index}", format!("{index:x}")))
        })
        .collect();
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("excess references must fail");

    assert!(error.to_string().contains("too many references"));
    assert!(backend.request.is_none());
    assert!(backend.queried_paths.is_empty());
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_missing_reference_before_staging() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());
    backend.valid_paths = vec![false];

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("missing reference must fail");

    assert!(error.to_string().contains("reference is not valid"));
    assert!(backend.request.is_none());
    assert_directory_empty(staging.path());
}
