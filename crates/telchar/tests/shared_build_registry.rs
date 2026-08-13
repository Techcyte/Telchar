//! Tests shared build registry contracts and failure boundaries, including identical connected builds share one execution.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use telchar::backend::{BuildResult, BuildStatus, OutputTrust};
use telchar::shared_build::{SharedBuildAccess, SharedBuildRegistry, SharedBuildTerminalFailure};

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

    assert!(
        failures
            .iter()
            .all(|result| *result == Err(SharedBuildTerminalFailure::Backend))
    );
    assert_eq!(registry.active_build_count(), 0);
    assert!(
        registry
            .execute_or_wait("failed-build", successful_result)
            .is_ok()
    );
}

#[test]
fn explicit_leader_and_follower_share_terminal_result() {
    let registry = SharedBuildRegistry::new();

    let leader = match registry.acquire("shared-build") {
        SharedBuildAccess::Leader(leader) => leader,
        SharedBuildAccess::Follower(_) => panic!("first acquisition must lead"),
    };
    let follower = match registry.acquire("shared-build") {
        SharedBuildAccess::Follower(follower) => follower,
        SharedBuildAccess::Leader(_) => panic!("second acquisition must follow"),
    };

    let expected = successful_result().expect("result constructs");
    assert_eq!(leader.complete(Ok(expected.clone())), Ok(expected.clone()));

    assert_eq!(follower.wait(), Ok(expected));
    assert_eq!(registry.active_build_count(), 0);
}

#[test]
fn follower_wait_is_bounded_without_cancelling_the_leader() {
    let registry = SharedBuildRegistry::new();

    let leader = match registry.acquire("slow-build") {
        SharedBuildAccess::Leader(leader) => leader,
        SharedBuildAccess::Follower(_) => panic!("first acquisition must lead"),
    };
    let follower = match registry.acquire("slow-build") {
        SharedBuildAccess::Follower(follower) => follower,
        SharedBuildAccess::Leader(_) => panic!("second acquisition must follow"),
    };

    assert_eq!(follower.wait_timeout(Duration::from_millis(10)), None);
    assert_eq!(registry.active_build_count(), 1);
    leader
        .complete(successful_result())
        .expect("leader still completes");
    assert_eq!(registry.active_build_count(), 0);
}

#[test]
fn dropped_leader_fails_followers_and_releases_build_key() {
    let registry = SharedBuildRegistry::new();

    let leader = match registry.acquire("abandoned-build") {
        SharedBuildAccess::Leader(leader) => leader,
        SharedBuildAccess::Follower(_) => panic!("first acquisition must lead"),
    };
    let follower = match registry.acquire("abandoned-build") {
        SharedBuildAccess::Follower(follower) => follower,
        SharedBuildAccess::Leader(_) => panic!("second acquisition must follow"),
    };

    drop(leader);

    assert_eq!(follower.wait(), Err(SharedBuildTerminalFailure::Internal));
    assert!(matches!(
        registry.acquire("abandoned-build"),
        SharedBuildAccess::Leader(_)
    ));
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
