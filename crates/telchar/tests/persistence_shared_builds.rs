//! Tests shared-build, build-request, and executor persistence contracts and failure boundaries.

mod support;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use support::postgres::PostgresFixture;
use telchar::backend::{
    BackendCapabilities, BackendKind, CancellationCapability, ExecutionRecovery, LogRecovery,
};
#[test]
fn requester_reference_is_deterministic_and_component_separated() {
    let requester = telchar::service::ipc::RequesterMetadata {
        credential_id: "ssh-pubkey:fixture".into(),
        audit_subject: "fixture".into(),
        quota_subject: "ssh-pubkey:fixture".into(),
    };

    let reference = telchar::persistence::requester_reference(&requester);

    assert_eq!(
        reference,
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e"
    );
    assert_eq!(reference.len(), 64);
    assert!(reference
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    assert_ne!(
        telchar::persistence::requester_reference(&telchar::service::ipc::RequesterMetadata {
            credential_id: "ab".into(),
            audit_subject: "c".into(),
            quota_subject: "quota".into(),
        }),
        telchar::persistence::requester_reference(&telchar::service::ipc::RequesterMetadata {
            credential_id: "a".into(),
            audit_subject: "bc".into(),
            quota_subject: "quota".into(),
        })
    );
    for requester in [
        telchar::service::ipc::RequesterMetadata {
            credential_id: "other-credential".into(),
            ..requester.clone()
        },
        telchar::service::ipc::RequesterMetadata {
            audit_subject: "other-audit".into(),
            ..requester.clone()
        },
        telchar::service::ipc::RequesterMetadata {
            quota_subject: "other-quota".into(),
            ..requester.clone()
        },
    ] {
        assert_ne!(
            telchar::persistence::requester_reference(&requester),
            reference
        );
    }
}

fn durable_build_request() -> telchar::build::BuildRequest {
    use nix_worker_protocol::{
        write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerOperation,
        WorkerReader,
    };

    let derivation = b"/nix/store/11111111111111111111111111111111-shared.drv";
    let output = b"/nix/store/22222222222222222222222222222222-shared";
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 36);
    write_worker_byte_string(&mut wire, derivation);
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"out");
    write_worker_byte_string(&mut wire, output);
    write_worker_byte_string(&mut wire, b"r:sha256");
    write_worker_byte_string(
        &mut wire,
        b"0000000000000000000000000000000000000000000000000000000000000000",
    );
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(
        &mut wire,
        b"/nix/store/33333333333333333333333333333333-input",
    );
    write_worker_byte_string(&mut wire, b"x86_64-linux");
    write_worker_byte_string(&mut wire, b"/bin/sh");
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"-e");
    write_worker_integer(&mut wire, 4);
    for (key, value) in [
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"shared".as_slice()),
        (b"out".as_slice(), output.as_slice()),
    ] {
        write_worker_byte_string(&mut wire, key);
        write_worker_byte_string(&mut wire, value);
    }
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation reads"),
        WorkerOperation::BuildDerivation
    );
    let request = reader
        .complete_build_derivation()
        .expect("build request decodes");
    telchar::build::BuildRequest::from_worker_request(
        &request,
        &[telchar::backend::BackendTarget::new(
            "nomad",
            BackendKind::Nomad,
            "x86_64-linux",
            Vec::<String>::new(),
        )
        .expect("backend target creates")],
    )
    .expect("durable build request admits")
}

#[test]
fn shared_build_persists_exact_admitted_build_request() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let request = durable_build_request();
    let digest = request.shared_build_digest();

    let claim = telchar::persistence::claim_shared_build_with_request(
        fixture.url(),
        "/nix/store/11111111111111111111111111111111-shared.drv",
        &digest,
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-shared"),
        &["/nix/store/22222222222222222222222222222222-shared"],
        &request,
    )
    .expect("shared build claim succeeds");

    assert_eq!(
        claim.build.build_request.as_ref(),
        Some(&request),
        "exact admitted BuildDerivation request persists"
    );
    telchar::persistence::start_shared_build(
        fixture.url(),
        "/nix/store/11111111111111111111111111111111-shared.drv",
    )
    .expect("shared build starts");
    let loaded = telchar::persistence::read_shared_build_by_execution(
        fixture.url(),
        "nomad",
        "telchar-build-shared",
    )
    .expect("shared build reads")
    .expect("shared build exists");
    assert_eq!(loaded.build_request.as_ref(), Some(&request));
    let authority = &loaded
        .build_request
        .as_ref()
        .expect("build request persists")
        .output_authorities()[0];
    assert_eq!(authority.hash_algorithm(), b"r:sha256");
    assert_eq!(
        authority.hash(),
        b"0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[test]
fn equivalent_shared_build_claims_have_one_owner() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let database_url = fixture.url().to_owned();
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let claims = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let database_url = database_url.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(scope.spawn(move || {
                barrier.wait();
                telchar::persistence::claim_shared_build(
                    &database_url,
                    "/nix/store/11111111111111111111111111111111-shared.drv",
                    &[7_u8; 32],
                    "ssh-fast",
                    BackendKind::StaticSsh,
                    BackendCapabilities::new(
                        ExecutionRecovery::OutputOnly,
                        CancellationCapability::ConnectionBound,
                        LogRecovery::LiveOnly,
                    ),
                    None,
                    &["/nix/store/22222222222222222222222222222222-shared"],
                )
                .expect("shared build claim succeeds")
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread joins"))
            .collect::<Vec<_>>()
    });

    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ownership == telchar::persistence::SharedBuildOwnership::Claimed)
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ownership == telchar::persistence::SharedBuildOwnership::Joined)
            .count(),
        1
    );
    assert!(claims.iter().all(|claim| {
        claim.build.derivation_path == "/nix/store/11111111111111111111111111111111-shared.drv"
            && claim.build.state == telchar::persistence::SharedBuildState::Claimed
            && claim.build.backend_name == "ssh-fast"
            && claim.build.capabilities.execution_recovery() == ExecutionRecovery::OutputOnly
            && claim.build.backend_execution_id.is_none()
    }));
}

#[test]
fn shared_build_claim_rejects_digest_conflict_and_requires_adoptable_identity() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/33333333333333333333333333333333-conflict.drv";
    let output_path = "/nix/store/44444444444444444444444444444444-conflict";
    let output_only = BackendCapabilities::new(
        ExecutionRecovery::OutputOnly,
        CancellationCapability::ConnectionBound,
        LogRecovery::LiveOnly,
    );

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[1_u8; 32],
        "local",
        BackendKind::Local,
        output_only,
        None,
        &[output_path],
    )
    .expect("first shared build claim succeeds");

    assert_eq!(
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation_path,
            &[2_u8; 32],
            "local",
            BackendKind::Local,
            output_only,
            None,
            &[output_path],
        )
        .expect_err("digest disagreement fails closed")
        .failure(),
        telchar::persistence::SharedBuildFailure::Conflict
    );
    assert_eq!(
        telchar::persistence::claim_shared_build(
            fixture.url(),
            "/nix/store/55555555555555555555555555555555-adoptable.drv",
            &[3_u8; 32],
            "nomad",
            BackendKind::Nomad,
            BackendCapabilities::new(
                ExecutionRecovery::Adoptable,
                CancellationCapability::Explicit,
                LogRecovery::LiveOnly,
            ),
            None,
            &["/nix/store/66666666666666666666666666666666-adoptable"],
        )
        .expect_err("adoptable execution requires a stable identity")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}

#[test]
fn shared_build_lifecycle_persists_immutable_terminal_success() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/77777777777777777777777777777777-lifecycle.drv";
    let output_path = "/nix/store/88888888888888888888888888888888-lifecycle";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[4_u8; 32],
        "nomad",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some("telchar-build-lifecycle"),
        &[output_path],
    )
    .expect("shared build claim succeeds");
    assert_eq!(
        telchar::persistence::start_shared_build(fixture.url(), derivation_path)
            .expect("shared build starts")
            .state,
        telchar::persistence::SharedBuildState::Running
    );
    assert_eq!(
        telchar::persistence::collect_shared_build(fixture.url(), derivation_path)
            .expect("shared build collects")
            .state,
        telchar::persistence::SharedBuildState::Collecting
    );
    let completed = telchar::persistence::complete_shared_build_success(
        fixture.url(),
        derivation_path,
        &serde_json::json!({"status": "built", "output_count": 1}),
        Duration::from_secs(3_600),
    )
    .expect("shared build succeeds");

    assert_eq!(
        completed.state,
        telchar::persistence::SharedBuildState::Succeeded
    );
    assert_eq!(
        completed.result_metadata,
        Some(serde_json::json!({"status": "built", "output_count": 1}))
    );
    assert!(completed.failure_classification.is_none());
    assert!(completed.started_at.is_some());
    assert!(completed.collecting_at.is_some());
    assert!(completed.completed_at.is_some());
    assert!(completed.expires_at.is_some());

    fixture.restart();
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), derivation_path)
            .expect("shared build reads")
            .expect("shared build exists"),
        completed
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "infrastructure-failure",
            &serde_json::json!({"stage": "monitor"}),
            Duration::from_secs(3_600),
        )
        .expect_err("terminal shared build is immutable")
        .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
}

#[test]
fn shared_build_lifecycle_rejects_skipped_transitions_and_bounds_terminal_data() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/99999999999999999999999999999999-lifecycle.drv";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[5_u8; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-lifecycle"],
    )
    .expect("shared build claim succeeds");

    assert_eq!(
        telchar::persistence::collect_shared_build(fixture.url(), derivation_path)
            .expect_err("claimed build cannot collect")
            .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
    telchar::persistence::start_shared_build(fixture.url(), derivation_path)
        .expect("shared build starts");
    assert_eq!(
        telchar::persistence::complete_shared_build_success(
            fixture.url(),
            derivation_path,
            &serde_json::json!({"status": "built"}),
            Duration::from_secs(3_600),
        )
        .expect_err("running build cannot skip collection")
        .failure(),
        telchar::persistence::SharedBuildFailure::InvalidState
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "",
            &serde_json::json!({}),
            Duration::from_secs(3_600),
        )
        .expect_err("empty classification rejects")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
    assert_eq!(
        telchar::persistence::complete_shared_build_failure(
            fixture.url(),
            derivation_path,
            "build-failure",
            &serde_json::json!([]),
            Duration::from_secs(3_600),
        )
        .expect_err("non-object result rejects")
        .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}

#[test]
fn later_request_replaces_failed_shared_build_without_automatic_retry() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    let derivation_path = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-retry.drv";
    let output_path = "/nix/store/cccccccccccccccccccccccccccccccc-retry";

    telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[6_u8; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[output_path],
    )
    .expect("first shared build claims");
    telchar::persistence::complete_shared_build_failure(
        fixture.url(),
        derivation_path,
        "infrastructure-failure",
        &serde_json::json!({"stage": "execute"}),
        Duration::from_secs(3_600),
    )
    .expect("first shared build fails");

    let replacement = telchar::persistence::claim_shared_build(
        fixture.url(),
        derivation_path,
        &[7_u8; 32],
        "ssh-fast",
        BackendKind::StaticSsh,
        BackendKind::StaticSsh.capabilities(),
        None,
        &[output_path],
    )
    .expect("later independent request claims a replacement");

    assert_eq!(
        replacement.ownership,
        telchar::persistence::SharedBuildOwnership::Claimed
    );
    assert_eq!(
        replacement.build.state,
        telchar::persistence::SharedBuildState::Claimed
    );
    assert_eq!(replacement.build.request_digest, [7_u8; 32]);
    assert_eq!(replacement.build.backend_name, "ssh-fast");
    assert!(replacement.build.result_metadata.is_none());
    assert!(replacement.build.completed_at.is_none());
    assert_eq!(
        fixture
            .connect()
            .query_one(
                "SELECT count(*) FROM shared_builds WHERE derivation_path = $1",
                &[&derivation_path],
            )
            .expect("shared build count reads")
            .get::<_, i64>(0),
        1
    );
}

#[test]
fn active_shared_builds_survive_restart_in_deterministic_order() {
    let mut fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    for (derivation_path, digest) in [
        (
            "/nix/store/dddddddddddddddddddddddddddddddd-active-a.drv",
            [8_u8; 32],
        ),
        (
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active-b.drv",
            [9_u8; 32],
        ),
    ] {
        telchar::persistence::claim_shared_build(
            fixture.url(),
            derivation_path,
            &digest,
            "nomad",
            BackendKind::Nomad,
            BackendKind::Nomad.capabilities(),
            Some(if digest[0] == 8 { "nomad-a" } else { "nomad-b" }),
            &["/nix/store/ffffffffffffffffffffffffffffffff-active"],
        )
        .expect("active shared build claims");
    }
    telchar::persistence::start_shared_build(
        fixture.url(),
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-active-b.drv",
    )
    .expect("second shared build starts");

    fixture.restart();
    let active = telchar::persistence::read_active_shared_builds(fixture.url(), 16)
        .expect("active shared builds read");

    assert_eq!(active.len(), 2);
    assert!(active[0].created_at <= active[1].created_at);
    assert_eq!(
        active.iter().map(|build| build.state).collect::<Vec<_>>(),
        [
            telchar::persistence::SharedBuildState::Claimed,
            telchar::persistence::SharedBuildState::Running,
        ]
    );
    assert_eq!(
        telchar::persistence::read_active_shared_builds(fixture.url(), 0)
            .expect_err("zero active-build limit rejects")
            .failure(),
        telchar::persistence::SharedBuildFailure::Configuration
    );
}
