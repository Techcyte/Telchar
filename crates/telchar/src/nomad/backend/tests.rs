//! Checks monitoring deadlines independently of log notifications and HTTP duration.

use super::StatusPoll;
use std::time::{Duration, Instant};

#[test]
fn log_wakes_do_not_advance_status_poll() {
    let start = Instant::now();
    let interval = Duration::from_secs(2);
    let mut poll = StatusPoll::new(start, interval);
    assert!(poll.due(start));
    poll.completed(start);
    for milliseconds in [0, 1, 50, 500, 1999] {
        let now = start + Duration::from_millis(milliseconds);
        assert!(!poll.due(now));
        assert_eq!(
            poll.wait(now, start + interval * 10),
            interval - Duration::from_millis(milliseconds)
        );
    }
    assert!(poll.due(start + interval));
}

#[test]
fn slow_status_request_does_not_create_catch_up_polls() {
    let start = Instant::now();
    let interval = Duration::from_secs(2);
    let mut poll = StatusPoll::new(start, interval);
    let completed = start + interval * 4;
    poll.completed(completed);
    assert!(!poll.due(completed));
    assert_eq!(poll.wait(completed, completed + interval * 10), interval);
    assert!(poll.due(completed + interval));
}

#[test]
fn execution_deadline_caps_wait() {
    let start = Instant::now();
    let mut poll = StatusPoll::new(start, Duration::from_secs(2));
    assert_eq!(
        poll.wait(start, start + Duration::from_secs(1)),
        Duration::ZERO
    );
    poll.completed(start);
    let deadline = start + Duration::from_millis(100);
    assert_eq!(poll.wait(start, deadline), Duration::from_millis(100));
    assert_eq!(poll.wait(deadline, deadline), Duration::ZERO);
    assert_eq!(
        poll.wait(deadline + Duration::from_secs(1), deadline),
        Duration::ZERO
    );
}
