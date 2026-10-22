use telchar_test_support::postgres::PostgresFixture;

#[derive(Clone)]
struct TestDatabase {
    database: telchar::persistence::Database,
}

impl telchar::persistence::DatabaseSource for TestDatabase {
    fn database(
        &self,
    ) -> Result<telchar::persistence::Database, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.database.clone())
    }
}

#[test]
fn database_reuses_an_established_connection() {
    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url())
        .expect("database connection pool starts");

    let first_backend = database
        .connection()
        .expect("first database connection checks out")
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("first backend identity reads")
        .get::<_, i32>(0);
    let second_backend = database
        .connection()
        .expect("second database connection checks out")
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("second backend identity reads")
        .get::<_, i32>(0);

    assert_eq!(first_backend, second_backend);
}

#[test]
fn database_replaces_stale_connections_after_restart() {
    let mut fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    database
        .connection()
        .unwrap()
        .batch_execute("CREATE TABLE restart_rows (id integer PRIMARY KEY); INSERT INTO restart_rows VALUES (1)")
        .unwrap();

    fixture.restart();

    assert_eq!(
        database
            .connection()
            .expect("checkout replaces stale connections")
            .query_one("SELECT id FROM restart_rows", &[])
            .unwrap()
            .get::<_, i32>(0),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_database_retires_background_tasks() {
    fn pool_threads() -> std::collections::BTreeSet<std::path::PathBuf> {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                std::fs::read_to_string(path.join("comm"))
                    .is_ok_and(|name| name.starts_with("r2d2-worker-"))
            })
            .collect()
    }

    let fixture = PostgresFixture::start();
    let before = pool_threads();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    database
        .connection()
        .unwrap()
        .simple_query("SELECT 1")
        .unwrap();
    let created = pool_threads()
        .difference(&before)
        .cloned()
        .collect::<Vec<_>>();
    assert!(!created.is_empty(), "pool background tasks are observed");
    drop(database);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while created.iter().any(|path| path.exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "dropped database retains background tasks: {:?}",
            pool_threads()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
struct ResumeBackend(i32);

#[cfg(target_os = "linux")]
impl ResumeBackend {
    fn stop(backend: i32) -> Self {
        let resume = Self(backend);
        assert_eq!(unsafe { libc::kill(backend, libc::SIGSTOP) }, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let status = std::fs::read_to_string(format!("/proc/{backend}/status")).unwrap();
            if status.lines().any(|line| line.starts_with("State:\tT")) {
                return resume;
            }
            assert!(std::time::Instant::now() < deadline, "backend did not stop");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ResumeBackend {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0, libc::SIGCONT) };
    }
}

#[cfg(target_os = "linux")]
#[test]
fn database_checkout_bounds_stalled_validation() {
    use std::time::{Duration, Instant};

    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    let mut held = (0..16)
        .map(|_| database.connection().unwrap())
        .collect::<Vec<_>>();
    let mut connection = held.pop().unwrap();
    let backend = connection
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get::<_, i32>(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    let within_deadline = std::thread::scope(|scope| {
        let resume = ResumeBackend::stop(backend);
        drop(connection);
        let checkout = scope.spawn(|| {
            let started = Instant::now();
            let result = database.connection();
            sender.send((started.elapsed(), result.is_err())).unwrap();
        });
        let observed = receiver.recv_timeout(Duration::from_secs(6));
        assert_eq!(unsafe { libc::kill(backend, libc::SIGCONT) }, 0);
        drop(resume);
        checkout.join().unwrap();
        observed
    });
    let (elapsed, rejected) = within_deadline.expect(
        "database checkout exceeded its five-second budget while validating a stalled backend",
    );
    assert!(rejected, "stalled connection must not be checked out");
    assert!(elapsed < Duration::from_secs(6));
}

#[test]
fn database_deadline_discards_an_uncommitted_transaction() {
    use std::time::{Duration, Instant};

    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    database
        .connection()
        .unwrap()
        .batch_execute("CREATE TABLE deadline_rows (id integer PRIMARY KEY)")
        .unwrap();
    let started = Instant::now();
    let mut connection = database.connection().unwrap();
    let backend: i32 = connection
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    let mut transaction = connection.transaction().unwrap();
    transaction
        .execute("INSERT INTO deadline_rows VALUES (1)", &[])
        .unwrap();
    let error = transaction.simple_query("SELECT pg_sleep(10)").unwrap_err();
    assert!(matches!(
        error,
        telchar::persistence::DatabaseError::Deadline
    ));
    drop(transaction);
    assert!(matches!(
        connection.simple_query("SELECT 1"),
        Err(telchar::persistence::DatabaseError::Closed)
    ));
    drop(connection);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "query timeout and rollback must share the checkout deadline"
    );

    let mut replacement = database.connection().unwrap();
    assert_ne!(
        replacement
            .query_one("SELECT pg_backend_pid()", &[])
            .unwrap()
            .get::<_, i32>(0),
        backend
    );
    assert_eq!(
        replacement
            .query_one("SELECT count(*) FROM deadline_rows", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    replacement
        .execute("INSERT INTO deadline_rows VALUES (2)", &[])
        .unwrap();
    assert_eq!(
        replacement
            .query_one("SELECT id FROM deadline_rows", &[])
            .unwrap()
            .get::<_, i32>(0),
        2
    );
}

#[cfg(target_os = "linux")]
#[test]
fn database_bounds_stalled_query_and_transaction_teardown() {
    use std::time::{Duration, Instant};
    use telchar::persistence::DatabaseError;

    let fixture = PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    database
        .connection()
        .unwrap()
        .batch_execute("CREATE TABLE stalled_rows (id integer PRIMARY KEY)")
        .unwrap();

    for operation in ["query", "rollback", "drop"] {
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (start_sender, start_receiver) = std::sync::mpsc::channel();
        let (done_sender, done_receiver) = std::sync::mpsc::channel();
        let (backend, observed) = std::thread::scope(|scope| {
            let database = &database;
            let worker = scope.spawn(move || {
                let started = Instant::now();
                let mut connection = database.connection().unwrap();
                let backend = connection
                    .query_one("SELECT pg_backend_pid()", &[])
                    .unwrap()
                    .get::<_, i32>(0);
                let mut transaction = connection.transaction().unwrap();
                transaction
                    .execute("INSERT INTO stalled_rows VALUES (1)", &[])
                    .unwrap();
                ready_sender.send(backend).unwrap();
                start_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                match operation {
                    "query" => {
                        assert!(matches!(
                            transaction.simple_query("SELECT 1"),
                            Err(DatabaseError::Deadline)
                        ));
                        drop(transaction);
                    }
                    "rollback" => assert!(matches!(
                        transaction.rollback(),
                        Err(DatabaseError::Deadline)
                    )),
                    "drop" => drop(transaction),
                    _ => unreachable!(),
                }
                assert!(matches!(
                    connection.simple_query("SELECT 1"),
                    Err(DatabaseError::Closed)
                ));
                drop(connection);
                done_sender.send(started.elapsed()).unwrap();
            });
            let backend = ready_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            let resume = ResumeBackend::stop(backend);
            start_sender.send(()).unwrap();
            let observed = done_receiver.recv_timeout(Duration::from_secs(6));
            drop(resume);
            worker.join().unwrap();
            (backend, observed)
        });
        assert!(
            observed.expect("stalled transaction exceeded checkout budget")
                < Duration::from_secs(6),
            "{operation} must share the checkout deadline"
        );
        let mut replacement = database.connection().unwrap();
        assert_ne!(
            replacement
                .query_one("SELECT pg_backend_pid()", &[])
                .unwrap()
                .get::<_, i32>(0),
            backend
        );
        assert_eq!(
            replacement
                .query_one("SELECT count(*) FROM stalled_rows", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}

#[cfg(unix)]
#[test]
fn database_commit_timeout_can_leave_a_durable_transaction() {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use telchar::persistence::DatabaseError;

    let fixture = PostgresFixture::start();
    let mut observer = fixture.connect();
    observer
        .batch_execute("CREATE TABLE commit_rows (id integer PRIMARY KEY)")
        .unwrap();
    let config = fixture.keyword_url().parse::<postgres::Config>().unwrap();
    let host = config
        .get_hosts()
        .iter()
        .find_map(|host| match host {
            postgres::config::Host::Unix(path) => Some(path),
            _ => None,
        })
        .expect("fixture must use a private Unix socket");
    let socket_name = format!(".s.PGSQL.{}", config.get_ports()[0]);
    let upstream = host.join(&socket_name);
    let root = tempfile::Builder::new()
        .prefix("commit-")
        .tempdir_in("/tmp")
        .unwrap();
    let listener = UnixListener::bind(root.path().join(socket_name)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let url =
        fixture
            .keyword_url()
            .replacen(host.to_str().unwrap(), root.path().to_str().unwrap(), 1);
    let hide_responses = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let relay = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            let (mut client, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "database did not connect");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("proxy accept failed: {error}"),
                }
            };
            let mut server = UnixStream::connect(upstream).unwrap();
            for stream in [&client, &server] {
                stream
                    .set_read_timeout(Some(Duration::from_millis(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
            }
            let mut bytes = [0; 65536];
            while Instant::now() < deadline {
                for toward_server in [true, false] {
                    let (source, destination) = if toward_server {
                        (&mut client, &mut server)
                    } else {
                        (&mut server, &mut client)
                    };
                    match source.read(&mut bytes) {
                        Ok(0) => return,
                        Ok(length) => {
                            if toward_server || !hide_responses.load(Ordering::SeqCst) {
                                destination.write_all(&bytes[..length]).unwrap();
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => panic!("proxy read failed: {error}"),
                    }
                }
            }
            panic!("database transport did not close after commit timeout");
        });
        let database = telchar::persistence::Database::connect(url.as_str()).unwrap();
        let started = Instant::now();
        let mut connection = database.connection().unwrap();
        let mut transaction = connection.transaction().unwrap();
        transaction
            .execute("INSERT INTO commit_rows VALUES (1)", &[])
            .unwrap();
        hide_responses.store(true, Ordering::SeqCst);
        let error = transaction.commit().unwrap_err();
        assert!(matches!(error, DatabaseError::Deadline));
        assert!(
            error
                .to_string()
                .contains("mutation outcome may be unknown")
        );
        assert!(matches!(
            connection.simple_query("SELECT 1"),
            Err(DatabaseError::Closed)
        ));
        drop(connection);
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(
            observer
                .query_one("SELECT count(*) FROM commit_rows", &[])
                .unwrap()
                .get::<_, i64>(0),
            1,
            "lost COMMIT response must not be mistaken for rollback"
        );
        relay.join().unwrap();
    });
}

#[test]
fn persistence_accepts_a_substituted_database_source() {
    let fixture = PostgresFixture::start();
    let database = TestDatabase {
        database: telchar::persistence::Database::connect(fixture.url())
            .expect("test database configures"),
    };

    let migration = telchar::persistence::migrate(&database).expect("migration succeeds");

    assert_eq!(
        migration.resulting_version,
        telchar::persistence::latest_migration_version()
    );
}
