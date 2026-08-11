use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use telchar::backend::{BuildResult, BuildStatus, OutputTrust};
use telchar::shared_build::{SharedBuildRegistry, SharedBuildTerminalFailure};

#[test]
fn identical_connected_builds_share_one_execution() {
    let registry = Arc::new(SharedBuildRegistry::new());
    let executions = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(3));

    let follower_notifications = Arc::new(AtomicUsize::new(0));
    let results = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let registry = Arc::clone(&registry);
            let executions = Arc::clone(&executions);
            let barrier = Arc::clone(&barrier);
            let follower_notifications = Arc::clone(&follower_notifications);
            handles.push(scope.spawn(move || {
                barrier.wait();
                registry
                    .execute_or_wait_with_follower(
                        "shared-build",
                        || {
                            follower_notifications.fetch_add(1, Ordering::SeqCst);
                        },
                        || {
                            executions.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(50));
                            successful_result()
                        },
                    )
                    .expect("shared build succeeds")
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("build thread joins"))
            .collect::<Vec<_>>()
    });

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(follower_notifications.load(Ordering::SeqCst), 1);
    assert_eq!(results[0], results[1]);
    assert_eq!(registry.active_build_count(), 0);
}

#[test]
fn shared_failure_wakes_all_waiters_and_later_request_can_execute() {
    let registry = Arc::new(SharedBuildRegistry::new());
    let barrier = Arc::new(Barrier::new(3));

    let failures = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            handles.push(scope.spawn(move || {
                barrier.wait();
                registry.execute_or_wait("failed-build", || {
                    thread::sleep(Duration::from_millis(50));
                    Err(SharedBuildTerminalFailure::Backend)
                })
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("build thread joins"))
            .collect::<Vec<_>>()
    });

    assert!(failures
        .iter()
        .all(|result| *result == Err(SharedBuildTerminalFailure::Backend)));
    assert_eq!(registry.active_build_count(), 0);
    assert!(registry
        .execute_or_wait("failed-build", successful_result)
        .is_ok());
}

#[test]
fn distinct_builds_execute_independently() {
    let registry = SharedBuildRegistry::new();
    let executions = AtomicUsize::new(0);

    registry
        .execute_or_wait("build-a", || {
            executions.fetch_add(1, Ordering::SeqCst);
            successful_result()
        })
        .expect("first build succeeds");
    registry
        .execute_or_wait("build-b", || {
            executions.fetch_add(1, Ordering::SeqCst);
            successful_result()
        })
        .expect("second build succeeds");

    assert_eq!(executions.load(Ordering::SeqCst), 2);
}

fn successful_result() -> Result<BuildResult, SharedBuildTerminalFailure> {
    BuildResult::new(
        BuildStatus::Built,
        vec![(
            b"out".to_vec(),
            b"/nix/store/11111111111111111111111111111111-shared".to_vec(),
        )],
        OutputTrust::TrustedExecutor,
    )
    .map_err(|_| SharedBuildTerminalFailure::Internal)
}
