use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use telchar::daemon_services::{MaintenanceService, RecoveryMonitorService};

#[test]
fn maintenance_shutdown_interrupts_wait_and_joins_worker() {
    let runs = Arc::new(AtomicUsize::new(0));
    let worker_runs = Arc::clone(&runs);
    let mut service = MaintenanceService::start(Duration::from_secs(60), move || {
        worker_runs.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .expect("maintenance starts");

    let started = Instant::now();
    service.shutdown().expect("maintenance shuts down");

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(runs.load(Ordering::Relaxed), 0);
}

#[test]
fn recovery_monitor_completes_and_joins_after_authoritative_terminal_state() {
    let (completed_tx, completed_rx) = mpsc::channel();
    let mut service = RecoveryMonitorService::start(Duration::from_millis(1), move || {
        completed_tx.send(()).expect("completion reports");
        Ok(false)
    })
    .expect("recovery monitor starts");

    completed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("recovery runs");
    service.shutdown().expect("recovery monitor joins");
}

#[test]
fn background_service_failure_is_reported_without_exposing_detail() {
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let mut service = MaintenanceService::start(Duration::from_millis(1), move || {
        attempted_tx.send(()).expect("attempt reports");
        Err(io::Error::other("sensitive backend detail"))
    })
    .expect("maintenance starts");

    attempted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("maintenance runs");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match service.check() {
            Err(error) => {
                assert_eq!(error.to_string(), "output retention maintenance failed");
                break;
            }
            Ok(()) if Instant::now() < deadline => std::thread::yield_now(),
            Ok(()) => panic!("maintenance failure was not reported"),
        }
    }
    assert_eq!(
        service
            .shutdown()
            .expect_err("shutdown preserves failure")
            .to_string(),
        "output retention maintenance failed"
    );
}
