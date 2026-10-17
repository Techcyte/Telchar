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
        let deployment = r#"
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
"#;
        let config = telchar::service::config::MicrovmBackendConfig::parse(deployment).unwrap();
        let second = telchar::service::config::MicrovmBackendConfig::parse(
            &deployment.replace("name = \"arm\"", "name = \"arm-secondary\""),
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
        let services = telchar::microvm::service::Service::start_many(
            vec![(config, provider.clone()), (second, provider)],
            owner,
            listener,
            telchar::store::daemon::GatewayStoreEndpoint::parse("unix:///unused").unwrap(),
        )
        .unwrap();
        let service = Arc::clone(&services[0]);
        service.check().unwrap();
        use telchar::backend::BuildBackend;
        let configuration_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            configuration_file.path(),
            format!("[backends]\npermit_wait_seconds = 1\nmicrovm_bind = \"127.0.0.1:7444\"\n[[backends.microvm]]\n{deployment}\n[[backends.microvm]]\n{}", deployment.replace("name = \"arm\"", "name = \"arm-secondary\"")),
        )
        .unwrap();
        let configuration =
            telchar::service::config::ServiceConfig::load_from_default(configuration_file.path())
                .unwrap();
        let backends =
            telchar::backend::routing::ConfiguredBackends::new(&configuration, None).unwrap();
        assert!(
            backends
                .executor(
                    database.clone(),
                    Arc::new(telchar::shared_build::SharedBuildRegistry::new())
                )
                .unwrap()
                .selected_target("aarch64-linux", &[])
                .is_err()
        );
        let backends = backends
            .attach_microvm_services(&configuration, services.clone())
            .unwrap();
        let executor = backends
            .executor(
                database.clone(),
                Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
            )
            .unwrap();
        let target = executor.selected_target("aarch64-linux", &[]).unwrap();
        assert_eq!(target.name(), "arm");
        assert_eq!(target.kind(), telchar::backend::BackendKind::Microvm);
        assert!(executor.selected_target("aarch64-linux", &["kvm"]).is_err());
        let permit = executor
            .reserve_target("aarch64-linux", &[])
            .unwrap()
            .unwrap();
        assert_eq!(permit.target(), &target);
        let second_permit = executor
            .reserve_target("aarch64-linux", &[])
            .unwrap()
            .unwrap();
        let reloadable = telchar::backend::routing::ReloadableBackends::new(backends.clone());
        let mut current = configuration.clone();
        let mut health = telchar::service::daemon_services::StaticSshHealthService::start(
            backends.static_ssh_health(),
            Duration::from_secs(1),
        )
        .unwrap();
        telchar::service::config_reload::BackendReload::prepare_config(
            &current,
            configuration.clone(),
            None,
            None,
            Duration::from_secs(1),
        )
        .unwrap()
        .apply(&mut current, &reloadable, &mut health)
        .unwrap();
        let reloaded = reloadable
            .snapshot()
            .executor(
                database.clone(),
                Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
            )
            .unwrap();
        assert_eq!(
            reloaded.selected_target("aarch64-linux", &[]).unwrap(),
            target
        );
        assert_eq!(
            reloaded
                .reserve_target("aarch64-linux", &[])
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        drop(permit);
        let replacement_permit = reloaded
            .reserve_target("aarch64-linux", &[])
            .unwrap()
            .unwrap();
        assert_eq!(replacement_permit.target(), &target);
        drop(replacement_permit);
        drop(second_permit);
        health.shutdown().unwrap();
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
        assert!(services[1].check().is_err());
        services[1].shutdown().ok();
        assert!(executor.selected_target("aarch64-linux", &[]).is_err());
        assert!(reloaded.selected_target("aarch64-linux", &[]).is_err());
        assert!(TcpStream::connect(address).is_err());
    }
}
