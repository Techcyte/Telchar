//! Tests persistence release leases.

use super::*;

#[test]
fn request_lease_release_rejects_missing_derivation_and_mixed_state_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-invalid-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    for (request_id, derivation_lease, input_lease) in [
        (
            "release-missing-derivation",
            None,
            Some("release-missing-input"),
        ),
        (
            "release-mixed-state",
            Some("release-mixed-derivation"),
            Some("release-mixed-input"),
        ),
    ] {
        telchar::persistence::create_build_request(
            fixture.url(),
            request_id,
            "/nix/store/11111111111111111111111111111111-release-invalid.drv",
            "x86_64-linux",
            "test-audit",
            "test-quota",
        )
        .expect("request persists");
        telchar::persistence::attach_request(fixture.url(), "release-invalid-session", request_id)
            .expect("request attaches");
        if let Some(derivation_lease) = derivation_lease {
            telchar::persistence::create_store_lease(
                fixture.url(),
                derivation_lease,
                telchar::persistence::StoreLeaseOwnerKind::Request,
                request_id,
                "/nix/store/11111111111111111111111111111111-release-invalid.drv",
                telchar::persistence::StoreLeasePurpose::Derivation,
            )
            .expect("derivation persists");
        }
        if let Some(input_lease) = input_lease {
            telchar::persistence::create_store_lease(
                fixture.url(),
                input_lease,
                telchar::persistence::StoreLeaseOwnerKind::Request,
                request_id,
                "/nix/store/22222222222222222222222222222222-release-invalid-input",
                telchar::persistence::StoreLeasePurpose::Input,
            )
            .expect("input persists");
        }
    }
    telchar::persistence::release_store_lease(fixture.url(), "release-mixed-input")
        .expect("input changes to released");

    for request_id in ["release-missing-derivation", "release-mixed-state"] {
        assert_eq!(
            telchar::persistence::detach_request_and_release_leases(
                fixture.url(),
                "release-invalid-session",
                request_id,
            )
            .expect_err("invalid request lease set rejects")
            .failure(),
            telchar::persistence::StoreLeaseFailure::Query
        );
        assert_eq!(
            telchar::persistence::read_request_attachment(
                fixture.url(),
                "release-invalid-session",
                request_id,
            )
            .expect("attachment reads")
            .expect("attachment exists")
            .state,
            telchar::persistence::RequestAttachmentState::Attached
        );
    }
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-mixed-derivation")
            .expect("derivation reads")
            .expect("derivation exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_preserves_active_output_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-output-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-output-request",
        "/nix/store/11111111111111111111111111111111-release-output.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-output-session",
        "release-output-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-output-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-output-request",
        "/nix/store/11111111111111111111111111111111-release-output.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation lease persists");
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "release-output-request",
        &[(
            "release-output-input".to_owned(),
            "/nix/store/22222222222222222222222222222222-release-output-input".to_owned(),
        )],
    )
    .expect("input lease persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "release-output-request",
        Duration::from_secs(3_600),
        &[(
            "release-output-result".to_owned(),
            "/nix/store/33333333333333333333333333333333-release-output-result".to_owned(),
        )],
    )
    .expect("output lease persists");

    let released = telchar::persistence::detach_request_and_release_leases(
        fixture.url(),
        "release-output-session",
        "release-output-request",
    )
    .expect("request detaches and releasable leases release");

    assert_eq!(
        released
            .leases
            .iter()
            .map(|lease| lease.purpose)
            .collect::<Vec<_>>(),
        vec![
            telchar::persistence::StoreLeasePurpose::Derivation,
            telchar::persistence::StoreLeasePurpose::Input,
        ]
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-output-result")
            .expect("output lease reads")
            .expect("output lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-output-session",
            "release-output-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
}

#[test]
fn request_lease_release_commit_failure_keeps_attachment_and_leases_active() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-commit-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-commit-request",
        "/nix/store/11111111111111111111111111111111-release-commit.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-commit-session",
        "release-commit-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-commit-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-commit-request",
        "/nix/store/11111111111111111111111111111111-release-commit.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    fixture
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_request_release_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject request release commit'; END $$; CREATE CONSTRAINT TRIGGER reject_request_release_commit AFTER UPDATE ON store_leases DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_request_release_commit();",
        )
        .expect("commit failure trigger installs");

    assert_eq!(
        telchar::persistence::detach_request_and_release_leases(
            fixture.url(),
            "release-commit-session",
            "release-commit-request",
        )
        .expect_err("commit rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Commit
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-commit-session",
            "release-commit-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-commit-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_rejects_statement_failure_without_mutation() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-failure-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-failure-request",
        "/nix/store/11111111111111111111111111111111-release-failure.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(
        fixture.url(),
        "release-failure-session",
        "release-failure-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "release-failure-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "release-failure-request",
        "/nix/store/11111111111111111111111111111111-release-failure.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");
    fixture
        .connect()
        .batch_execute(
            "CREATE FUNCTION reject_request_lease_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject request lease release'; END $$; CREATE TRIGGER reject_request_lease_release BEFORE UPDATE ON store_leases FOR EACH ROW EXECUTE FUNCTION reject_request_lease_release();",
        )
        .expect("failure trigger installs");

    assert_eq!(
        telchar::persistence::detach_request_and_release_leases(
            fixture.url(),
            "release-failure-session",
            "release-failure-request",
        )
        .expect_err("lease update rejects")
        .failure(),
        telchar::persistence::StoreLeaseFailure::Query
    );
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-failure-session",
            "release-failure-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Attached
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "release-failure-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_page_is_bounded_keyset_ordered_and_includes_output_leases() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-page-request",
        "/nix/store/11111111111111111111111111111111-released-page.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "released-page-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "released-page-request",
        "/nix/store/11111111111111111111111111111111-released-page.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("derivation persists");
    let inputs = (0..256)
        .map(|index| {
            (
                format!("released-page-input-{index:03}"),
                format!("/nix/store/{index:032x}-released-page-input-{index:03}"),
            )
        })
        .collect::<Vec<_>>();
    telchar::persistence::create_request_input_leases(
        fixture.url(),
        "released-page-request",
        &inputs,
    )
    .expect("inputs persist");
    telchar::persistence::release_unattached_request_leases(fixture.url(), "released-page-request")
        .expect("request leases release");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-page-other",
        "/nix/store/ffffffffffffffffffffffffffffffff-released-page-other",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("other request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "released-page-output",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "released-page-other",
        "/nix/store/ffffffffffffffffffffffffffffffff-released-page-other",
        telchar::persistence::StoreLeasePurpose::Output,
    )
    .expect("output persists");
    telchar::persistence::release_store_lease(fixture.url(), "released-page-output")
        .expect("output releases");

    let first = telchar::persistence::read_released_request_leases_page(fixture.url(), None, 999)
        .expect("first page reads");
    assert_eq!(first.len(), 256);
    assert!(first
        .windows(2)
        .all(|window| window[0].lease_id < window[1].lease_id));
    assert!(first.iter().all(|lease| {
        lease.owner_kind == telchar::persistence::StoreLeaseOwnerKind::Request
            && lease.state == telchar::persistence::StoreLeaseState::Released
    }));
    let last = first.last().expect("first page has rows");
    let second = telchar::persistence::read_released_request_leases_page(
        fixture.url(),
        Some(&last.lease_id),
        256,
    )
    .expect("second page reads");
    assert_eq!(second.len(), 2);
    assert!(second[0].lease_id > last.lease_id);
    assert!(second[0].lease_id < second[1].lease_id);
    assert!(second.iter().any(|lease| {
        lease.lease_id == "released-page-output"
            && lease.purpose == telchar::persistence::StoreLeasePurpose::Output
    }));
}

#[test]
fn released_request_lease_page_includes_output_reconciliation_authority() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "released-output-page-request",
        "/nix/store/11111111111111111111111111111111-released-output-page.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_request_output_leases(
        fixture.url(),
        "released-output-page-request",
        Duration::from_secs(60),
        &[(
            "released-output-page".to_owned(),
            "/nix/store/22222222222222222222222222222222-released-output-page".to_owned(),
        )],
    )
    .expect("output lease persists");
    telchar::persistence::release_store_lease(fixture.url(), "released-output-page")
        .expect("output lease releases");

    let released =
        telchar::persistence::read_released_request_leases_page(fixture.url(), None, 256)
            .expect("released page reads");

    assert_eq!(released.len(), 1);
    assert_eq!(released[0].lease_id, "released-output-page");
    assert_eq!(
        released[0].purpose,
        telchar::persistence::StoreLeasePurpose::Output
    );
    assert_eq!(
        released[0].state,
        telchar::persistence::StoreLeaseState::Released
    );
}

#[test]
fn request_lease_release_unattached_releases_only_without_attachment() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    telchar::persistence::create_build_request(
        fixture.url(),
        "unattached-release-request",
        "/nix/store/11111111111111111111111111111111-unattached-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "unattached-release-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "unattached-release-request",
        "/nix/store/11111111111111111111111111111111-unattached-release.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    let released = telchar::persistence::release_unattached_request_leases(
        fixture.url(),
        "unattached-release-request",
    )
    .expect("unattached lease releases");
    assert_eq!(released.leases.len(), 1);
    assert_eq!(
        released.leases[0].state,
        telchar::persistence::StoreLeaseState::Released
    );

    telchar::persistence::create_build_request(
        fixture.url(),
        "attached-release-request",
        "/nix/store/22222222222222222222222222222222-attached-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("attached request persists");
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "attached-release-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::attach_request(
        fixture.url(),
        "attached-release-session",
        "attached-release-request",
    )
    .expect("request attaches");
    telchar::persistence::create_store_lease(
        fixture.url(),
        "attached-release-derivation",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "attached-release-request",
        "/nix/store/22222222222222222222222222222222-attached-release.drv",
        telchar::persistence::StoreLeasePurpose::Derivation,
    )
    .expect("lease persists");

    assert_eq!(
        telchar::persistence::release_unattached_request_leases(
            fixture.url(),
            "attached-release-request",
        )
        .expect_err("attachment blocks unattached release")
        .failure(),
        telchar::persistence::StoreLeaseFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::read_store_lease(fixture.url(), "attached-release-derivation")
            .expect("lease reads")
            .expect("lease exists")
            .state,
        telchar::persistence::StoreLeaseState::Active
    );
}

#[test]
fn request_lease_release_detaches_and_releases_complete_active_set_atomically() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let requester = "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e";
    telchar::persistence::open_protocol_session(
        fixture.url(),
        "release-session",
        requester,
        "ssh-pubkey:SHA256:test",
        "test-audit",
        "test-quota",
    )
    .expect("session opens");
    telchar::persistence::create_build_request(
        fixture.url(),
        "release-request",
        "/nix/store/11111111111111111111111111111111-release.drv",
        "x86_64-linux",
        "test-audit",
        "test-quota",
    )
    .expect("request persists");
    telchar::persistence::attach_request(fixture.url(), "release-session", "release-request")
        .expect("request attaches");
    for (lease_id, store_path, purpose) in [
        (
            "release-derivation",
            "/nix/store/11111111111111111111111111111111-release.drv",
            telchar::persistence::StoreLeasePurpose::Derivation,
        ),
        (
            "release-input",
            "/nix/store/22222222222222222222222222222222-release-input",
            telchar::persistence::StoreLeasePurpose::Input,
        ),
    ] {
        telchar::persistence::create_store_lease(
            fixture.url(),
            lease_id,
            telchar::persistence::StoreLeaseOwnerKind::Request,
            "release-request",
            store_path,
            purpose,
        )
        .expect("lease persists");
    }

    let released = telchar::persistence::detach_request_and_release_leases(
        fixture.url(),
        "release-session",
        "release-request",
    )
    .expect("complete request lease set releases");

    assert_eq!(released.leases.len(), 2);
    assert!(released.leases.iter().all(|lease| {
        lease.state == telchar::persistence::StoreLeaseState::Released
            && lease.released_at.is_some()
    }));
    assert_eq!(
        telchar::persistence::read_request_attachment(
            fixture.url(),
            "release-session",
            "release-request",
        )
        .expect("attachment reads")
        .expect("attachment exists")
        .state,
        telchar::persistence::RequestAttachmentState::Detached
    );
}
