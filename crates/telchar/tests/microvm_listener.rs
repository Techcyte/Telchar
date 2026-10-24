//! Listener shutdown drains bounded silent bootstrap connections.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;

#[test]
fn rejected_bootstrap_exports_bounded_admission_metrics() {
    const CHILD: &str = "TELCHAR_TEST_ADMISSION_METRICS";
    if std::env::var_os(CHILD).is_none() {
        let collector = collector::start_collector();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rejected_bootstrap_exports_bounded_admission_metrics",
            ])
            .env(CHILD, "1")
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        collector.assert_counter("telchar.microvm.admissions", "rejected", 2);
        collector.assert_counter("telchar.microvm.admission.rejections", "protocol", 1);
        collector.assert_counter("telchar.microvm.admission.rejections", "bootstrap", 1);
        assert!(
            collector
                .metric_names()
                .contains("telchar.microvm.admission.duration")
        );
        collector.assert_metric_attributes_are_bounded();
        return;
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
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
    let mut client = TcpStream::connect(address).unwrap();
    use std::io::Write;
    client.write_all(b"invalid bootstrap\r\n\r\n").unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut response = Vec::new();
    match client.read_to_end(&mut response) {
        Ok(_) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bootstrap = loop {
        use tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://{address}/microvm")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            "telchar-microvm-v1".parse().unwrap(),
        );
        match tungstenite::connect(request) {
            Ok(connection) => break connection.0,
            Err(_) => {
                assert!(Instant::now() < deadline, "admission slot not released");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    bootstrap
        .send(tungstenite::Message::Binary(
            b"invalid bootstrap".to_vec().into(),
        ))
        .unwrap();
    assert!(bootstrap.read().is_err());
    service.shutdown().unwrap();
    telemetry.shutdown();
}

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
