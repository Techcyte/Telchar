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
        collector.assert_counter("telchar.microvm.admissions", "rejected", 3);
        collector.assert_counter("telchar.microvm.admission.rejections", "protocol", 1);
        collector.assert_counter("telchar.microvm.admission.rejections", "bootstrap", 1);
        collector.assert_counter("telchar.microvm.admission.rejections", "authentication", 1);
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
    let connect = || {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            use tungstenite::client::IntoClientRequest;
            let mut request = format!("ws://{address}/microvm")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().unwrap(),
            );
            match tungstenite::connect(request) {
                Ok((mut socket, _)) => {
                    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                    }
                    break socket;
                }
                Err(_) => {
                    assert!(Instant::now() < deadline, "admission slot not released");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    };
    let mut bootstrap = connect();
    bootstrap
        .send(tungstenite::Message::Binary(
            b"invalid bootstrap".to_vec().into(),
        ))
        .unwrap();
    assert!(bootstrap.read().is_err());
    let mut authentication = connect();
    let capability = telchar::microvm::Capability::generate().unwrap();
    let message = serde_json::to_vec(&serde_json::json!({
        "intent_id": "unregistered-intent",
        "provider_id": "unregistered-provider",
        "bearer": capability.bearer(),
    }))
    .unwrap();
    authentication
        .send(tungstenite::Message::Binary(message.into()))
        .unwrap();
    assert!(authentication.read().is_err());
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

#[test]
fn listener_and_background_observers_pause_on_outage_and_resume_same_owner() {
    let mut fixture = telchar_test_support::postgres::PostgresFixture::start();
    telchar::persistence::migrate(fixture.url()).unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            fixture.url(),
            Duration::from_secs(10),
        )
        .unwrap(),
    );
    let renewal = owner.with_renewal(Duration::from_millis(100)).unwrap();
    let sessions = Arc::new(telchar::microvm::rendezvous::ExecutionSessions::new(1).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut service = telchar::microvm::callback_service::CallbackService::start(
        listener,
        Arc::clone(&owner),
        sessions,
        1,
        1,
        Duration::from_millis(200),
    )
    .unwrap();
    let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&passes);
    let mut maintenance =
        telchar::service::daemon_services::MaintenanceService::start_with_ownership(
            Duration::from_millis(50),
            Arc::clone(&owner),
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
    let mut recovery =
        telchar::service::daemon_services::RecoveryMonitorService::start_with_ownership(
            Duration::from_millis(50),
            Arc::clone(&owner),
            || Ok(true),
        )
        .unwrap();
    fixture.stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while owner.admission_available().unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    let before = passes.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(before, passes.load(std::sync::atomic::Ordering::SeqCst));
    service.check().unwrap();
    maintenance.check().unwrap();
    assert!(recovery.check().unwrap());
    fixture.restart();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !owner.admission_available().unwrap()
        || passes.load(std::sync::atomic::Ordering::SeqCst) == before
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::io::Write;
    let mut peer = TcpStream::connect(address).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.write_all(b"invalid bootstrap\r\n\r\n").unwrap();
    let mut response = [0; 512];
    assert!(
        peer.read(&mut response).is_ok(),
        "listener failed to resume"
    );
    service.check().unwrap();
    maintenance.shutdown().unwrap();
    recovery.shutdown().unwrap();
    service.shutdown().unwrap();
    drop(renewal);
}
