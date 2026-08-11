mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use telchar::backend::{BackendCapabilities, BackendKind};
use telchar::persistence::{SharedBuild, SharedBuildState};
use telchar::shared_build_recovery::{
    reconcile_active_shared_builds, AdoptedExecution, RecoveryBackend, SharedBuildOutputStore,
};

use support::postgres::PostgresFixture;

const DERIVATION: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-recovery.drv";
const OUTPUT: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-recovery";

#[derive(Default)]
struct OutputStore {
    valid: BTreeSet<String>,
    checked: Vec<String>,
}

impl SharedBuildOutputStore for OutputStore {
    fn contains_all(&mut self, outputs: &[String]) -> io::Result<bool> {
        self.checked.extend(outputs.iter().cloned());
        Ok(outputs.iter().all(|output| self.valid.contains(output)))
    }
}

#[derive(Default)]
struct Backends {
    capabilities: BTreeMap<String, (BackendKind, BackendCapabilities)>,
    adopted: Vec<String>,
    adoption: Option<AdoptedExecution>,
}

impl RecoveryBackend for Backends {
    fn capabilities(&self, backend_name: &str) -> Option<(BackendKind, BackendCapabilities)> {
        self.capabilities.get(backend_name).copied()
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

fn claim(
    fixture: &PostgresFixture,
    backend_name: &str,
    backend_kind: BackendKind,
    backend_execution_id: Option<&str>,
) {
    telchar::persistence::claim_shared_build(
        fixture.url(),
        DERIVATION,
        &[1_u8; 32],
        backend_name,
        backend_kind,
        backend_kind.capabilities(),
        backend_execution_id,
        &[OUTPUT],
    )
    .expect("shared build claims");
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

    let outcome = reconcile_active_shared_builds(
        fixture.url(),
        Duration::from_secs(3_600),
        &mut outputs,
        &mut backends,
    )
    .expect("reconciliation succeeds");

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
        Some(serde_json::json!({"outputs": [OUTPUT], "recovered": true}))
    );
}

#[test]
fn output_only_execution_without_outputs_fails() {
    let fixture = PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).expect("migration succeeds");
    claim(&fixture, "local", BackendKind::Local, None);
    let mut outputs = OutputStore::default();
    let mut backends = Backends {
        capabilities: BTreeMap::from([(
            "local".to_owned(),
            (BackendKind::Local, BackendKind::Local.capabilities()),
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
