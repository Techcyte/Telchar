use super::*;

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
