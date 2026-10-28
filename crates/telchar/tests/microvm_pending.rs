//! Shared pending-connection capacity is independent of execution reservations.

use telchar::microvm::rendezvous::PendingConnections;

#[test]
fn pending_capacity_bounds_global_and_backend_slots_and_releases_on_drop() {
    assert!(PendingConnections::new(0, 1).is_err());
    assert!(PendingConnections::new(2, 0).is_err());
    let limits = PendingConnections::new(2, 1).expect("limits");
    let mut first = limits.reserve().expect("first global slot");
    first
        .authenticate("backend-one")
        .expect("first backend slot");
    let mut second = limits.reserve().expect("second global slot");
    assert!(limits.reserve().is_err(), "global capacity bounded");
    assert!(
        second.authenticate("backend-one").is_err(),
        "backend capacity bounded"
    );
    second
        .authenticate("backend-two")
        .expect("independent backend slot");
    assert!(
        second.authenticate("backend-two").is_err(),
        "one assignment per slot"
    );
    drop(first);
    let mut replacement = limits.reserve().expect("global slot releases");
    replacement
        .authenticate("backend-one")
        .expect("backend slot releases");
    drop(replacement);
    drop(second);
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        let callers: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    let slot = limits.reserve();
                    barrier.wait();
                    slot.is_ok()
                })
            })
            .collect();
        assert_eq!(
            callers
                .into_iter()
                .map(|caller| caller.join().expect("caller"))
                .filter(|reserved| *reserved)
                .count(),
            2
        );
    });
    assert!(limits.reserve().is_ok(), "concurrent drops release slots");
}
