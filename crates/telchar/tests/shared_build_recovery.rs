//! Tests shared build recovery contracts and failure boundaries, including claim.

use telchar_test_support::postgres::PostgresFixture;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use telchar::backend::{BackendCapabilities, BackendKind};
use telchar::persistence::{SharedBuild, SharedBuildState};
use telchar::shared_build::recovery::{
    AdoptedExecution, RecoveryBackend, SharedBuildOutputStore, reconcile_active_shared_builds,
};

const DERIVATION: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-recovery.drv";
const OUTPUT: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-recovery";

#[derive(Default)]
struct OutputStore {
    valid: BTreeSet<String>,
    checked: Vec<String>,
    validation_failure: bool,
}

impl SharedBuildOutputStore for OutputStore {
    fn retain_outputs(
        &mut self,
        _: &dyn telchar::persistence::DatabaseSource,
        _: &SharedBuild,
        _: Duration,
    ) -> io::Result<()> {
        Ok(())
    }
    fn validate_outputs(&mut self, _: &telchar::build::BuildRequest) -> io::Result<()> {
        if self.validation_failure {
            Err(io::Error::other("output authority rejected"))
        } else {
            Ok(())
        }
    }
    fn protect_hook_delivery(
        &mut self,
        database: &dyn telchar::persistence::DatabaseSource,
        build: &SharedBuild,
    ) -> io::Result<()> {
        for root in
            telchar::persistence::prepare_post_build_hook_roots(database, &build.derivation_path)
                .map_err(io::Error::other)?
        {
            telchar::persistence::retain_post_build_hook_root(database, &root)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool> {
        self.checked.extend(outputs.iter().cloned());
        Ok(outputs.iter().all(|output| self.valid.contains(output)))
    }
}

#[derive(Default)]
struct Backends {
    capabilities: BTreeMap<String, (BackendKind, BackendCapabilities)>,
    recovered_outputs: Vec<String>,
    recovery_result: bool,
    recovery_error: Option<io::ErrorKind>,
    adopted: Vec<String>,
    adoption: Option<AdoptedExecution>,
}

impl RecoveryBackend for Backends {
    fn capabilities(&self, backend_name: &str) -> Option<(BackendKind, BackendCapabilities)> {
        self.capabilities.get(backend_name).copied()
    }

    fn recover_outputs(&mut self, build: &SharedBuild) -> io::Result<bool> {
        self.recovered_outputs.push(build.backend_name.clone());
        match self.recovery_error {
            Some(kind) => Err(io::Error::new(kind, "output recovery failed")),
            None => Ok(self.recovery_result),
        }
    }

    fn adopt(&mut self, build: &SharedBuild) -> io::Result<AdoptedExecution> {
        self.adopted.push(
            build
                .backend_execution_id
                .clone()
                .expect("adoptable execution has durable identity"),
        );
        Ok(self.adoption.unwrap_or(AdoptedExecution::Monitoring))
    }
}

#[path = "support/build_request.rs"]
mod build_request;

fn recovery_request() -> telchar::build::BuildRequest {
    let mut value = serde_json::to_value(build_request::admitted_request()).unwrap();
    value["derivation_path"] = serde_json::json!(DERIVATION.as_bytes());
    value["expected_outputs"] = serde_json::json!([[b"out".as_slice(), OUTPUT.as_bytes()]]);
    value["output_authorities"][0]["path"] = serde_json::json!(OUTPUT.as_bytes());
    value["environment"][1][1] = serde_json::json!(b"recovery".as_slice());
    value["environment"][2][1] = serde_json::json!(OUTPUT.as_bytes());
    serde_json::from_value(value).unwrap()
}

fn claim(
    fixture: &PostgresFixture,
    backend_name: &str,
    backend_kind: BackendKind,
    backend_execution_id: Option<&str>,
) {
    let request = recovery_request();
    telchar::persistence::claim_shared_build_with_request(
        fixture.url(),
        DERIVATION,
        &request.shared_build_digest(),
        backend_name,
        backend_kind,
        backend_kind.capabilities(),
        backend_execution_id,
        &[OUTPUT],
        &request,
    )
    .expect("shared build claims");
}

#[test]
fn running_output_retention_cannot_create_request_before_collection() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    claim(&fixture, "nomad", BackendKind::Nomad, Some("job-running"));
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION).unwrap();
    let build = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .unwrap()
        .unwrap();
    let attempt = telchar::persistence::read_shared_build_attempt(fixture.url(), DERIVATION)
        .unwrap()
        .unwrap();
    let mut outputs =
        telchar::shared_build::recovery::GatewaySharedBuildOutputStore::with_endpoint(None);
    assert!(
        outputs
            .retain_outputs(
                &telchar::persistence::Database::connect(fixture.url()).unwrap(),
                &build,
                Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(
        telchar::persistence::read_build_request(
            fixture.url(),
            &format!("recovery-{}", attempt.attempt_id)
        )
        .unwrap()
        .is_none(),
        "running attempt acquired retention request before collection"
    );
}

#[test]
fn stale_output_retention_cannot_create_successor_request() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    claim(&fixture, "nomad", BackendKind::Nomad, Some("job-first"));
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION).unwrap();
    let predecessor = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .unwrap()
        .unwrap();
    telchar::persistence::retry_shared_build(
        fixture.url(),
        DERIVATION,
        "job-first",
        "job-second",
        "infrastructure-lost",
        &serde_json::json!({}),
    )
    .unwrap();
    let successor = telchar::persistence::read_shared_build_attempt(fixture.url(), DERIVATION)
        .unwrap()
        .unwrap();
    let mut outputs =
        telchar::shared_build::recovery::GatewaySharedBuildOutputStore::with_endpoint(None);
    assert!(
        outputs
            .retain_outputs(
                &telchar::persistence::Database::connect(fixture.url()).unwrap(),
                &predecessor,
                Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(
        telchar::persistence::read_build_request(
            fixture.url(),
            &format!("recovery-{}", successor.attempt_id)
        )
        .unwrap()
        .is_none(),
        "stale retention attributed request to successor"
    );
}

#[test]
fn recovery_traverses_monitoring_records_beyond_first_page() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    for index in 0..257 {
        let path = format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-recovery-{index:03}.drv");
        telchar::persistence::claim_shared_build(
            fixture.url(),
            &path,
            &[1; 32],
            "nomad",
            BackendKind::Nomad,
            BackendKind::Nomad.capabilities(),
            Some(&format!("job-{index}")),
            &[OUTPUT],
        )
        .unwrap();
        telchar::persistence::start_shared_build(fixture.url(), &path).unwrap();
        if index % 2 == 0 {
            telchar::persistence::collect_shared_build(fixture.url(), &path).unwrap();
        }
    }
    let mut outputs = OutputStore::default();
    let mut backends = Backends::default();
    backends.capabilities.insert(
        "nomad".into(),
        (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
    );
    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(60),
        &mut outputs,
        &mut backends,
    )
    .unwrap();
    assert_eq!(outcome.monitoring, 257);
    assert_eq!(backends.adopted.len(), 257);
    assert_eq!(backends.adopted.iter().collect::<BTreeSet<_>>().len(), 257);
}

#[test]
fn invalid_registered_outputs_cannot_become_recovered_success() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    claim(&fixture, "local", BackendKind::Local, None);
    let mut outputs = OutputStore {
        valid: BTreeSet::from([OUTPUT.to_owned()]),
        validation_failure: true,
        ..Default::default()
    };
    let result = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(60),
        &mut outputs,
        &mut Backends::default(),
    );
    assert!(result.is_err());
    assert_ne!(
        telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
            .unwrap()
            .unwrap()
            .state,
        SharedBuildState::Succeeded
    );
}

#[test]
fn complete_expected_outputs_win_before_backend_recovery() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(&fixture, "missing-backend", BackendKind::Local, None);
    let mut outputs = OutputStore {
        valid: BTreeSet::from([OUTPUT.to_owned()]),
        ..OutputStore::default()
    };
    let mut backends = Backends::default();
    let database = telchar::persistence::Database::connect(fixture.url())
        .unwrap()
        .with_post_build_hooks(true);
    let outcome = reconcile_active_shared_builds(
        &database,
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");
    let hook_delivery = telchar::persistence::claim_post_build_hooks(
        &database,
        "recovery",
        Duration::from_secs(30),
    )
    .unwrap()
    .unwrap();
    assert_eq!(hook_delivery.output_paths, [OUTPUT]);

    assert_eq!(outcome.succeeded, 1);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.monitoring, 0);
    assert_eq!(outputs.checked, [OUTPUT]);
    assert!(backends.adopted.is_empty());
    let build = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .expect("shared build reads")
        .expect("shared build exists");
    assert_eq!(build.state, SharedBuildState::Succeeded);
    assert_eq!(
        build.result_metadata,
        Some(
            serde_json::json!({"status": "built", "outputs": [{"name": "out", "path": OUTPUT}], "recovered": true})
        )
    );
}

#[test]
fn static_ssh_output_only_execution_imports_exact_remote_outputs() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(&fixture, "ssh", BackendKind::StaticSsh, None);
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "ssh".to_owned(),
            (
                BackendKind::StaticSsh,
                BackendKind::StaticSsh.capabilities(),
            ),
        )]),
        recovery_result: true,
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.succeeded, 1);
    assert_eq!(backends.recovered_outputs, ["ssh"]);
    let build = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .expect("shared build reads")
        .expect("shared build exists");
    assert_eq!(build.state, SharedBuildState::Succeeded);
}

#[test]
fn static_ssh_output_recovery_failure_fails_cleanly() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(&fixture, "ssh", BackendKind::StaticSsh, None);
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "ssh".to_owned(),
            (
                BackendKind::StaticSsh,
                BackendKind::StaticSsh.capabilities(),
            ),
        )]),
        recovery_error: Some(io::ErrorKind::TimedOut),
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.failed, 1);
    assert_eq!(backends.recovered_outputs, ["ssh"]);
    let build = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .expect("shared build reads")
        .expect("shared build exists");
    assert_eq!(build.state, SharedBuildState::Failed);
    assert_eq!(
        build.failure_classification.as_deref(),
        Some("restart-recovery-failed")
    );
}

#[test]
fn output_only_execution_uses_configured_backend_recovery_capability() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(&fixture, "local", BackendKind::Local, None);
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "local".to_owned(),
            (BackendKind::Local, BackendKind::Local.capabilities()),
        )]),
        recovery_result: true,
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.succeeded, 1);
    assert_eq!(backends.recovered_outputs, ["local"]);
    let build = telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
        .expect("shared build reads")
        .expect("shared build exists");
    assert_eq!(build.state, SharedBuildState::Succeeded);
}

#[test]
fn adoptable_execution_resumes_only_with_matching_persisted_capabilities() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(
        &fixture,
        "nomad",
        BackendKind::Nomad,
        Some("telchar-recovery-job"),
    );
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "nomad".to_owned(),
            (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
        )]),
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.monitoring, 1);
    assert_eq!(outcome.monitoring_derivations, [DERIVATION]);
    assert_eq!(backends.adopted, ["telchar-recovery-job"]);
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
            .expect("shared build reads")
            .expect("shared build exists")
            .state,
        SharedBuildState::Running
    );
}

#[test]
fn recovery_adopts_the_rotated_nomad_retry_identity() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(
        &fixture,
        "nomad",
        BackendKind::Nomad,
        Some("telchar-recovery-job-1"),
    );
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    telchar::persistence::retry_shared_build(
        fixture.url(),
        DERIVATION,
        "telchar-recovery-job-1",
        "telchar-recovery-job-2",
        "nomad-infrastructure-lost",
        &serde_json::json!({"reason": "allocation-missing"}),
    )
    .expect("shared build retries");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "nomad".to_owned(),
            (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
        )]),
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.monitoring, 1);
    assert_eq!(backends.adopted, ["telchar-recovery-job-2"]);
    assert_eq!(
        telchar::persistence::read_shared_build_attempt(fixture.url(), DERIVATION)
            .expect("attempt reads")
            .expect("attempt exists")
            .ordinal,
        2
    );
}

#[test]
fn adopted_execution_can_be_reconciled_to_terminal_state() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(
        &fixture,
        "nomad",
        BackendKind::Nomad,
        Some("telchar-recovery-job"),
    );
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "nomad".to_owned(),
            (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
        )]),
        adoption: Some(AdoptedExecution::Succeeded),
        ..Backends::default()
    };

    let outcome = telchar::shared_build::recovery::reconcile_adopted_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &[DERIVATION.to_owned()],
        &mut outputs,
        &mut backends,
    )
    .expect("adopted execution reconciliation succeeds");

    assert_eq!(outcome.failed, 1);
    assert_eq!(outcome.monitoring, 0);
    assert!(outcome.monitoring_derivations.is_empty());
    assert_eq!(backends.adopted, ["telchar-recovery-job"]);
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
            .expect("shared build reads")
            .expect("shared build exists")
            .state,
        SharedBuildState::Failed
    );
}

#[test]
fn attempt_execution_identity_disagreement_fails_closed() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(
        &fixture,
        "nomad",
        BackendKind::Nomad,
        Some("telchar-recovery-job"),
    );
    telchar::persistence::start_shared_build(fixture.url(), DERIVATION)
        .expect("shared build starts");
    fixture
        .connect()
        .execute(
            "UPDATE shared_build_attempts SET backend_execution_id = 'different-job' WHERE derivation_path = $1",
            &[&DERIVATION],
        )
        .expect("attempt identity diverges");
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "nomad".to_owned(),
            (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
        )]),
        ..Backends::default()
    };

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

    assert_eq!(outcome.failed, 1);
    assert!(backends.adopted.is_empty());
    assert_eq!(
        telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
            .expect("shared build reads")
            .expect("shared build exists")
            .state,
        SharedBuildState::Failed
    );
}

#[test]
fn capability_disagreement_and_missing_adopted_execution_fail_closed() {
    for adoption in [None, Some(AdoptedExecution::Missing)] {
        let fixture = PostgresFixture::start();
        telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
        claim(
            &fixture,
            "nomad",
            BackendKind::Nomad,
            Some("telchar-missing-job"),
        );
        let mut outputs = OutputStore::default();
        let capabilities = adoption.map(|_| {
            BTreeMap::from([(
                "nomad".to_owned(),
                (BackendKind::Nomad, BackendKind::Nomad.capabilities()),
            )])
        });
        let mut backends = Backends {
            capabilities: capabilities.unwrap_or_default(),
            adoption,
            ..Backends::default()
        };

        let outcome = reconcile_active_shared_builds(
            fixture.url(),
            Duration::from_secs(3_600),
            &mut outputs,
            &mut backends,
        )
        .expect("reconciliation succeeds");

        assert_eq!(outcome.failed, 1);
        assert_eq!(
            telchar::persistence::read_shared_build(fixture.url(), DERIVATION)
                .expect("shared build reads")
                .expect("shared build exists")
                .state,
            SharedBuildState::Failed
        );
    }
}
