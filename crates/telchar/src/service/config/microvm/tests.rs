use super::*;

#[test]
fn idle_retention_is_bounded_by_worker_lifetime() {
    let raw = include_str!("../../../../tests/fixtures/microvm.toml");
    assert_eq!(
        MicrovmBackendConfig::parse(raw).unwrap().idle_retention(),
        Duration::ZERO
    );
    let mut deployment: Deployment = toml::from_str(raw).unwrap();
    deployment.idle_retention_seconds = 30;
    assert_eq!(
        MicrovmBackendConfig::validate(deployment.clone())
            .unwrap()
            .idle_retention(),
        Duration::from_secs(30)
    );
    deployment.idle_retention_seconds = deployment.maximum_duration_seconds + 1;
    assert!(MicrovmBackendConfig::validate(deployment).is_err());
}

#[test]
fn worker_job_limit_rejects_unbounded_configuration() {
    let raw = include_str!("../../../../tests/fixtures/microvm.toml");
    assert_eq!(
        MicrovmBackendConfig::parse(raw)
            .unwrap()
            .maximum_jobs_per_worker(),
        100
    );
    for limit in [0, 1_000_001] {
        let mut deployment: Deployment = toml::from_str(raw).unwrap();
        deployment.maximum_jobs_per_worker = limit;
        assert!(MicrovmBackendConfig::validate(deployment).is_err());
    }
}

#[test]
fn launches_resolve_current_transport_without_changing_tls_identity() {
    let config =
        MicrovmBackendConfig::parse(include_str!("../../../../tests/fixtures/microvm.toml"))
            .unwrap();
    let fingerprint = config.fingerprint();
    let mut launched = Vec::new();
    for (attempt, address) in [(1, "192.0.2.1:443"), (2, "192.0.2.2:443")] {
        let (intent, payload) = config
            .launch_with_resolver(attempt, |host, port| {
                assert_eq!(host, "internal-builder.example");
                assert_eq!(port, 443);
                Ok(address.parse().unwrap())
            })
            .unwrap();
        let hook: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(hook["endpoint"], "wss://gateway.example/microvm");
        assert_eq!(hook["address"], address);
        assert_eq!(intent.configuration_digest, fingerprint);
        assert_eq!(
            intent.request_digest,
            crate::microvm::launch_request_digest(
                &intent.image_arn,
                &intent.image_version,
                &intent.vpc_egress_connector_arn,
                &intent.client_token,
                &payload,
                intent.maximum_duration_seconds,
            )
        );
        launched.push(payload);
    }
    let first: serde_json::Value = serde_json::from_str(&launched[0]).unwrap();
    assert_eq!(first["address"], "192.0.2.1:443");
    assert!(
        config
            .launch_with_resolver(3, |_, _| Err(io::Error::other("DNS unavailable")))
            .is_err()
    );
}
