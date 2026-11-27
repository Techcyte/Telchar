//! Database uncertainty diagnostics must distinguish deadlines without leaking SQL.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn ordinary_fenced_transaction_allows_renewal_on_healthy_database() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    let generation = owner.generation();
    let mut connection = owner.database().connection().unwrap();
    let mut transaction = connection.transaction().unwrap();
    // Even a no-op ordinary durable write invokes the statement-level fencing trigger.
    transaction
        .execute(
            "UPDATE protocol_sessions SET state = state WHERE false",
            &[],
        )
        .unwrap();
    assert_eq!(
        fixture
            .connect()
            .query_one("SELECT 42::integer", &[])
            .unwrap()
            .get::<_, i32>(0),
        42
    );
    owner
        .renew()
        .expect("ordinary fenced write must not block renewal");
    assert!(owner.admission_available().unwrap());
    transaction.rollback().unwrap();
    owner.renew().unwrap();
    assert!(owner.admission_available().unwrap());
    assert_eq!(owner.generation(), generation);
}

#[test]
fn key_share_alone_does_not_protect_generation_from_takeover() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    fixture
        .connect()
        .batch_execute("DROP INDEX IF EXISTS singleton_ownership_identity")
        .unwrap();
    let mut blocker = fixture.connect();
    let mut held = blocker.transaction().unwrap();
    held.query_one(
        "SELECT generation FROM singleton_ownership WHERE owner_kind = 'daemon' FOR KEY SHARE",
        &[],
    )
    .unwrap();
    let mut replacement = fixture.connect();
    replacement
        .batch_execute("SET statement_timeout = '500ms'")
        .unwrap();
    let changed = replacement.execute("UPDATE singleton_ownership SET generation = generation + 1, owner_token = 'replacement' WHERE owner_kind = 'daemon'", &[]).unwrap();
    assert_eq!(
        changed, 1,
        "KEY SHARE alone does not conflict with non-key identity updates"
    );
    held.rollback().unwrap();
    assert!(
        owner.verify().is_err(),
        "old owner must still detect changed generation"
    );
}

#[test]
fn key_share_with_identity_index_blocks_takeover_but_allows_renewal() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    let mut blocker = owner.database().connection().unwrap();
    let mut held = blocker.transaction().unwrap();
    held.execute(
        "UPDATE protocol_sessions SET state = state WHERE false",
        &[],
    )
    .unwrap();
    owner
        .renew()
        .expect("timestamp-only renewal is compatible with key-share lock");
    let mut replacement = fixture.connect();
    replacement
        .batch_execute("SET statement_timeout = '300ms'")
        .unwrap();
    let error = replacement.execute("UPDATE singleton_ownership SET generation = generation + 1, owner_token = 'replacement' WHERE owner_kind = 'daemon'", &[]).unwrap_err();
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::QUERY_CANCELED)
    );
    let error = replacement
        .execute(
            "DELETE FROM singleton_ownership WHERE owner_kind = 'daemon'",
            &[],
        )
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::QUERY_CANCELED)
    );
    held.rollback().unwrap();
    owner.verify().unwrap();
}

#[test]
fn expired_owner_takeover_waits_for_fenced_transaction_then_fences_old_owner() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = telchar::service::singleton_ownership::SingletonOwnership::acquire(
        fixture.url(),
        Duration::from_secs(10),
    )
    .unwrap();
    let generation = owner.generation();
    let mut connection = owner.database().connection().unwrap();
    let mut held = connection.transaction().unwrap();
    held.execute(
        "UPDATE protocol_sessions SET state = state WHERE false",
        &[],
    )
    .unwrap();
    fixture.expire_singleton_ownership("daemon");
    let url = fixture.url().to_owned();
    let (send, recv) = std::sync::mpsc::channel();
    let takeover = std::thread::spawn(move || {
        let next = telchar::service::singleton_ownership::SingletonOwnership::acquire(
            &url,
            Duration::from_secs(10),
        )
        .unwrap();
        send.send(next).unwrap();
    });
    assert!(
        matches!(
            recv.recv_timeout(Duration::from_millis(300)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "takeover crossed an in-flight fenced write"
    );
    held.rollback().unwrap();
    let replacement = recv.recv_timeout(Duration::from_secs(5)).unwrap();
    takeover.join().unwrap();
    assert!(replacement.generation() > generation);
    assert!(owner.renew().is_err());
    let error = connection
        .execute(
            "UPDATE protocol_sessions SET state = state WHERE false",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.code().map(|code| code.code()), Some("55000"));
    replacement.verify().unwrap();
}

#[test]
fn deadline_reports_phase_without_sql_or_credentials() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
    let bounded = database.for_request(
        Instant::now() + Duration::from_secs(2),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let mut connection = bounded.connection().unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let error = connection
            .simple_query("SELECT pg_sleep(5) /* secret-query-marker */")
            .unwrap_err();
        assert!(matches!(
            error,
            telchar::persistence::DatabaseError::Deadline
        ));
    });
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "database.connection.operation_failed",
        "cause=\"deadline\"",
        "operation=\"simple_query\"",
    ] {
        assert!(logs.contains(field), "missing {field}: {logs}");
    }
    assert!(!logs.contains("secret-query-marker"));
    assert!(!logs.contains(fixture.url()));
    assert_eq!(
        database
            .connection()
            .unwrap()
            .query_one("SELECT 42::integer", &[])
            .unwrap()
            .get::<_, i32>(0),
        42
    );
}
