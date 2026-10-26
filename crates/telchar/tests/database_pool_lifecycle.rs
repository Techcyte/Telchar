use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use telchar_test_support::postgres::PostgresFixture;

fn pool_workers() -> BTreeSet<std::path::PathBuf> {
    std::fs::read_dir("/proc/self/task")
        .expect("process threads list")
        .filter_map(|entry| {
            let path = entry.expect("thread entry reads").path();
            std::fs::read_to_string(path.join("comm"))
                .ok()
                .filter(|name| name.starts_with("r2d2-worker-"))
                .map(|_| path)
        })
        .collect()
}

#[test]
fn pool_workers_retire_after_last_connection_is_returned() {
    let fixture = PostgresFixture::start();
    let initial_workers = pool_workers();
    let database =
        telchar::persistence::Database::connect(fixture.url()).expect("database pool starts");
    let mut connection = database
        .connection()
        .expect("database connection checks out");
    let workers = pool_workers()
        .difference(&initial_workers)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(!workers.is_empty(), "database pool starts workers");

    drop(database);
    let value: i32 = connection
        .query_one("SELECT 42::integer", &[])
        .expect("checked-out connection keeps pool alive")
        .get(0);
    assert_eq!(value, 42);
    drop(connection);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = workers
            .iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dropped database pool retains worker threads: {remaining:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
