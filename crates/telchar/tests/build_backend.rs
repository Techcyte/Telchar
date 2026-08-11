use std::io;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use nix_worker_protocol::{ProtocolSessionLimits, WorkerReader};
use telchar::backend::{
    select_backend, BackendCapabilities, BackendKind, BackendPool, BackendTarget, BuildBackend,
    BuildExecution, BuildResult, BuildStatus, CancellationCapability, ExecutionRecovery,
    LogRecovery, OutputTrust,
};
use telchar::build_request::BuildRequest;
use telchar::deployment::DeploymentConfig;

#[test]
fn routing_selects_first_backend_with_matching_system_and_features() {
    let backends = [
        BackendTarget::new("local", BackendKind::Local, "x86_64-linux", ["kvm"])
            .expect("local backend is valid"),
        BackendTarget::new(
            "ssh-fast",
            BackendKind::StaticSsh,
            "x86_64-linux",
            ["big-parallel", "kvm"],
        )
        .expect("SSH backend is valid"),
        BackendTarget::new(
            "nomad-fallback",
            BackendKind::Nomad,
            "x86_64-linux",
            ["big-parallel", "kvm"],
        )
        .expect("Nomad backend is valid"),
    ];

    assert_eq!(
        select_backend(&backends, "x86_64-linux", &["kvm"])
            .expect("compatible backend exists")
            .name(),
        "local"
    );
    assert_eq!(
        select_backend(&backends, "x86_64-linux", &["big-parallel", "kvm"])
            .expect("compatible backend exists")
            .name(),
        "ssh-fast"
    );
    assert!(select_backend(&backends, "aarch64-linux", &[]).is_none());
    assert!(select_backend(&backends, "x86_64-linux", &["benchmark"]).is_none());
}

#[test]
fn backend_pool_waits_for_selected_backend_and_releases_permits() {
    let pool = BackendPool::new(
        vec![
            BackendTarget::new("local", BackendKind::Local, "x86_64-linux", ["kvm"])
                .expect("local backend is valid"),
            BackendTarget::new(
                "ssh",
                BackendKind::StaticSsh,
                "x86_64-linux",
                ["kvm", "big-parallel"],
            )
            .expect("SSH backend is valid"),
        ],
        vec![1, 1],
    )
    .expect("backend pool is valid");
    let held = pool
        .acquire("x86_64-linux", &["kvm"], Duration::from_secs(1))
        .expect("first local permit acquires");
    assert_eq!(held.target().name(), "local");

    let waiting_pool = pool.clone();
    let started = Arc::new(Barrier::new(2));
    let waiting_started = Arc::clone(&started);
    let waiter = thread::spawn(move || {
        waiting_started.wait();
        waiting_pool
            .acquire("x86_64-linux", &["kvm"], Duration::from_secs(1))
            .expect("released local permit acquires")
            .target()
            .name()
            .to_owned()
    });
    started.wait();
    thread::sleep(Duration::from_millis(25));
    assert!(!waiter.is_finished());
    drop(held);
    assert_eq!(waiter.join().expect("waiter joins"), "local");

    let ssh = pool
        .acquire(
            "x86_64-linux",
            &["big-parallel", "kvm"],
            Duration::from_secs(1),
        )
        .expect("feature-specific SSH permit acquires");
    assert_eq!(ssh.target().name(), "ssh");
}

#[test]
fn backend_pool_times_out_and_releases_after_failure_paths() {
    let pool = BackendPool::new(
        vec![
            BackendTarget::new("ssh", BackendKind::StaticSsh, "x86_64-linux", ["kvm"])
                .expect("SSH backend is valid"),
        ],
        vec![1],
    )
    .expect("backend pool is valid");
    let held = pool
        .acquire("x86_64-linux", &["kvm"], Duration::from_secs(1))
        .expect("first permit acquires");
    let started = Instant::now();
    let error = pool
        .acquire("x86_64-linux", &["kvm"], Duration::from_millis(25))
        .expect_err("busy backend times out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(20));
    drop(held);
    assert!(pool
        .acquire("x86_64-linux", &["kvm"], Duration::from_millis(25))
        .is_ok());
    assert_eq!(
        pool.acquire("aarch64-linux", &[], Duration::from_millis(25))
            .expect_err("incompatible backend is unavailable")
            .kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn backend_kinds_advertise_coordination_capabilities() {
    let output_only = BackendCapabilities::new(
        ExecutionRecovery::OutputOnly,
        CancellationCapability::ConnectionBound,
        LogRecovery::LiveOnly,
    );
    let adoptable = BackendCapabilities::new(
        ExecutionRecovery::Adoptable,
        CancellationCapability::Explicit,
        LogRecovery::LiveOnly,
    );

    for kind in [BackendKind::Local, BackendKind::StaticSsh] {
        let target = BackendTarget::new("builder", kind, "x86_64-linux", [] as [&str; 0])
            .expect("backend target is valid");
        assert_eq!(target.capabilities(), output_only);
    }

    let nomad = BackendTarget::new("nomad", BackendKind::Nomad, "x86_64-linux", [] as [&str; 0])
        .expect("Nomad target is valid");
    assert_eq!(nomad.capabilities(), adoptable);
}

#[test]
fn backend_contract_forwards_logs_and_returns_terminal_result() {
    let build = admitted_request();
    let execution = BuildExecution::new("request-1", &build, Duration::from_secs(30))
        .expect("execution is valid");
    let mut backend = FixtureBackend;
    let mut logs = Vec::new();

    let result = backend
        .execute_with_logs(
            &execution,
            &mut |chunk| {
                logs.extend_from_slice(chunk);
                Ok(())
            },
            &mut || Ok(false),
        )
        .expect("backend succeeds");

    assert_eq!(logs, b"building\n");
    assert_eq!(result.status(), BuildStatus::Built);
    assert_eq!(result.output_trust(), OutputTrust::TrustedExecutor);
    assert_eq!(result.outputs(), &[(b"out".to_vec(), output_path())]);
}

struct FixtureBackend;

impl BuildBackend for FixtureBackend {
    fn execute_with_logs(
        &mut self,
        execution: &BuildExecution<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        cancelled: &mut dyn FnMut() -> io::Result<bool>,
    ) -> io::Result<BuildResult> {
        assert_eq!(execution.request_id(), "request-1");
        assert_eq!(execution.timeout(), Duration::from_secs(30));
        assert_eq!(execution.build().system(), "x86_64-linux");
        assert!(!cancelled()?);
        logs(b"building\n")?;
        BuildResult::new(
            BuildStatus::Built,
            vec![(b"out".to_vec(), output_path())],
            OutputTrust::TrustedExecutor,
        )
    }
}

fn admitted_request() -> BuildRequest {
    let output = output_path();
    let mut wire = Vec::new();
    write_string(
        &mut wire,
        b"/nix/store/00000000000000000000000000000000-backend.drv",
    );
    write_integer(&mut wire, 1);
    write_string(&mut wire, b"out");
    write_string(&mut wire, &output);
    write_string(&mut wire, b"");
    write_string(&mut wire, b"");
    write_integer(&mut wire, 0);
    write_string(&mut wire, b"x86_64-linux");
    write_string(&mut wire, b"/bin/sh");
    write_integer(&mut wire, 2);
    write_string(&mut wire, b"-c");
    write_string(&mut wire, b"printf backend > $out");
    write_integer(&mut wire, 4);
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"backend".as_slice()),
        (b"out".as_slice(), output.as_slice()),
        (b"system".as_slice(), b"x86_64-linux".as_slice()),
    ] {
        write_string(&mut wire, key);
        write_string(&mut wire, value);
    }
    write_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
    let worker = reader
        .complete_build_derivation()
        .expect("worker request parses");
    BuildRequest::from_worker_request(
        &worker,
        &DeploymentConfig::parse("x86_64-linux", "").expect("deployment parses"),
    )
    .expect("request admits")
}

fn write_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_string(output: &mut Vec<u8>, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.extend_from_slice(&[0; 7][..(8 - value.len() % 8) % 8]);
}

fn output_path() -> Vec<u8> {
    b"/nix/store/33333333333333333333333333333333-backend".to_vec()
}
