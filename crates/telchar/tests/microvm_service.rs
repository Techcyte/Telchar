//! Composed listener and cleanup lifecycle uses one fenced owner.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn service_closes_admission_on_shutdown_and_ownership_loss() {
    for fenced in [false, true] {
        let fixture = telchar_test_support::postgres::PostgresFixture::start();
        let database = telchar::persistence::Database::connect(fixture.url()).unwrap();
        telchar::persistence::migrate(&database).unwrap();
        let owner = Arc::new(
            telchar::service::singleton_ownership::SingletonOwnership::acquire(
                fixture.url(),
                Duration::from_secs(30),
            )
            .unwrap(),
        );
        let config = telchar::service::config::MicrovmBackendConfig::parse(
            r#"
name = "arm"
system = "aarch64-linux"
account_id = "000000000001"
region = "us-west-2"
image_arn = "arn:aws:lambda:us-west-2:000000000001:microvm-image:builder"
image_version = "1"
maximum_concurrent_builds = 1
maximum_duration_seconds = 60
execution_timeout_seconds = 50
admission_timeout_seconds = 2
provider_timeout_seconds = 1
cleanup_interval_seconds = 1
callback_url = "wss://gateway.example/microvm"
callback_address = "192.0.2.1:443"
staging_directory = "/unused"
maximum_paths = 10
maximum_nar_bytes = 1024
maximum_total_bytes = 2048
metadata_bytes = 1024
chunk_bytes = 1024
"#,
        )
        .unwrap();
        let provider = telchar::microvm::Provider::new(
            aws_sdk_lambdamicrovms::Config::builder()
                .behavior_version_latest()
                .region(aws_sdk_lambdamicrovms::config::Region::new("us-west-2"))
                .credentials_provider(aws_credential_types::Credentials::new(
                    "fixture-key",
                    "fixture-secret",
                    None,
                    None,
                    "test",
                ))
                .endpoint_url("http://127.0.0.1:1")
                .build(),
            Duration::from_secs(1),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let service = Arc::new(
            telchar::microvm::service::Service::start(
                config,
                provider,
                owner,
                listener,
                telchar::store::daemon::GatewayStoreEndpoint::parse("unix:///unused").unwrap(),
            )
            .unwrap(),
        );
        service.check().unwrap();
        if fenced {
            fixture.expire_singleton_ownership("daemon");
            let deadline = Instant::now() + Duration::from_secs(2);
            while service.check().is_ok() {
                assert!(Instant::now() < deadline, "service must fail closed");
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = service.shutdown();
        } else {
            use tungstenite::client::IntoClientRequest;
            let mut request = format!("ws://{address}/microvm")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "sec-websocket-protocol",
                "telchar-microvm-v1".parse().unwrap(),
            );
            let (peer, _) =
                tungstenite::client(request, TcpStream::connect(address).unwrap()).unwrap();
            let stopping = Arc::clone(&service);
            let shutdown = std::thread::spawn(move || stopping.shutdown());
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                let started = Instant::now();
                let health = service.check();
                assert!(
                    started.elapsed() < Duration::from_millis(200),
                    "health must not wait for admission drain"
                );
                if health.is_err() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "shutdown must close execution admission promptly"
                );
                std::thread::yield_now();
            }
            assert!(
                !shutdown.is_finished(),
                "silent admitted upgrade still draining"
            );
            drop(peer);
            shutdown.join().unwrap().unwrap();
            service.shutdown().unwrap();
        }
        assert!(service.check().is_err());
        assert!(TcpStream::connect(address).is_err());
    }
}
