//! Listener shutdown drains bounded silent bootstrap connections.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn ownership_loss_closes_idle_listener_and_remains_unhealthy() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let sessions = Arc::new(telchar::microvm::rendezvous::ExecutionSessions::new(1).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut service = telchar::microvm::callback_service::CallbackService::start(
        listener,
        owner,
        sessions,
        1,
        1,
        Duration::from_millis(200),
    )
    .unwrap();
    service.check().unwrap();
    fixture.expire_singleton_ownership("daemon");
    let until = Instant::now() + Duration::from_secs(2);
    while service.check().is_ok() {
        assert!(
            Instant::now() < until,
            "idle listener must detect ownership loss"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(TcpStream::connect(address).is_err());
    assert!(service.check().is_err(), "health failure persists");
    assert!(
        service.shutdown().is_err(),
        "shutdown reports listener failure"
    );
    assert!(service.check().is_err());
}

#[test]
fn shutdown_drains_silent_bootstrap_and_closes_listener() {
    let fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let sessions = Arc::new(telchar::microvm::rendezvous::ExecutionSessions::new(1).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut service = telchar::microvm::callback_service::CallbackService::start(
        listener,
        owner,
        sessions,
        1,
        1,
        Duration::from_millis(200),
    )
    .unwrap();
    let mut silent = TcpStream::connect(address).unwrap();
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let started = Instant::now();
    service.shutdown().unwrap();
    service.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    let result = silent.read(&mut [0]);
    assert!(
        matches!(result, Ok(0))
            || matches!(result, Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset)
    );
    assert!(TcpStream::connect(address).is_err());
}
